//! Running statements on Postgres, on the pool or inside a transaction.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::postgres::{PgArguments, PgPool, PgRow, Postgres};
use sqlx::{Arguments, AssertSqlSafe, FromRow};

use crate::dates::{PgDate, PgTime, PgTimestamp, PgTimestamptz};
use crate::error::{Error, Result};
use crate::query::{Built, Select};
use crate::value::Value;

/// What every connection shares: constraint names for errors, and a statement counter.
#[derive(Debug, Default)]
struct Shared {
    constraints: HashMap<String, Vec<String>>,
    statements: AtomicU64,
}

/// A connection pool and the schema's constraint names (for errors that name fields).
#[derive(Debug, Clone)]
pub struct Db {
    pool: PgPool,
    shared: Arc<Shared>,
}

/// An open transaction. Statements run on it until it is committed or rolled back; dropping it
/// rolls back.
#[derive(Debug)]
pub struct Tx<'c> {
    tx: sqlx::Transaction<'c, Postgres>,
    shared: Arc<Shared>,
}

/// How strictly a transaction is isolated from others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// Postgres's default.
    ReadCommitted,
    /// One snapshot for the whole transaction.
    RepeatableRead,
    /// As if transactions ran one after another; may fail with P2034 and need a retry.
    Serializable,
}

fn bind_list(
    args: &mut PgArguments,
    items: &[Value],
) -> std::result::Result<(), sqlx::error::BoxDynError> {
    macro_rules! all {
        ($variant:ident, $t:ty) => {
            items
                .iter()
                .map(|v| match v {
                    Value::$variant(x) => Some(x.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<$t>>>()
        };
    }
    if let Some(v) = all!(Int, i32) {
        return args.add(v);
    }
    if let Some(v) = all!(BigInt, i64) {
        return args.add(v);
    }
    if let Some(v) = all!(Bool, bool) {
        return args.add(v);
    }
    if let Some(v) = all!(Float, f64) {
        return args.add(v);
    }
    if let Some(v) = all!(DateTime, i64) {
        return args.add(v.into_iter().map(PgTimestamptz).collect::<Vec<_>>());
    }
    if let Some(v) = all!(Timestamp, i64) {
        return args.add(v.into_iter().map(PgTimestamp).collect::<Vec<_>>());
    }
    if let Some(v) = all!(Date, i32) {
        return args.add(v.into_iter().map(PgDate).collect::<Vec<_>>());
    }
    if let Some(v) = all!(Time, i64) {
        return args.add(v.into_iter().map(PgTime).collect::<Vec<_>>());
    }
    let texts: Option<Vec<String>> = items
        .iter()
        .map(|v| match v {
            Value::Text(s) | Value::Uuid(s) | Value::Decimal(s) | Value::Enum(s, _) => {
                Some(s.clone())
            }
            _ => None,
        })
        .collect();
    match texts {
        Some(v) => args.add(v),
        None => Err("a list must hold values of one simple type".into()),
    }
}

fn arguments(params: &[Value]) -> Result<PgArguments> {
    let mut args = PgArguments::default();
    for v in params {
        let added = match v {
            Value::Null => args.add(None::<String>),
            Value::Bool(x) => args.add(*x),
            Value::Int(x) => args.add(*x),
            Value::BigInt(x) => args.add(*x),
            Value::Float(x) => args.add(*x),
            Value::Text(s)
            | Value::Uuid(s)
            | Value::Decimal(s)
            | Value::Enum(s, _)
            | Value::ObjectId(s) => args.add(s.clone()),
            Value::Bytes(b) => args.add(b.clone()),
            Value::Json(j) => args.add(sqlx::types::Json(j.clone())),
            Value::DateTime(t) => args.add(PgTimestamptz(*t)),
            Value::Timestamp(t) => args.add(PgTimestamp(*t)),
            Value::Date(d) => args.add(PgDate(*d)),
            Value::Time(t) => args.add(PgTime(*t)),
            Value::List(items) => bind_list(&mut args, items),
        };
        added.map_err(|e| {
            Error::new(
                "P2009",
                format!("a value cannot be sent to the database: {e}"),
            )
        })?;
    }
    Ok(args)
}

async fn fetch_all_on<'e, E, T>(e: E, built: Built, shared: &Shared) -> Result<Vec<T>>
where
    E: sqlx::Executor<'e, Database = Postgres>,
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    shared.statements.fetch_add(1, Ordering::Relaxed);
    let args = arguments(&built.params)?;
    // the SQL is made by lumen's renderer (or is the caller's raw SQL); values are parameters
    sqlx::query_as_with::<_, T, _>(AssertSqlSafe(built.sql), args)
        .fetch_all(e)
        .await
        .map_err(|err| Error::from_sqlx(err, &shared.constraints))
}

async fn execute_on<'e, E>(e: E, built: Built, shared: &Shared) -> Result<u64>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    shared.statements.fetch_add(1, Ordering::Relaxed);
    let args = arguments(&built.params)?;
    sqlx::query_with(AssertSqlSafe(built.sql), args)
        .execute(e)
        .await
        .map(|r| r.rows_affected())
        .map_err(|err| Error::from_sqlx(err, &shared.constraints))
}

async fn count_on<'e, E>(e: E, select: &Select, shared: &Shared) -> Result<i64>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    shared.statements.fetch_add(1, Ordering::Relaxed);
    let built = select.count();
    let args = arguments(&built.params)?;
    sqlx::query_scalar_with::<_, i64, _>(AssertSqlSafe(built.sql), args)
        .fetch_one(e)
        .await
        .map_err(|err| Error::from_sqlx(err, &shared.constraints))
}

/// Something statements run on: the pool (`&Db`) or a transaction (`Tx`). The generated client
/// is generic over it, so every operation works inside and outside transactions.
#[allow(
    async_fn_in_trait,
    reason = "lumen's own code and the generated client are the callers"
)]
pub trait Run {
    /// Every row, decoded as `T`.
    ///
    /// # Errors
    /// A coded [`Error`].
    async fn fetch_all<T>(&mut self, built: Built) -> Result<Vec<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin;

    /// Run a statement; the number of rows it touched.
    ///
    /// # Errors
    /// A coded [`Error`].
    async fn execute(&mut self, built: Built) -> Result<u64>;

    /// How many rows a select matches.
    ///
    /// # Errors
    /// A coded [`Error`].
    async fn count(&mut self, select: &Select) -> Result<i64>;

    /// The first row, if any.
    ///
    /// # Errors
    /// A coded [`Error`].
    async fn fetch_optional<T>(&mut self, built: Built) -> Result<Option<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        Ok(self.fetch_all(built).await?.into_iter().next())
    }

    /// Exactly one row.
    ///
    /// # Errors
    /// P2025 when there is none.
    async fn fetch_one<T>(&mut self, built: Built) -> Result<T>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        self.fetch_optional(built)
            .await?
            .ok_or_else(|| Error::not_found("record"))
    }
}

impl<T: Run> Run for &mut T {
    async fn fetch_all<U>(&mut self, built: Built) -> Result<Vec<U>>
    where
        U: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        (**self).fetch_all(built).await
    }

    async fn execute(&mut self, built: Built) -> Result<u64> {
        (**self).execute(built).await
    }

    async fn count(&mut self, select: &Select) -> Result<i64> {
        (**self).count(select).await
    }
}

impl Run for &Db {
    async fn fetch_all<T>(&mut self, built: Built) -> Result<Vec<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        fetch_all_on(&self.pool, built, &self.shared).await
    }

    async fn execute(&mut self, built: Built) -> Result<u64> {
        execute_on(&self.pool, built, &self.shared).await
    }

    async fn count(&mut self, select: &Select) -> Result<i64> {
        count_on(&self.pool, select, &self.shared).await
    }
}

impl Run for Tx<'_> {
    async fn fetch_all<T>(&mut self, built: Built) -> Result<Vec<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        fetch_all_on(&mut *self.tx, built, &self.shared).await
    }

    async fn execute(&mut self, built: Built) -> Result<u64> {
        execute_on(&mut *self.tx, built, &self.shared).await
    }

    async fn count(&mut self, select: &Select) -> Result<i64> {
        count_on(&mut *self.tx, select, &self.shared).await
    }
}

impl Db {
    /// Connect to `url`.
    ///
    /// # Errors
    /// P1013 when `url` is not a `postgresql://` or `postgres://` URL, P1001 when the database
    /// cannot be reached.
    pub async fn connect(url: &str) -> Result<Self> {
        let scheme = url.split_once("://").map_or("", |(s, _)| s);
        if scheme != "postgresql" && scheme != "postgres" {
            let shown = url.split_once(':').map_or(url, |(s, _)| s);
            return Err(Error::new(
                "P1013",
                format!(
                    "the database URL must start with postgresql://, but it starts with `{shown}:`"
                ),
            ));
        }
        let pool = PgPool::connect(url)
            .await
            .map_err(|e| Error::from_sqlx(e, &HashMap::new()))?;
        Ok(Self::from_pool(pool))
    }

    /// Use an existing pool.
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            shared: Arc::new(Shared::default()),
        }
    }

    /// Name the fields behind constraint names, so errors can say `email` instead of
    /// `User_email_key`. The generated client fills this from the schema.
    #[must_use]
    pub fn with_constraints(self, constraints: HashMap<String, Vec<String>>) -> Self {
        Self {
            pool: self.pool,
            shared: Arc::new(Shared {
                constraints,
                statements: AtomicU64::new(0),
            }),
        }
    }

    /// The pool, for anything lumen does not cover.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// How many statements this `Db` (and its transactions) has run, for tests and metrics.
    #[must_use]
    pub fn statements(&self) -> u64 {
        self.shared.statements.load(Ordering::Relaxed)
    }

    /// Every row, decoded as `T`.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn fetch_all<T>(&self, built: Built) -> Result<Vec<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        fetch_all_on(&self.pool, built, &self.shared).await
    }

    /// The first row, if any.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn fetch_optional<T>(&self, built: Built) -> Result<Option<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        Ok(self.fetch_all(built).await?.into_iter().next())
    }

    /// Exactly one row.
    ///
    /// # Errors
    /// P2025 when there is none; a coded [`Error`] otherwise.
    pub async fn fetch_one<T>(&self, built: Built) -> Result<T>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        self.fetch_optional(built)
            .await?
            .ok_or_else(|| Error::not_found("record"))
    }

    /// Run a statement; the number of rows it touched.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn execute(&self, built: Built) -> Result<u64> {
        execute_on(&self.pool, built, &self.shared).await
    }

    /// How many rows a select matches.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn count(&self, select: &Select) -> Result<i64> {
        count_on(&self.pool, select, &self.shared).await
    }

    /// Hand-written SQL with parameters `$1`, `$2`, ..., decoded as `T`. The SQL is the caller's;
    /// values must still be passed as parameters, never formatted into it.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn query_raw<T>(&self, sql: &'static str, params: Vec<Value>) -> Result<Vec<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        self.fetch_all(Built {
            sql: sql.to_owned(),
            params,
        })
        .await
    }

    /// Start a transaction by hand; prefer [`Db::transaction`].
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn begin(&self) -> Result<Tx<'static>> {
        let tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Error::from_sqlx(e, &self.shared.constraints))?;
        Ok(Tx {
            tx,
            shared: Arc::clone(&self.shared),
        })
    }

    /// Run `f` in a transaction: committed when it returns `Ok`, rolled back when it returns
    /// `Err` (or panics).
    ///
    /// # Errors
    /// The error `f` returned, or a coded [`Error`] from beginning or committing.
    pub async fn transaction<T, F>(&self, f: F) -> Result<T>
    where
        F: AsyncFnOnce(&mut Tx<'static>) -> Result<T>,
    {
        let mut tx = self.begin().await?;
        finish(f(&mut tx).await, tx).await
    }

    /// Like [`Db::transaction`] with an isolation level.
    ///
    /// # Errors
    /// As [`Db::transaction`]; P2034 when a serializable transaction conflicts.
    pub async fn transaction_with<T, F>(&self, isolation: Isolation, f: F) -> Result<T>
    where
        F: AsyncFnOnce(&mut Tx<'static>) -> Result<T>,
    {
        let mut tx = self.begin().await?;
        let level = match isolation {
            Isolation::ReadCommitted => "READ COMMITTED",
            Isolation::RepeatableRead => "REPEATABLE READ",
            Isolation::Serializable => "SERIALIZABLE",
        };
        tx.execute(Built {
            sql: format!("SET TRANSACTION ISOLATION LEVEL {level}"),
            params: Vec::new(),
        })
        .await?;
        finish(f(&mut tx).await, tx).await
    }
}

async fn finish<T>(result: Result<T>, tx: Tx<'_>) -> Result<T> {
    let shared = Arc::clone(&tx.shared);
    match result {
        Ok(v) => {
            tx.tx
                .commit()
                .await
                .map_err(|e| Error::from_sqlx(e, &shared.constraints))?;
            Ok(v)
        }
        Err(e) => {
            // a failed rollback leaves the connection to be closed by the pool; the error that
            // caused it is the one worth reporting
            let _ = tx.tx.rollback().await;
            Err(e)
        }
    }
}

impl Tx<'_> {
    /// A nested transaction (a savepoint): rolling it back keeps what the outer one did.
    ///
    /// # Errors
    /// The error `f` returned, or a coded [`Error`].
    pub async fn transaction<T, F>(&mut self, f: F) -> Result<T>
    where
        F: for<'s> AsyncFnOnce(&mut Tx<'s>) -> Result<T>,
    {
        let inner = sqlx::Acquire::begin(&mut *self.tx)
            .await
            .map_err(|e| Error::from_sqlx(e, &self.shared.constraints))?;
        let mut nested = Tx {
            tx: inner,
            shared: Arc::clone(&self.shared),
        };
        let result = f(&mut nested).await;
        finish(result, nested).await
    }

    /// Commit by hand (after [`Db::begin`]).
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn commit(self) -> Result<()> {
        self.tx
            .commit()
            .await
            .map_err(|e| Error::from_sqlx(e, &self.shared.constraints))
    }

    /// Roll back by hand.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn rollback(self) -> Result<()> {
        self.tx
            .rollback()
            .await
            .map_err(|e| Error::from_sqlx(e, &self.shared.constraints))
    }
}
