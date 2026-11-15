//! Migrations: describe a database, diff it with the schema, write and apply SQL.
//!
//! Both sides of every comparison are a [`Shape`]: [`calculate`] builds one from a checked schema,
//! [`postgres::describe`] reads one from a live database.

mod calculate;
pub mod postgres;
pub mod shape;

pub use calculate::calculate;
pub use shape::Shape;

/// The version of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
