//! Migrations: describe a database, diff it with the schema, write and apply SQL.
//!
//! Both sides of every comparison are a [`Shape`]: [`calculate`] builds one from a checked schema,
//! [`postgres::describe`] reads one from a live database.

mod calculate;
pub mod check;
pub mod commands;
pub mod diff;
pub mod history;
pub mod mongo;
pub mod postgres;
pub mod shape;

pub use calculate::{calculate, renames};
pub use diff::{Renames, Step, diff};
pub use shape::Shape;

/// The version of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A migration command failed; the message says why and what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateError {
    /// What went wrong, in plain words.
    pub message: String,
}

impl MigrateError {
    /// An error with `message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MigrateError {}

impl From<sqlx::Error> for MigrateError {
    fn from(e: sqlx::Error) -> Self {
        Self::new(format!("database error: {e}"))
    }
}
