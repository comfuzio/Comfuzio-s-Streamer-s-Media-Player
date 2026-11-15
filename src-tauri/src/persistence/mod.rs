//! SQLite-backed persistence: settings, blocklist, queue state, play history
//! and a small key-value store for credentials.

pub mod blocklist;
pub mod db;
pub mod kv;
pub mod queue;
pub mod settings;

pub use db::Database;
