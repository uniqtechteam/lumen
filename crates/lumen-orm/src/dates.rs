//! Dates and times. A [`Value`] keeps them as plain numbers (microseconds or days since the Unix
//! epoch), so the query builder does not depend on a date crate. The types apps use come from
//! `time` (the default feature) or `chrono` (the `chrono` feature); both convert into a `Value`
//! the same way, so a client generated for either sends the same parameters.

use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgArgumentBuffer, PgHasArrayType, PgTypeInfo, Postgres};
use sqlx::{Encode, Type};

#[cfg(any(feature = "time", feature = "chrono"))]
use crate::value::Value;

/// Postgres counts from 2000-01-01; these are the Unix epoch's distance to it.
const PG_EPOCH_MICROS: i64 = 946_684_800_000_000;
const PG_EPOCH_DAYS: i32 = 10_957;
const MICROS_PER_DAY: i64 = 86_400_000_000;

macro_rules! pg_type {
    ($name:ident($inner:ty), $oid:literal, $array_oid:literal, $doc:literal, |$v:ident| $wire:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy)]
        pub(crate) struct $name(pub(crate) $inner);

        impl Type<Postgres> for $name {
            fn type_info() -> PgTypeInfo {
                PgTypeInfo::with_oid(Oid($oid))
            }
        }

        impl PgHasArrayType for $name {
            fn array_type_info() -> PgTypeInfo {
                PgTypeInfo::with_oid(Oid($array_oid))
            }
        }

        impl Encode<'_, Postgres> for $name {
            fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
                let $v = self.0;
                Encode::<Postgres>::encode($wire, buf)
            }
        }
    };
}

pg_type!(
    PgTimestamptz(i64),
    1184,
    1185,
    "A `timestamptz` parameter.",
    |v| v.saturating_sub(PG_EPOCH_MICROS)
);
pg_type!(
    PgTimestamp(i64),
    1114,
    1115,
    "A `timestamp` parameter.",
    |v| v.saturating_sub(PG_EPOCH_MICROS)
);
pg_type!(PgDate(i32), 1082, 1182, "A `date` parameter.", |v| v
    .saturating_sub(PG_EPOCH_DAYS));
pg_type!(PgTime(i64), 1083, 1183, "A `time` parameter.", |v| v);

/// Microseconds since midnight from the parts of a time of day.
#[cfg(any(feature = "time", feature = "chrono"))]
fn micros_of_day(h: u32, m: u32, s: u32, nanos: u32) -> i64 {
    i64::from(h * 3600 + m * 60 + s) * 1_000_000 + i64::from(nanos / 1000)
}

#[cfg(feature = "time")]
mod with_time {
    use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};

    use super::{Value, micros_of_day};

    /// Julian day number of 1970-01-01.
    const UNIX_JULIAN_DAY: i32 = 2_440_588;

    fn micros(t: OffsetDateTime) -> i64 {
        let us = t.unix_timestamp_nanos().div_euclid(1000);
        i64::try_from(us).unwrap_or(if us < 0 { i64::MIN } else { i64::MAX })
    }

    impl From<OffsetDateTime> for Value {
        fn from(v: OffsetDateTime) -> Self {
            Self::DateTime(micros(v))
        }
    }

    impl From<PrimitiveDateTime> for Value {
        fn from(v: PrimitiveDateTime) -> Self {
            Self::Timestamp(micros(v.assume_utc()))
        }
    }

    impl From<Date> for Value {
        fn from(v: Date) -> Self {
            Self::Date(v.to_julian_day() - UNIX_JULIAN_DAY)
        }
    }

    impl From<Time> for Value {
        fn from(v: Time) -> Self {
            Self::Time(micros_of_day(
                v.hour().into(),
                v.minute().into(),
                v.second().into(),
                v.nanosecond(),
            ))
        }
    }
}

/// Serde helpers the generated client uses for `time` fields, so JSON carries ISO 8601 text:
/// `rfc3339` for `OffsetDateTime` (`2024-05-01T09:30:00.25Z`) and `timestamp` for
/// `PrimitiveDateTime` (`2024-05-01T09:30:00.25`, the shape `chrono` uses for `NaiveDateTime`).
/// Each has an `option` module for `Option` fields.
#[cfg(feature = "time")]
pub mod serde {
    pub use time::serde::rfc3339;

    /// `PrimitiveDateTime` as `2024-05-01T09:30:00.25`; `option` for `Option` fields.
    pub mod timestamp {
        use serde::{Deserialize, Deserializer, Serializer};
        use time::PrimitiveDateTime;
        use time::format_description::BorrowedFormatItem;
        use time::macros::format_description;

        const FORMAT: &[BorrowedFormatItem<'_>] = format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second][optional [.[subsecond]]]"
        );

        /// Writes the timestamp as text.
        ///
        /// # Errors
        /// When the year has more than four digits.
        pub fn serialize<S: Serializer>(v: &PrimitiveDateTime, s: S) -> Result<S::Ok, S::Error> {
            let text = v.format(FORMAT).map_err(serde::ser::Error::custom)?;
            s.serialize_str(&text)
        }

        /// Reads the timestamp from text.
        ///
        /// # Errors
        /// When the text is not a timestamp in this shape.
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<PrimitiveDateTime, D::Error> {
            let text = <std::borrow::Cow<'de, str>>::deserialize(d)?;
            PrimitiveDateTime::parse(&text, FORMAT).map_err(serde::de::Error::custom)
        }

        /// The same, for `Option<PrimitiveDateTime>`.
        pub mod option {
            use serde::{Deserialize, Deserializer, Serializer};
            use time::PrimitiveDateTime;

            /// Writes the timestamp as text, or `null`.
            ///
            /// # Errors
            /// When the year has more than four digits.
            pub fn serialize<S: Serializer>(
                v: &Option<PrimitiveDateTime>,
                s: S,
            ) -> Result<S::Ok, S::Error> {
                match v {
                    Some(v) => super::serialize(v, s),
                    None => s.serialize_none(),
                }
            }

            /// Reads the timestamp from text, or `null`.
            ///
            /// # Errors
            /// When the text is not a timestamp in this shape.
            pub fn deserialize<'de, D: Deserializer<'de>>(
                d: D,
            ) -> Result<Option<PrimitiveDateTime>, D::Error> {
                let Some(text) = <Option<std::borrow::Cow<'de, str>>>::deserialize(d)? else {
                    return Ok(None);
                };
                PrimitiveDateTime::parse(&text, super::FORMAT)
                    .map(Some)
                    .map_err(serde::de::Error::custom)
            }
        }
    }
}

#[cfg(feature = "chrono")]
mod with_chrono {
    use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};

    use super::{Value, micros_of_day};

    /// Days from 0001-01-01 (day 1) to 1970-01-01.
    const UNIX_DAYS_FROM_CE: i32 = 719_163;

    impl From<DateTime<Utc>> for Value {
        fn from(v: DateTime<Utc>) -> Self {
            Self::DateTime(v.timestamp_micros())
        }
    }

    impl From<NaiveDateTime> for Value {
        fn from(v: NaiveDateTime) -> Self {
            Self::Timestamp(v.and_utc().timestamp_micros())
        }
    }

    impl From<NaiveDate> for Value {
        fn from(v: NaiveDate) -> Self {
            Self::Date(v.num_days_from_ce() - UNIX_DAYS_FROM_CE)
        }
    }

    impl From<NaiveTime> for Value {
        fn from(v: NaiveTime) -> Self {
            // a leap second (nanosecond >= 1e9) counts as the last microsecond of its second
            Self::Time(micros_of_day(
                v.hour(),
                v.minute(),
                v.second(),
                v.nanosecond().min(999_999_999),
            ))
        }
    }
}

/// Microseconds of a whole day, for code that turns a `Value::Date` into an instant.
pub(crate) const fn day_micros(days: i32) -> i64 {
    days as i64 * MICROS_PER_DAY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "time")]
    #[test]
    fn time_values_are_counted_from_the_unix_epoch() {
        use time::macros::{date, datetime, time};
        assert_eq!(
            Value::from(datetime!(1970-01-01 0:00 UTC)),
            Value::DateTime(0)
        );
        assert_eq!(
            Value::from(datetime!(2000-01-01 0:00:00.000_001)),
            Value::Timestamp(PG_EPOCH_MICROS + 1)
        );
        assert_eq!(
            Value::from(datetime!(1969-12-31 23:59:59.999_999_5 UTC)),
            Value::DateTime(-1)
        );
        assert_eq!(
            Value::from(date!(2000 - 01 - 01)),
            Value::Date(PG_EPOCH_DAYS)
        );
        assert_eq!(Value::from(date!(1969 - 12 - 31)), Value::Date(-1));
        assert_eq!(Value::from(time!(1:02:03.5)), Value::Time(3_723_500_000));
    }

    #[cfg(feature = "chrono")]
    #[test]
    fn chrono_values_match_the_time_ones() {
        use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};
        let d = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap_or_default();
        assert_eq!(Value::from(d), Value::Date(PG_EPOCH_DAYS));
        assert_eq!(
            Value::from(d.and_hms_opt(0, 0, 0).unwrap_or_default()),
            Value::Timestamp(PG_EPOCH_MICROS)
        );
        assert_eq!(
            Value::from(Utc.timestamp_opt(0, 0).single().unwrap_or_default()),
            Value::DateTime(0)
        );
        let t = NaiveTime::from_hms_micro_opt(1, 2, 3, 500_000).unwrap_or_default();
        assert_eq!(Value::from(t), Value::Time(3_723_500_000));
    }

    #[test]
    fn a_day_is_86400_seconds() {
        assert_eq!(day_micros(1), 86_400_000_000);
        assert_eq!(day_micros(-1), -86_400_000_000);
    }
}
