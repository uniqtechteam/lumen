//! SPEC-GEN-001 FR-005: dates through the generated client, with `time` (the default feature) and
//! with `chrono` (the `chrono` feature). Both clients come from the same schema
//! (`lumen-db-codegen/tests/dates.prisma`), write the same rows, find them with the same filters
//! and give the same JSON. The database's time zone is not UTC on purpose: `timestamp` and
//! `timestamptz` parameters must be sent as what they are, or Postgres would shift them.
//! Runs when `LUMEN_TEST_POSTGRES_URL` / `LUMEN_TEST_MONGO_URL` are set.

#![cfg(any(feature = "time", feature = "chrono"))]
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    reason = "tests: a failure should stop the test; each live test is one scenario"
)]

use std::sync::atomic::{AtomicUsize, Ordering};

use lumen_orm::sqlx::{AssertSqlSafe, PgPool};
use lumen_orm::{Db, Set};

#[cfg(feature = "time")]
#[rustfmt::skip]
#[path = "../../lumen-db-codegen/tests/generated/dates.rs"]
#[allow(dead_code)]
mod time_db;

#[cfg(feature = "chrono")]
#[rustfmt::skip]
#[path = "../../lumen-db-codegen/tests/generated/dates_chrono.rs"]
#[allow(dead_code)]
mod chrono_db;

/// What `lumen migrate` creates for `dates.prisma`.
const TABLE: &str = r#"
CREATE TABLE "Event" (
    "id" SERIAL NOT NULL,
    "at" TIMESTAMP(3) NOT NULL,
    "atTz" TIMESTAMPTZ(6) NOT NULL,
    "day" DATE NOT NULL,
    "clock" time(6) without time zone NOT NULL,
    "endsAt" TIMESTAMP(3),
    "endsAtTz" TIMESTAMPTZ(3),
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL,
    CONSTRAINT "Event_pkey" PRIMARY KEY ("id")
);
"#;

/// The first event as JSON, the same from both clients. The fractions have 3 or 6 digits because
/// `chrono` pads to those while `time` writes the fewest.
const FIRST_JSON: &str = r#"{"id":1,"at":"2024-02-29T23:59:59.123","atTz":"2024-02-29T23:59:59.123456Z","day":"2024-02-29","clock":"13:45:30.654321","endsAt":null,"endsAtTz":"2030-01-01T00:00:00.125Z"}"#;

async fn run(pool: &PgPool, sql: &str) {
    lumen_orm::sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(pool)
        .await
        .unwrap();
}

/// A fresh database whose sessions run in Ho Chi Minh City time (UTC+7).
async fn database() -> Option<(PgPool, Db, String)> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let url = std::env::var("LUMEN_TEST_POSTGRES_URL").ok()?;
    let admin = PgPool::connect(&url).await.unwrap();
    let name = format!(
        "lumen_dates_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    run(
        &admin,
        &format!("drop database if exists {name} with (force)"),
    )
    .await;
    run(&admin, &format!("create database {name}")).await;
    run(
        &admin,
        &format!("alter database {name} set timezone to 'Asia/Ho_Chi_Minh'"),
    )
    .await;
    let base = url.rsplit_once('/').map_or(url.as_str(), |(b, _)| b);
    let db = Db::connect(&format!("{base}/{name}")).await.unwrap();
    run(db.pool(), TABLE).await;
    Some((admin, db, name))
}

async fn drop_database(admin: PgPool, db: Db, name: String) {
    db.pool().close().await;
    run(
        &admin,
        &format!("drop database if exists {name} with (force)"),
    )
    .await;
}

/// Writes two events, reads them back, filters on every column and updates one. `$m` is the
/// generated module, `$v` makes the values; returns the first event's JSON without the
/// timestamps the database filled.
macro_rules! scenario {
    ($m:ident, $db:expr, $v:ident) => {{
        use $m::{Client, event};
        let client = Client::new($db);
        let first = event::Create {
            id: None,
            at: $v::at(2024, 2, 29, 23, 59, 59, 123_000),
            at_tz: $v::at_tz(2024, 2, 29, 23, 59, 59, 123_456),
            day: $v::day(2024, 2, 29),
            clock: $v::clock(13, 45, 30, 654_321),
            ends_at: None,
            ends_at_tz: Some($v::at_tz(2030, 1, 1, 0, 0, 0, 125_000)),
            created_at: None,
            updated_at: None,
        };
        let second = event::Create {
            id: None,
            at: $v::at(2024, 3, 1, 0, 0, 0, 0),
            at_tz: $v::at_tz(2024, 3, 1, 0, 0, 0, 1),
            day: $v::day(1969, 12, 31),
            clock: $v::clock(23, 59, 59, 999_999),
            ends_at: Some($v::at(1999, 12, 31, 23, 59, 59, 999_000)),
            ends_at_tz: None,
            created_at: None,
            updated_at: None,
        };
        let a = client.event().create(first.clone()).await.unwrap();
        let b = client.event().create(second.clone()).await.unwrap();

        // what was written comes back unchanged
        for (made, given) in [(&a, &first), (&b, &second)] {
            assert_eq!(made.at, given.at);
            assert_eq!(made.at_tz, given.at_tz);
            assert_eq!(made.day, given.day);
            assert_eq!(made.clock, given.clock);
            assert_eq!(made.ends_at, given.ends_at);
            assert_eq!(made.ends_at_tz, given.ends_at_tz);
            let found = client
                .event()
                .find_unique(event::UniqueWhere::Id(made.id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&found, made);
        }

        // the database stored the same instants (session time zone is UTC+7)
        let text: (String, String, String) = lumen_orm::sqlx::query_as(AssertSqlSafe(
            r#"SELECT to_char("at", 'YYYY-MM-DD HH24:MI:SS.MS'),
                      to_char("atTz" AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS.US'),
                      to_char("day", 'YYYY-MM-DD') FROM "Event" WHERE "id" = $1"#,
        ))
        .bind(a.id)
        .fetch_one(client.db().pool())
        .await
        .unwrap();
        assert_eq!(
            text,
            (
                "2024-02-29 23:59:59.123".to_owned(),
                "2024-02-29 23:59:59.123456".to_owned(),
                "2024-02-29".to_owned()
            )
        );

        // every comparison, on every kind of column
        let ids = |rows: Vec<event::Event>| rows.into_iter().map(|e| e.id).collect::<Vec<_>>();
        let find = |f: lumen_orm::Filter| {
            client
                .event()
                .find_many()
                .filter(f)
                .order_by(event::id::asc())
                .exec()
        };
        assert_eq!(ids(find(event::at::gt(first.at)).await.unwrap()), [b.id]);
        assert_eq!(ids(find(event::at::lte(first.at)).await.unwrap()), [a.id]);
        assert_eq!(
            ids(find(event::at::equals(second.at)).await.unwrap()),
            [b.id]
        );
        assert_eq!(
            ids(find(event::at_tz::lt(second.at_tz)).await.unwrap()),
            [a.id]
        );
        assert_eq!(
            ids(find(event::at_tz::gte(first.at_tz)).await.unwrap()),
            [a.id, b.id]
        );
        assert_eq!(
            ids(find(event::at_tz::equals(second.at_tz)).await.unwrap()),
            [b.id]
        );
        assert_eq!(ids(find(event::day::lt(first.day)).await.unwrap()), [b.id]);
        assert_eq!(
            ids(find(event::day::equals(first.day)).await.unwrap()),
            [a.id]
        );
        assert_eq!(
            ids(find(event::clock::gt(first.clock)).await.unwrap()),
            [b.id]
        );
        assert_eq!(
            ids(find(event::ends_at::lt(first.at)).await.unwrap()),
            [b.id]
        );
        assert_eq!(
            ids(find(event::at::is_in([first.at, second.at])).await.unwrap()),
            [a.id, b.id]
        );
        assert_eq!(
            ids(find(event::at_tz::not_in([first.at_tz])).await.unwrap()),
            [b.id]
        );
        assert_eq!(
            ids(find(event::day::is_in([second.day])).await.unwrap()),
            [b.id]
        );
        assert_eq!(
            ids(find(event::clock::is_in([first.clock])).await.unwrap()),
            [a.id]
        );
        assert_eq!(
            ids(find(event::created_at::lte(a.created_at)).await.unwrap()),
            [a.id]
        );

        // updates set dates, and `@updatedAt` moves on
        let changed = client
            .event()
            .update(
                event::UniqueWhere::Id(b.id),
                event::Update {
                    ends_at: Set::To(None),
                    ends_at_tz: Set::To(first.ends_at_tz),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(changed.ends_at, None);
        assert_eq!(changed.ends_at_tz, first.ends_at_tz);
        assert!(changed.updated_at >= b.updated_at);
        assert_eq!(
            client
                .event()
                .count(Some(event::updated_at::gte(a.updated_at)))
                .await
                .unwrap(),
            2
        );

        // JSON round trip
        let json = lumen_orm::serde_json::to_string(&a).unwrap();
        let back: event::Event = lumen_orm::serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
        let mut shown: lumen_orm::serde_json::Value =
            lumen_orm::serde_json::from_str(&json).unwrap();
        let fields = shown.as_object_mut().unwrap();
        fields.remove("createdAt");
        fields.remove("updatedAt");
        assert_eq!(
            shown,
            lumen_orm::serde_json::from_str::<lumen_orm::serde_json::Value>(FIRST_JSON).unwrap()
        );
        shown
    }};
}

#[cfg(feature = "time")]
mod time_values {
    use lumen_orm::time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

    pub fn day(y: i32, m: u8, d: u8) -> Date {
        Date::from_calendar_date(y, Month::try_from(m).unwrap(), d).unwrap()
    }

    pub fn clock(h: u8, m: u8, s: u8, us: u32) -> Time {
        Time::from_hms_micro(h, m, s, us).unwrap()
    }

    #[allow(clippy::too_many_arguments, reason = "a date and a time, spelled out")]
    pub fn at(y: i32, mo: u8, d: u8, h: u8, mi: u8, s: u8, us: u32) -> PrimitiveDateTime {
        PrimitiveDateTime::new(day(y, mo, d), clock(h, mi, s, us))
    }

    #[allow(clippy::too_many_arguments, reason = "a date and a time, spelled out")]
    pub fn at_tz(y: i32, mo: u8, d: u8, h: u8, mi: u8, s: u8, us: u32) -> OffsetDateTime {
        at(y, mo, d, h, mi, s, us).assume_utc()
    }
}

#[cfg(feature = "chrono")]
mod chrono_values {
    use lumen_orm::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

    pub fn day(y: i32, m: u8, d: u8) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m.into(), d.into()).unwrap()
    }

    pub fn clock(h: u8, m: u8, s: u8, us: u32) -> NaiveTime {
        NaiveTime::from_hms_micro_opt(h.into(), m.into(), s.into(), us).unwrap()
    }

    #[allow(clippy::too_many_arguments, reason = "a date and a time, spelled out")]
    pub fn at(y: i32, mo: u8, d: u8, h: u8, mi: u8, s: u8, us: u32) -> NaiveDateTime {
        day(y, mo, d).and_time(clock(h, mi, s, us))
    }

    #[allow(clippy::too_many_arguments, reason = "a date and a time, spelled out")]
    pub fn at_tz(y: i32, mo: u8, d: u8, h: u8, mi: u8, s: u8, us: u32) -> DateTime<Utc> {
        at(y, mo, d, h, mi, s, us).and_utc()
    }
}

#[cfg(feature = "time")]
#[tokio::test]
async fn time_dates_round_trip_on_postgres() {
    let Some((admin, db, name)) = database().await else {
        return;
    };
    scenario!(time_db, db.clone(), time_values);
    drop_database(admin, db, name).await;
}

#[cfg(feature = "chrono")]
#[tokio::test]
async fn chrono_dates_round_trip_on_postgres() {
    let Some((admin, db, name)) = database().await else {
        return;
    };
    scenario!(chrono_db, db.clone(), chrono_values);
    drop_database(admin, db, name).await;
}

/// With both features on, a row written through one client reads back the same through the other.
#[cfg(all(feature = "time", feature = "chrono"))]
#[tokio::test]
async fn both_clients_read_each_others_rows() {
    let Some((admin, db, name)) = database().await else {
        return;
    };
    let from_time = scenario!(time_db, db.clone(), time_values);
    let rows = chrono_db::Client::new(db.clone())
        .event()
        .find_many()
        .order_by(chrono_db::event::id::asc())
        .exec()
        .await
        .unwrap();
    let mut seen: lumen_orm::serde_json::Value = lumen_orm::serde_json::to_value(&rows[0]).unwrap();
    let fields = seen.as_object_mut().unwrap();
    fields.remove("createdAt");
    fields.remove("updatedAt");
    assert_eq!(seen, from_time);
    drop_database(admin, db, name).await;
}

/// MongoDB stores BSON dates; `time` and `chrono` values filter on them the same way.
#[cfg(feature = "mongodb")]
#[tokio::test]
async fn dates_filter_bson_dates_on_mongodb() {
    use lumen_orm::mongo::{DateTime, Find, MongoDb};
    use lumen_orm::{Order, Value, col};

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Doc {
        id: i32,
        at: DateTime,
    }

    let Ok(url) = std::env::var("LUMEN_TEST_MONGO_URL") else {
        return;
    };
    let db = MongoDb::connect(&url, &format!("lumen_dates_{}", std::process::id()))
        .await
        .unwrap();
    db.database().drop().await.unwrap();
    // 2024-02-29T23:59:59.123Z and one millisecond later
    let ms = 1_709_251_199_123;
    let docs = [
        Doc {
            id: 1,
            at: DateTime::from_millis(ms),
        },
        Doc {
            id: 2,
            at: DateTime::from_millis(ms + 1),
        },
    ];
    db.insert_many("Event", &docs).await.unwrap();
    let ids = async |f: lumen_orm::Filter| {
        db.find::<Doc>(&Find {
            collection: "Event".into(),
            filter: Some(f),
            order: vec![("id".into(), Order::Asc)],
            ..Find::default()
        })
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect::<Vec<_>>()
    };
    let cases: Vec<(Value, Value)> = vec![
        #[cfg(feature = "time")]
        (
            time_values::at_tz(2024, 2, 29, 23, 59, 59, 123_000).into(),
            time_values::at(2024, 2, 29, 23, 59, 59, 123_000).into(),
        ),
        #[cfg(feature = "chrono")]
        (
            chrono_values::at_tz(2024, 2, 29, 23, 59, 59, 123_000).into(),
            chrono_values::at(2024, 2, 29, 23, 59, 59, 123_000).into(),
        ),
    ];
    for (tz, plain) in cases {
        assert_eq!(ids(col("at").equals(tz.clone())).await, [1]);
        assert_eq!(ids(col("at").gt(tz.clone())).await, [2]);
        assert_eq!(ids(col("at").lte(plain.clone())).await, [1]);
        assert_eq!(ids(col("at").is_in([tz, plain])).await, [1]);
    }
    assert_eq!(
        ids(col("at").lt(Value::from(DateTime::from_millis(ms + 1)))).await,
        [1],
        "a BSON date as a value"
    );
    db.database().drop().await.unwrap();
}
