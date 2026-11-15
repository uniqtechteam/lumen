//! lumen's runtime: the query builder, relations and transactions that the generated client calls. Apps may use it directly for dynamic queries.

/// The version of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
