//! Values sent to the database as parameters. They are never written into the SQL text.

/// One parameter. Variants that Postgres cannot infer from a plain text parameter carry the cast
/// the placeholder needs (`$1::uuid`, `$1::numeric`, `$1::"Role"`).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// `boolean`.
    Bool(bool),
    /// `integer`.
    Int(i32),
    /// `bigint`.
    BigInt(i64),
    /// `double precision`.
    Float(f64),
    /// `text` and `varchar`.
    Text(String),
    /// `bytea`.
    Bytes(Vec<u8>),
    /// `jsonb`.
    Json(serde_json::Value),
    /// `timestamptz`, as microseconds since 1970-01-01 00:00 UTC.
    DateTime(i64),
    /// `timestamp` (without time zone), the default column type for `DateTime`, as microseconds
    /// since 1970-01-01 00:00 (read as UTC).
    Timestamp(i64),
    /// `date`, as days since 1970-01-01.
    Date(i32),
    /// `time` (without time zone), as microseconds since midnight.
    Time(i64),
    /// `uuid`, as text (cast with `::uuid`).
    Uuid(String),
    /// `numeric`, as text so no precision is lost (cast with `::numeric`).
    Decimal(String),
    /// An enum value and the type's SQL name (cast with `::"Role"`).
    Enum(String, String),
    /// A MongoDB ObjectId, as 24 hex characters.
    ObjectId(String),
    /// A list of values of one type, for `text[]` columns and `= ANY(..)`.
    List(Vec<Value>),
}

impl Value {
    /// The current time for a `timestamp` column (`@updatedAt` on a plain `DateTime`).
    #[must_use]
    pub fn timestamp_now() -> Self {
        Self::Timestamp(now_micros())
    }

    /// The current time for a `timestamptz` column.
    #[must_use]
    pub fn timestamptz_now() -> Self {
        Self::DateTime(now_micros())
    }

    /// The cast the placeholder needs, if any.
    #[must_use]
    pub fn cast(&self) -> Option<String> {
        match self {
            Self::Uuid(_) => Some("uuid".to_owned()),
            Self::Decimal(_) => Some("numeric".to_owned()),
            Self::Enum(_, ty) => Some(ty.clone()),
            Self::List(items) => items.first().and_then(Self::cast).map(|c| format!("{c}[]")),
            _ => None,
        }
    }
}

macro_rules! from {
    ($($t:ty => $variant:ident),* $(,)?) => {
        $(impl From<$t> for Value {
            fn from(v: $t) -> Self {
                Self::$variant(v.into())
            }
        })*
    };
}

from! {
    bool => Bool,
    i32 => Int,
    i64 => BigInt,
    f64 => Float,
    String => Text,
    &str => Text,
    Vec<u8> => Bytes,
    serde_json::Value => Json,
}

impl From<uuid::Uuid> for Value {
    fn from(v: uuid::Uuid) -> Self {
        Self::Uuid(v.to_string())
    }
}

impl From<rust_decimal::Decimal> for Value {
    fn from(v: rust_decimal::Decimal) -> Self {
        Self::Decimal(v.to_string())
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Self {
        Self::List(v.into_iter().map(Into::into).collect())
    }
}

/// A field in an update: left as it is, or set to a new value.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Set<T> {
    /// Leave the column as it is.
    #[default]
    NotSet,
    /// Set the column.
    To(T),
}

impl<T> Set<T> {
    /// Whether a value is given.
    #[must_use]
    pub fn is_set(&self) -> bool {
        matches!(self, Self::To(_))
    }
}

impl<T> From<T> for Set<T> {
    fn from(v: T) -> Self {
        Self::To(v)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Self::Null, Into::into)
    }
}

/// Microseconds since the Unix epoch, from the system clock.
fn now_micros() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_micros()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_micros()).map_or(i64::MIN, |us| -us),
    }
}
