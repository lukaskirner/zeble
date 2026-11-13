//! Zeble's non-UI core: saved connections, Postgres access and catalog
//! introspection. Everything here is testable without a window.

pub mod config;
pub mod db;
pub mod introspect;
pub mod query;
pub mod split;
pub mod sql;
pub mod store;

pub use config::{ColorTag, ConnectionConfig, SslMode};
pub use db::{Canceller, Db, QuerySession, is_connection_lost, pooled_canceller, runtime};
