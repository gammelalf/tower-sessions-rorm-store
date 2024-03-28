# tower-sessions-rorm-store

[![LICENSE](https://img.shields.io/github/license/rorm-orm/tower-sessions-rorm-store?color=blue)](LICENSE)
[![dependency status](https://deps.rs/repo/github/rorm-orm/tower-sessions-rorm-store/status.svg)](https://deps.rs/repo/github/rorm-orm/tower-sessions-rorm-store)
[![Crates.io](https://img.shields.io/crates/v/tower-sessions-rorm-store?label=Crates.io)](https://crates.io/crates/tower-sessions-rorm-store)
[![Docs](https://img.shields.io/docsrs/tower-sessions-rorm-store?label=Docs)](https://docs.rs/tower-sessions-rorm-store/latest)

Implementation of `SessionStore` provided by `tower_sessions` for `rorm`.

In order to provide the possibility to use a user-defined `Model`, this crate
defines `SessionModel` which must be implemented to create a `RormStore`.

Look at our example crate for the usage.
