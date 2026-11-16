//! How many statements, and how long, loading users with their posts takes: `include_posts()`
//! against the loop people write by hand (one query per user). Prints JSON for `bench/plot.py`.
//!
//! `LUMEN_TEST_POSTGRES_URL=postgres://.. cargo run --release -p lumen-db-codegen --example bench_includes`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    reason = "a benchmark: a failure should stop it, and timings are small numbers"
)]

#[rustfmt::skip]
#[path = "../tests/generated/blog.rs"]
#[allow(dead_code)]
mod db;

use std::time::Instant;

use db::{Client, post};
use lumen_orm::sqlx::{AssertSqlSafe, PgPool};

const POSTS_PER_USER: usize = 5;
const RUNS: usize = 21;

async fn setup(url: &str, users: usize) -> (PgPool, Client, String) {
    let admin = PgPool::connect(url).await.unwrap();
    let name = format!("lumen_bench_{}_{users}", std::process::id());
    for sql in [
        format!("drop database if exists {name} with (force)"),
        format!("create database {name}"),
    ] {
        lumen_orm::sqlx::raw_sql(AssertSqlSafe(sql))
            .execute(&admin)
            .await
            .unwrap();
    }
    let base = url.rsplit_once('/').map_or(url, |(b, _)| b);
    let pool = PgPool::connect(&format!("{base}/{name}")).await.unwrap();
    let (schema, _) = lumen_schema::check(&lumen_schema::SourceFile::new(
        "blog.prisma",
        include_str!("../tests/blog.prisma"),
    ));
    let steps = lumen_migrate::diff(
        &lumen_migrate::Shape::default(),
        &lumen_migrate::calculate(&schema),
        &lumen_migrate::Renames::default(),
    );
    let seed = format!(
        "{}\nINSERT INTO users (email, updated_at) SELECT 'u' || i || '@example.com', now() FROM generate_series(1, {users}) i;\n\
         INSERT INTO \"Post\" (title, \"authorId\") SELECT 'post ' || p, u FROM generate_series(1, {users}) u, generate_series(1, {POSTS_PER_USER}) p;",
        lumen_migrate::postgres::script(&steps, &[])
    );
    lumen_orm::sqlx::raw_sql(AssertSqlSafe(seed))
        .execute(&pool)
        .await
        .unwrap();
    (admin, Client::new(lumen_orm::Db::from_pool(pool)), name)
}

/// The median time in milliseconds and the statements of one run.
async fn measure<F: AsyncFn() -> usize>(client: &Client, f: F) -> (f64, u64) {
    let mut times = Vec::new();
    let mut statements = 0;
    for _ in 0..RUNS {
        let before = client.db().statements();
        let start = Instant::now();
        let loaded = f().await;
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        statements = client.db().statements() - before;
        assert!(loaded > 0);
    }
    times.sort_by(f64::total_cmp);
    (times[RUNS / 2], statements)
}

#[tokio::main]
async fn main() {
    let url = std::env::var("LUMEN_TEST_POSTGRES_URL").expect("set LUMEN_TEST_POSTGRES_URL");
    let mut rows = Vec::new();
    for users in [10, 100, 1000] {
        let (admin, client, name) = setup(&url, users).await;
        let include = measure(&client, async || {
            let users = client
                .user()
                .find_many()
                .include_posts()
                .exec()
                .await
                .unwrap();
            users.iter().map(|u| u.posts.len()).sum()
        })
        .await;
        let by_hand = measure(&client, async || {
            let users = client.user().find_many().exec().await.unwrap();
            let mut n = 0;
            for u in &users {
                n += client
                    .post()
                    .find_many()
                    .filter(post::author_id::equals(u.id))
                    .exec()
                    .await
                    .unwrap()
                    .len();
            }
            n
        })
        .await;
        rows.push(format!(
            "{{\"users\": {users}, \"posts\": {}, \"include\": {{\"ms\": {:.2}, \"statements\": {}}}, \"loop\": {{\"ms\": {:.2}, \"statements\": {}}}}}",
            users * POSTS_PER_USER,
            include.0,
            include.1,
            by_hand.0,
            by_hand.1
        ));
        client.db().pool().close().await;
        lumen_orm::sqlx::raw_sql(AssertSqlSafe(format!("drop database {name} with (force)")))
            .execute(&admin)
            .await
            .unwrap();
    }
    println!("[\n  {}\n]", rows.join(",\n  "));
}
