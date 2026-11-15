//! Running statements on Postgres.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::postgres::{PgArguments, PgPool, PgRow};
use sqlx::{Arguments, AssertSqlSafe, FromRow};

use crate::error::{Error, Result};
use crate::query::{Built, Select};
use crate::value::Value;

/// A connection pool and the schema's constraint names (for errors that name fields).
#[derive(Debug, Clone)]
pub struct Db {
    pool: PgPool,
    constraints: Arc<HashMap<String, Vec<String>>>,
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
            Value::Text(s) | Value::Uuid(s) | Value::Decimal(s) | Value::Enum(s, _) => {
                args.add(s.clone())
            }
            Value::Bytes(b) => args.add(b.clone()),
            Value::Json(j) => args.add(sqlx::types::Json(j.clone())),
            Value::DateTime(t) => args.add(*t),
            Value::Timestamp(t) => args.add(*t),
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

impl Db {
    /// Connect to `url`.
    ///
    /// # Errors
    /// P1001 when the database cannot be reached.
    pub async fn connect(url: &str) -> Result<Self> {
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
            constraints: Arc::new(HashMap::new()),
        }
    }

    /// Name the fields behind constraint names, so errors can say `email` instead of
    /// `User_email_key`. The generated client fills this from the schema.
    #[must_use]
    pub fn with_constraints(mut self, constraints: HashMap<String, Vec<String>>) -> Self {
        self.constraints = Arc::new(constraints);
        self
    }

    /// The pool, for anything lumen does not cover.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    fn err(&self, e: sqlx::Error) -> Error {
        Error::from_sqlx(e, &self.constraints)
    }

    /// Every row, decoded as `T`.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn fetch_all<T>(&self, built: Built) -> Result<Vec<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        let args = arguments(&built.params)?;
        // the SQL is made by lumen's renderer; values are parameters
        sqlx::query_as_with::<_, T, _>(AssertSqlSafe(built.sql), args)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| self.err(e))
    }

    /// The first row, if any.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn fetch_optional<T>(&self, built: Built) -> Result<Option<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        let args = arguments(&built.params)?;
        sqlx::query_as_with::<_, T, _>(AssertSqlSafe(built.sql), args)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| self.err(e))
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
        let args = arguments(&built.params)?;
        sqlx::query_with(AssertSqlSafe(built.sql), args)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected())
            .map_err(|e| self.err(e))
    }

    /// How many rows a select matches.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn count(&self, select: &Select) -> Result<i64> {
        let built = select.count();
        let args = arguments(&built.params)?;
        sqlx::query_scalar_with::<_, i64, _>(AssertSqlSafe(built.sql), args)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| self.err(e))
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
}
