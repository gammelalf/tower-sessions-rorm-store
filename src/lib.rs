use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::marker::PhantomData;
use std::str::FromStr;

use async_trait::async_trait;
use rorm::and;
use rorm::delete;
use rorm::fields::traits::FieldEq;
use rorm::fields::traits::FieldOrd;
use rorm::fields::types::Json;
use rorm::insert;
use rorm::internal::field::Field;
use rorm::internal::field::FieldProxy;
use rorm::internal::relation_path::Path;
use rorm::query;
use rorm::update;
use rorm::Database;
use rorm::FieldAccess;
use rorm::Model;
use rorm::Patch;
pub use serde_json::Value;
use tower_sessions::cookie::time::OffsetDateTime;
use tower_sessions::session::Id;
use tower_sessions::session::Record;
use tower_sessions::session_store::Error;
use tower_sessions::session_store::Result;
use tower_sessions::ExpiredDeletion;
use tower_sessions::SessionStore;

pub trait SessionModel
where
    Self: Model + Send + Sync + 'static,
{
    fn get_primary_field<PRIM>() -> FieldProxy<PRIM, Self>
    where
        PRIM: Field<Type = String, Model = Self> + FieldEq<'static, String>;

    fn get_expiry_field<EXPIRY>() -> FieldProxy<EXPIRY, Self>
    where
        EXPIRY: Field<Type = OffsetDateTime, Model = Self> + FieldOrd<'static, OffsetDateTime>;

    fn get_data_field<DATA>() -> FieldProxy<DATA, Self>
    where
        DATA: Field<Type = Json<HashMap<String, Value>>, Model = Self>;

    fn get_insert_patch<P>(
        id: i128,
        expiry: OffsetDateTime,
        data: Json<HashMap<String, Value>>,
    ) -> P
    where
        P: Patch<Model = Self>;

    fn get_session_data(&self) -> (String, OffsetDateTime, Json<HashMap<String, Value>>);
}

/// The session store for rorm
pub struct RormStore<S> {
    db: Database,
    marker: PhantomData<S>,
}

impl<S> RormStore<S> {
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

#[async_trait]
impl<S> ExpiredDeletion for RormStore<S>
where
    S: Model + SessionModel + Debug,
    <S as Model>::Primary: Field<Type = String>,
{
    async fn delete_expired(&self) -> Result<()> {
        let db = &self.db;

        delete!(db, S)
            .condition(FieldAccess::less_than(
                S::get_expiry_field(),
                OffsetDateTime::now_utc(),
            ))
            .await
            .map_err(|e| tower_sessions::session_store::Error::Backend(e.to_string()))?;

        Ok(())
    }
}

#[async_trait]
impl<S> SessionStore for RormStore<S>
where
    S: Model + Send + Sync + Debug + SessionModel,
    <S as Model>::Primary: Field<Type = String>,
{
    async fn create(&self, session_record: &mut Record) -> Result<()> {
        let mut tx = self
            .db
            .start_transaction()
            .await
            .map_err(|e| Error::Backend(e.to_string()))?;
        loop {
            let existing = query!(&mut tx, S)
                .condition::<<String as FieldEq<'static, String, ()>>::EqCond<FieldProxy<_, S>>>(
                    FieldEq::<'static, String, ()>::field_equals(
                        S::get_primary_field(),
                        session_record.id.0.to_string(),
                    ),
                )
                .optional()
                .await
                .map_err(|e| Error::Backend(e.to_string()))?;

            if existing.is_none() {
                insert!(&mut tx, S)
                    .return_nothing()
                    .single(&S::get_insert_patch(
                        session_record.id.0,
                        session_record.expiry_date,
                        Json(session_record.data.clone()),
                    ))
                    .await
                    .map_err(|e| Error::Backend(e.to_string()))?;

                break;
            }

            session_record.id = Id::default();
        }

        tx.commit()
            .await
            .map_err(|e| Error::Backend(e.to_string()))?;

        Ok(())
    }

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
            .map_err(|e| Error::Backend(e.to_string()))?;

        let existing_session = query!(&mut tx, S)
            .condition(S::get_primary_field().equals(id.0.to_string()))
            .optional()
            .await
            .map_err(|e| Error::Backend(e.to_string()))?;

        if existing_session.is_some() {
            update!(&mut tx, S)
                .condition(S::get_primary_field().equals(id.0.to_string()))
                .set(S::get_expiry_field(), *expiry_date)
                .set(S::get_data_field(), Json(data.clone()))
                .exec()
                .await
                .map_err(|e| Error::Backend(e.to_string()))?;
        } else {
            insert!(&mut tx, S)
                .single(&S::get_insert_patch(id.0, *expiry_date, Json(data.clone())))
                .await
                .map_err(|e| Error::Backend(e.to_string()))?;
        }

        tx.commit()
            .await
            .map_err(|e| Error::Backend(e.to_string()))?;

        Ok(())
    }

    async fn load(&self, session_id: &Id) -> Result<Option<Record>> {
        let db = &self.db;

        let session = query!(db, S)
            .condition(and!(
                S::get_primary_field().equals(session_id.0.to_string()),
                S::get_expiry_field().greater_than(OffsetDateTime::now_utc())
            ))
            .optional()
            .await
            .map_err(|e| Error::Backend(e.to_string()))?;

        Ok(match session {
            None => None,
            Some(session) => {
                let (id, expiry, data) = session.get_session_data();
                Some(Record {
                    id: Id::from_str(id.as_str()).map_err(|e| Error::Backend(e.to_string()))?,
                    data: data.into_inner(),
                    expiry_date: expiry,
                })
            }
        })
    }

    async fn delete(&self, session_id: &Id) -> Result<()> {
        let db = &self.db;

        delete!(db, S)
            .condition(S::get_primary_field().equals(session_id.0.to_string()))
            .await
            .map_err(|e| Error::Backend(e.to_string()))?;

        Ok(())
    }
}
