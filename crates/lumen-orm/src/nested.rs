//! Nested writes (SPEC-QUERY-001 FR-005): a create that also creates or connects related records,
//! all in one transaction. The generated client builds `create_nested` and `create_with_*` on
//! these types; the steps follow the foreign key, so the record holding it is written last.
//!
//! ```
//! use lumen_orm::nested::{Many, One};
//!
//! // two new children, and one existing child (found by a unique key) moved under the parent
//! let posts: Many<&str, i32> = Many::create(vec!["Hello", "Again"]).and_connect(vec![7]);
//! assert_eq!(posts.create.len(), 2);
//! let author: One<&str, i32> = One::Connect(1);
//! assert!(matches!(author, One::Connect(1)));
//! ```

use crate::db::{Db, Tx};
use crate::error::Result;

/// A nested write of a to-one relation: create the related record, or connect an existing one
/// found by a unique key (P2025 when there is none).
#[derive(Debug, Clone, PartialEq)]
pub enum One<C, W> {
    /// Create the related record.
    Create(C),
    /// Connect the record with this id or unique key.
    Connect(W),
}

/// A nested write of a to-many relation: records to create and existing records to connect
/// (each found by a unique key, P2025 when one is missing). Empty by default.
#[derive(Debug, Clone, PartialEq)]
pub struct Many<C, W> {
    /// New related records.
    pub create: Vec<C>,
    /// Existing records to connect, by id or unique key.
    pub connect: Vec<W>,
}

impl<C, W> Default for Many<C, W> {
    fn default() -> Self {
        Self {
            create: Vec::new(),
            connect: Vec::new(),
        }
    }
}

impl<C, W> Many<C, W> {
    /// Create these records.
    #[must_use]
    pub fn create(records: Vec<C>) -> Self {
        Self {
            create: records,
            connect: Vec::new(),
        }
    }

    /// Connect these existing records.
    #[must_use]
    pub fn connect(keys: Vec<W>) -> Self {
        Self {
            create: Vec::new(),
            connect: keys,
        }
    }

    /// Also connect these existing records.
    #[must_use]
    pub fn and_connect(mut self, keys: Vec<W>) -> Self {
        self.connect.extend(keys);
        self
    }

    /// Whether there is nothing to write.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.create.is_empty() && self.connect.is_empty()
    }
}

/// Where a nested write can run as one unit: on the pool it opens a transaction, inside a
/// transaction it opens a savepoint, so a failed nested write never leaves rows behind and never
/// undoes what the outer transaction did before it.
#[allow(
    async_fn_in_trait,
    reason = "lumen's own code and the generated client are the callers"
)]
pub trait Atomic {
    /// Run `f` as one unit: kept when it returns `Ok`, undone when it returns `Err`.
    ///
    /// # Errors
    /// The error `f` returned, or a coded error from beginning or committing.
    async fn atomic<T, F>(&mut self, f: F) -> Result<T>
    where
        F: for<'s> AsyncFnOnce(&mut Tx<'s>) -> Result<T>;
}

impl Atomic for &Db {
    async fn atomic<T, F>(&mut self, f: F) -> Result<T>
    where
        F: for<'s> AsyncFnOnce(&mut Tx<'s>) -> Result<T>,
    {
        Db::transaction(self, f).await
    }
}

impl Atomic for Tx<'_> {
    async fn atomic<T, F>(&mut self, f: F) -> Result<T>
    where
        F: for<'s> AsyncFnOnce(&mut Tx<'s>) -> Result<T>,
    {
        self.transaction(f).await
    }
}

impl<A: Atomic> Atomic for &mut A {
    async fn atomic<T, F>(&mut self, f: F) -> Result<T>
    where
        F: for<'s> AsyncFnOnce(&mut Tx<'s>) -> Result<T>,
    {
        (**self).atomic(f).await
    }
}

/// [`Atomic`] for MongoDB: on the database it starts a transaction (which needs a replica set);
/// inside a transaction it runs in that transaction, since MongoDB has no savepoints, so an error
/// aborts the outer transaction too when it is returned from it.
#[cfg(feature = "mongodb")]
#[allow(
    async_fn_in_trait,
    reason = "lumen's own code and the generated client are the callers"
)]
pub trait MongoAtomic {
    /// Run `f` as one unit.
    ///
    /// # Errors
    /// The error `f` returned, or a coded error (P2010 on a standalone server).
    async fn atomic<T, F>(&mut self, f: F) -> Result<T>
    where
        F: AsyncFnOnce(&mut crate::mongo::MongoTx) -> Result<T>;
}

#[cfg(feature = "mongodb")]
impl MongoAtomic for &crate::mongo::MongoDb {
    async fn atomic<T, F>(&mut self, f: F) -> Result<T>
    where
        F: AsyncFnOnce(&mut crate::mongo::MongoTx) -> Result<T>,
    {
        crate::mongo::MongoDb::transaction(self, f).await
    }
}

#[cfg(feature = "mongodb")]
impl MongoAtomic for crate::mongo::MongoTx {
    async fn atomic<T, F>(&mut self, f: F) -> Result<T>
    where
        F: AsyncFnOnce(&mut crate::mongo::MongoTx) -> Result<T>,
    {
        f(self).await
    }
}

#[cfg(feature = "mongodb")]
impl<A: MongoAtomic> MongoAtomic for &mut A {
    async fn atomic<T, F>(&mut self, f: F) -> Result<T>
    where
        F: AsyncFnOnce(&mut crate::mongo::MongoTx) -> Result<T>,
    {
        (**self).atomic(f).await
    }
}
