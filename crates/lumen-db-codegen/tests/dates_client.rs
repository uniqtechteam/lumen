//! SPEC-GEN-001 FR-005: `DateTime` fields use `time` by default and `chrono` with
//! `dateTimeType = "chrono"`. Both clients for `tests/dates.prisma` are committed; lumen-orm's
//! `tests/dates.rs` runs them against Postgres.

#![allow(clippy::unwrap_used, reason = "tests: a failure should stop the test")]

#[rustfmt::skip]
#[path = "generated/dates.rs"]
#[allow(dead_code)]
mod db;

const SCHEMA: &str = include_str!("dates.prisma");

fn generate(text: &str) -> Result<String, String> {
    let (schema, d) = lumen_schema::check(&lumen_schema::SourceFile::new("dates.prisma", text));
    assert!(!d.has_errors());
    lumen_db_codegen::generate(&schema, "dates.prisma")
}

#[test]
fn both_date_clients_are_current() {
    assert_eq!(
        generate(SCHEMA).unwrap(),
        include_str!("generated/dates.rs"),
        "tests/generated/dates.rs is out of date: regenerate it with the gen example"
    );
    let chrono = SCHEMA.replace("dateTimeType = \"time\"", "dateTimeType = \"chrono\"");
    assert_eq!(
        generate(&chrono).unwrap(),
        include_str!("generated/dates_chrono.rs"),
        "tests/generated/dates_chrono.rs is out of date: regenerate it from dates.prisma with \
         dateTimeType = \"chrono\""
    );
}

#[test]
fn time_is_the_default() {
    let plain = SCHEMA.replace("  dateTimeType = \"time\"\n", "");
    assert_ne!(plain, SCHEMA);
    assert_eq!(generate(&plain).unwrap(), generate(SCHEMA).unwrap());
}

/// The `time` client's field types, checked by the compiler.
#[allow(dead_code, reason = "only compiled")]
fn time_types(
    e: &db::event::Event,
) -> (
    lumen_orm::time::PrimitiveDateTime,
    lumen_orm::time::OffsetDateTime,
    lumen_orm::time::Date,
    lumen_orm::time::Time,
) {
    (e.at, e.at_tz, e.day, e.clock)
}

#[test]
fn an_unknown_date_type_is_an_error() {
    let jiff = SCHEMA.replace("dateTimeType = \"time\"", "dateTimeType = \"jiff\"");
    assert_eq!(
        generate(&jiff).unwrap_err(),
        "dateTimeType = \"jiff\" is not supported; use \"time\" (the default) or \"chrono\""
    );
}
