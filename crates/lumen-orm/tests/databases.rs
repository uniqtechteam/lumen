//! The databases the test suite runs against are reachable. Each test runs when its URL is set
//! (CI sets both) and is skipped otherwise, so `cargo test` works on a laptop without databases.

#![allow(clippy::expect_used, reason = "tests: a failure should stop the test")]

#[tokio::test]
async fn postgres_answers() {
    let Ok(url) = std::env::var("LUMEN_TEST_POSTGRES_URL") else {
        eprintln!("skipped: LUMEN_TEST_POSTGRES_URL is not set");
        return;
    };
    let pool = sqlx::PgPool::connect(&url)
        .await
        .expect("connect to Postgres");
    let one: i32 = sqlx::query_scalar("select 1")
        .fetch_one(&pool)
        .await
        .expect("select 1");
    assert_eq!(one, 1);
}

#[tokio::test]
async fn mongodb_answers() {
    let Ok(url) = std::env::var("LUMEN_TEST_MONGO_URL") else {
        eprintln!("skipped: LUMEN_TEST_MONGO_URL is not set");
        return;
    };
    let client = mongodb::Client::with_uri_str(&url)
        .await
        .expect("connect to MongoDB");
    client
        .database("admin")
        .run_command(mongodb::bson::doc! { "ping": 1 })
        .await
        .expect("ping");
}

#[tokio::test]
async fn a_url_that_is_not_postgres_fails_at_once() {
    let started = std::time::Instant::now();
    let err = lumen_orm::Db::connect("file:./dev.db")
        .await
        .expect_err("a file: URL is not Postgres");
    assert_eq!(err.code, "P1013");
    assert!(err.message.contains("`file:`"), "{}", err.message);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}
