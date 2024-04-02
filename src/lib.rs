//! # tower-sessions-rorm-store
//!
//! Implementation of [SessionStore] provided by [tower_sessions] for [rorm].
//!
//! In order to provide the possibility to use a user-defined [Model], this crate
//! defines [SessionModel] which must be implemented to create a [RormStore].
//!
//! Look at our example crate for the usage.

#![warn(missing_docs)]

use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::marker::PhantomData;
use std::str::FromStr;

use async_trait::async_trait;
use rorm::and;
use rorm::delete;
use rorm::fields::types::Json;
use rorm::insert;
use rorm::internal::field::Field;
use rorm::internal::field::FieldProxy;
use rorm::query;
use rorm::update;
use rorm::Database;
use rorm::FieldAccess;
use rorm::Model;
use rorm::Patch;
pub use serde_json::Value;
use thiserror::Error;
use tower_sessions::cookie::time::OffsetDateTime;
use tower_sessions::session::Id;
use tower_sessions::session::Record;
use tower_sessions::session_store::Error;
use tower_sessions::session_store::Result;
use tower_sessions::ExpiredDeletion;
use tower_sessions::SessionStore;
use tracing::debug;
use tracing::instrument;

/// Implement this trait on a [Model] that should be used
/// to store sessions in a database.
///
/// Ths [Model] must not define relations that make it impossible to
/// delete it using the [FieldProxy] retrieved by [SessionModel::get_expires_at_field].
pub trait SessionModel
where
    Self: Model + Send + Sync + 'static,
    Self::Primary: Field<Type = String>,
{
    /// Retrieve the primary field from the Model
    fn get_primary_field() -> FieldProxy<Self::Primary, Self> {
        FieldProxy::new()
    }

    /// Retrieve the expires_at field from the Model
    fn get_expires_at_field() -> FieldProxy<impl Field<Type = OffsetDateTime, Model = Self>, Self>;

    /// Retrieve the data field of the Model
    fn get_data_field(
    ) -> FieldProxy<impl Field<Type = Json<HashMap<String, Value>>, Model = Self>, Self>;

    /// Retrieve an insert patch that should use the parameters
    /// provided for construction
    fn get_insert_patch(
        id: String,
        expires_at: OffsetDateTime,
        data: Json<HashMap<String, Value>>,
    ) -> impl Patch<Model = Self> + Send + Sync + 'static;

    /// Get the session data from an instance of the session [Model]
    fn get_session_data(&self) -> (String, OffsetDateTime, Json<HashMap<String, Value>>);
}

/// The session store for rorm
pub struct RormStore<S> {
    db: Database,
    marker: PhantomData<S>,
}

impl<S> RormStore<S> {
    /// Construct a new Store
    pub fn new(db: Database) -> Self {
        Self {
            db,
            marker: PhantomData,
        }
    }
}

impl<S> Debug for RormStore<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "")
    }
}

impl<S> Clone for RormStore<S> {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            marker: PhantomData,
        }
    }
}

#[async_trait]
impl<S> ExpiredDeletion for RormStore<S>
where
    S: Model + SessionModel + Debug,
    <S as Model>::Primary: Field<Type = String>,
    <S as Patch>::Decoder: Send + Sync + 'static,
{
    #[instrument(level = "trace")]
    async fn delete_expired(&self) -> Result<()> {
        let db = &self.db;

        delete!(db, S)
            .condition(S::get_expires_at_field().less_than(OffsetDateTime::now_utc()))
            .await
            .map_err(RormStoreError::from)?;

        Ok(())
    }
}

#[async_trait]
impl<S> SessionStore for RormStore<S>
where
    S: Model + Send + Sync + SessionModel,
    <S as Model>::Primary: Field<Type = String>,
    <S as Patch>::Decoder: Send + Sync + 'static,
{
    #[instrument(level = "trace")]
    async fn create(&self, session_record: &mut Record) -> Result<()> {
        debug!("Creating new session");
        let mut tx = self
            .db
            .start_transaction()
            .await
            .map_err(RormStoreError::from)?;
        loop {
            let existing = query!(&mut tx, S)
                .condition(S::get_primary_field().equals(session_record.id.to_string()))
                .optional()
                .await
                .map_err(RormStoreError::from)?;

            if existing.is_none() {
                insert!(&mut tx, S)
                    .return_nothing()
                    .single(&S::get_insert_patch(
                        session_record.id.to_string(),
                        session_record.expiry_date,
                        Json(session_record.data.clone()),
                    ))
                    .await
                    .map_err(RormStoreError::from)?;

                break;
            }

            session_record.id = Id::default();
        }

        tx.commit().await.map_err(RormStoreError::from)?;

        Ok(())
    }

    #[instrument(level = "trace")]
    async fn save(&self, session_record: &Record) -> Result<()> {
        let Record {
            id,
            data,
            expiry_date,
        } = session_record;

        let mut tx = self
            .db
            .start_transaction()
            .await
            .map_err(RormStoreError::from)?;

        let existing_session = query!(&mut tx, S)
            .condition(S::get_primary_field().equals(id.to_string()))
            .optional()
            .await
            .map_err(RormStoreError::from)?;

        if existing_session.is_some() {
            update!(&mut tx, S)
                .condition(S::get_primary_field().equals(id.to_string()))
                .set(S::get_expires_at_field(), *expiry_date)
                .set(S::get_data_field(), Json(data.clone()))
                .exec()
                .await
                .map_err(RormStoreError::from)?;
        } else {
            insert!(&mut tx, S)
                .single(&S::get_insert_patch(
                    id.to_string(),
                    *expiry_date,
                    Json(data.clone()),
                ))
                .await
                .map_err(RormStoreError::from)?;
        }

        tx.commit().await.map_err(RormStoreError::from)?;

        Ok(())
    }

    #[instrument(level = "trace")]
    async fn load(&self, session_id: &Id) -> Result<Option<Record>> {
        debug!("Loading session");
        let db = &self.db;

        let session = query!(db, S)
            .condition(and!(
                S::get_primary_field().equals(session_id.to_string()),
                S::get_expires_at_field().greater_than(OffsetDateTime::now_utc())
            ))
            .optional()
            .await
            .map_err(RormStoreError::from)?;

        Ok(match session {
            None => None,
            Some(session) => {
                let (id, expiry, data) = session.get_session_data();

                Some(Record {
                    id: Id::from_str(id.as_str()).map_err(RormStoreError::from)?,
                    data: data.into_inner(),
                    expiry_date: expiry,
                })
            }
        })
    }

    #[instrument(level = "trace")]
    async fn delete(&self, session_id: &Id) -> Result<()> {
        let db = &self.db;

        delete!(db, S)
            .condition(S::get_primary_field().equals(session_id.to_string()))
            .await
            .map_err(RormStoreError::from)?;

        Ok(())
    }
}

/// Error type that is used in the [SessionStore] trait
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum RormStoreError {
    #[error("Database error: {0}")]
    Database(#[from] rorm::Error),
    #[error("Decoding of id failed: {0}")]
    DecodingFailed(#[from] base64::DecodeSliceError),
}

impl From<RormStoreError> for Error {
    fn from(value: RormStoreError) -> Self {
        Self::Backend(value.to_string())
    }
}
