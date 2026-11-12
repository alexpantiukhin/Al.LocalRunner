//! SQLite stores for the three databases of spec 14: [`SqliteBrokerStore`] (`broker.db` and
//! `logs/`), [`SqliteHubStore`] (`hub.db`) and [`SqliteRunnerStore`] (`runner.db`), each
//! implementing the matching trait of `lr-abstractions` and passing its contract suite.
//!
//! Threading: each store owns one `rusqlite::Connection` behind a mutex, and every call runs
//! on `tokio::task::spawn_blocking`, so SQLite and file system work never occupies a runtime
//! worker. Calls of one store are serialised; each write is one immediate transaction.
//!
//! Persistence: WAL journal, `foreign_keys = ON`, `application_id` marking the kind of
//! database and `user_version` its schema version (see [`schema`]): a newer version than this
//! build knows is refused, an older one is migrated step by step.

mod broker;
mod codec;
mod db;
#[cfg(test)]
mod db_tests;
mod hub;
mod job_read;
mod job_write;
mod logs;
mod outbox;
mod prune;
mod registry;
mod runner;
pub mod schema;
mod seq;

pub use broker::SqliteBrokerStore;
pub use hub::SqliteHubStore;
pub use runner::SqliteRunnerStore;
