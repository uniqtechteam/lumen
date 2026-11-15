//! Values sent to the database as parameters. They are never written into the SQL text.

use chrono::{DateTime, NaiveDateTime, Utc};

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
    /// `timestamptz`.
    DateTime(DateTime<Utc>),
    /// `timestamp` (without time zone), Prisma's default for `DateTime`.
    Timestamp(NaiveDateTime),
    /// `uuid`, as text (cast with `::uuid`).
    Uuid(String),
    /// `numeric`, as text so no precision is lost (cast with `::numeric`).
    Decimal(String),
    /// An enum value and the type's SQL name (cast with `::"Role"`).
    Enum(String, String),
    /// A list of values of one type, for `text[]` columns and `= ANY(..)`.
    List(Vec<Value>),
}

impl Value {
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
    DateTime<Utc> => DateTime,
    NaiveDateTime => Timestamp,
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
