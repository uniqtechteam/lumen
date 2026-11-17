//! lumen's runtime: the query builder, relations and transactions that the generated client
//! calls. Apps may also use it directly for dynamic queries:
//!
//! ```no_run
//! use lumen_orm::{Db, Order, Select, Table, col};
//!
//! #[derive(sqlx::FromRow)]
//! struct User {
//!     id: i32,
//!     email: String,
//! }
//!
//! # async fn run() -> lumen_orm::Result<()> {
//! let db = Db::connect("postgres://localhost/app").await?;
//! let mut q = Select::from(Table::new("User"));
//! q.filter = Some(col("email").ends_with("@example.com").and(col("id").gt(10)));
//! q.order = vec![("id".into(), Order::Desc)];
//! q.limit = Some(20);
//! let users: Vec<User> = db.fetch_all(q.build()).await?;
//! # Ok(()) }
//! ```

mod dates;
mod db;
mod error;
mod filter;
pub mod load;
#[cfg(feature = "mongodb")]
pub mod mongo;
pub mod nested;
mod query;
mod value;

#[cfg(feature = "time")]
pub use dates::serde as time_serde;
pub use db::{Db, Isolation, Run, Tx};
pub use error::{Error, ErrorInfo, Result};
pub use filter::{Col, Filter, Link, Mode, Op, Quantifier, Related, col};
pub use query::{Built, Delete, Insert, OnConflict, Order, Select, Table, Update, ident};
pub use value::{Set, Value};

/// sqlx, re-exported so generated code and apps use the same version.
pub use sqlx;
/// The crates the generated client's types come from, re-exported so a project only needs lumen.
pub use {rust_decimal, serde, serde_json, uuid};

/// `chrono`, for clients generated with `dateTimeType = "chrono"` (the `chrono` feature).
#[cfg(feature = "chrono")]
pub use chrono;
/// `time`, the date and time types of generated clients (the default `time` feature).
#[cfg(feature = "time")]
pub use time;

/// The version of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
