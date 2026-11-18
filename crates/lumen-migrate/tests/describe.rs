//! SPEC-MIG-001 FR-001/FR-002: the shape read from a live Postgres equals the shape calculated
//! from the schema, for the SQL Prisma writes for that schema. Runs when `LUMEN_TEST_POSTGRES_URL`
//! is set (CI sets it); each test gets its own database.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: a failure should stop the test"
)]

use std::sync::atomic::{AtomicUsize, Ordering};

use lumen_migrate::postgres::describe;
use lumen_migrate::{Shape, calculate};
use lumen_schema::{SourceFile, check};
use sqlx::{AssertSqlSafe, PgPool};

/// A fresh database (the name is made here, so the SQL is safe). A database left behind by a
/// failed run is dropped by the next run with the same name. `None` without a test server.
async fn fresh_database() -> Option<(PgPool, PgPool, String)> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let url = std::env::var("LUMEN_TEST_POSTGRES_URL").ok()?;
    let admin = PgPool::connect(&url)
        .await
        .expect("connect to the test server");
    let name = format!(
        "lumen_test_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    sqlx::raw_sql(AssertSqlSafe(format!("drop database if exists {name}")))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::raw_sql(AssertSqlSafe(format!("create database {name}")))
        .execute(&admin)
        .await
        .unwrap();
    let base = url.rsplit_once('/').map_or(url.as_str(), |(b, _)| b);
    let pool = PgPool::connect(&format!("{base}/{name}")).await.unwrap();
    Some((admin, pool, name))
}

async fn drop_database(admin: &PgPool, pool: PgPool, name: &str) {
    pool.close().await;
    sqlx::raw_sql(AssertSqlSafe(format!("drop database if exists {name}")))
        .execute(admin)
        .await
        .unwrap();
}

fn calculated(text: &str) -> Shape {
    let (schema, d) = check(&SourceFile::new("schema.prisma", text));
    assert!(!d.has_errors());
    calculate(&schema)
}

async fn round_trip(schema_file: &str, sql: &'static str, schemas: &[&str]) {
    let Some((admin, pool, name)) = fresh_database().await else {
        eprintln!("skipped: LUMEN_TEST_POSTGRES_URL is not set");
        return;
    };
    sqlx::raw_sql(sql)
        .execute(&pool)
        .await
        .expect("apply Prisma's SQL");
    let schemas: Vec<String> = schemas.iter().map(|s| (*s).to_owned()).collect();
    let described = describe(&pool, &schemas).await.expect("describe");
    drop_database(&admin, pool, &name).await;
    let expected = calculated(schema_file);
    assert_eq!(described.enums, expected.enums);
    assert_eq!(
        described.tables.len(),
        expected.tables.len(),
        "{described:#?}"
    );
    for (got, want) in described.tables.iter().zip(&expected.tables) {
        assert_eq!(got, want, "table {}.{}", want.schema, want.name);
    }
}

/// What `prisma migrate diff --from-empty --to-schema blog.prisma --script` writes.
const BLOG_SQL: &str = r#"
CREATE TYPE "user_role" AS ENUM ('READER', 'writer', 'ADMIN');
CREATE TABLE "users" (
    "id" SERIAL NOT NULL,
    "email" VARCHAR(320) NOT NULL,
    "name" TEXT,
    "role" "user_role" NOT NULL DEFAULT 'READER',
    "created_at" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_at" TIMESTAMP(3) NOT NULL,
    CONSTRAINT "users_pkey" PRIMARY KEY ("id")
);
CREATE TABLE "Profile" (
    "id" SERIAL NOT NULL,
    "bio" TEXT NOT NULL,
    "userId" INTEGER NOT NULL,
    CONSTRAINT "Profile_pkey" PRIMARY KEY ("id")
);
CREATE TABLE "Post" (
    "id" SERIAL NOT NULL,
    "title" TEXT NOT NULL,
    "body" TEXT,
    "published" BOOLEAN NOT NULL DEFAULT false,
    "authorId" INTEGER NOT NULL,
    "views" BIGINT NOT NULL DEFAULT 0,
    CONSTRAINT "Post_pkey" PRIMARY KEY ("id")
);
CREATE TABLE "Tag" (
    "name" TEXT NOT NULL,
    CONSTRAINT "Tag_pkey" PRIMARY KEY ("name")
);
CREATE TABLE "_PostToTag" (
    "A" INTEGER NOT NULL,
    "B" TEXT NOT NULL,
    CONSTRAINT "_PostToTag_AB_pkey" PRIMARY KEY ("A","B")
);
CREATE UNIQUE INDEX "users_email_key" ON "users"("email");
CREATE UNIQUE INDEX "Profile_userId_key" ON "Profile"("userId");
CREATE UNIQUE INDEX "Post_authorId_title_key" ON "Post"("authorId", "title");
CREATE INDEX "Post_authorId_published_idx" ON "Post"("authorId", "published" DESC);
CREATE INDEX "_PostToTag_B_index" ON "_PostToTag"("B");
ALTER TABLE "Profile" ADD CONSTRAINT "Profile_userId_fkey" FOREIGN KEY ("userId") REFERENCES "users"("id") ON DELETE CASCADE ON UPDATE CASCADE;
ALTER TABLE "Post" ADD CONSTRAINT "Post_authorId_fkey" FOREIGN KEY ("authorId") REFERENCES "users"("id") ON DELETE RESTRICT ON UPDATE CASCADE;
ALTER TABLE "_PostToTag" ADD CONSTRAINT "_PostToTag_A_fkey" FOREIGN KEY ("A") REFERENCES "Post"("id") ON DELETE CASCADE ON UPDATE CASCADE;
ALTER TABLE "_PostToTag" ADD CONSTRAINT "_PostToTag_B_fkey" FOREIGN KEY ("B") REFERENCES "Tag"("name") ON DELETE CASCADE ON UPDATE CASCADE;
"#;

/// The same for shop.prisma: two schemas, uuid, decimal, jsonb, bytea, an unsupported type, a
/// compound primary key and a partial index.
const SHOP_SQL: &str = r#"
CREATE SCHEMA IF NOT EXISTS "billing";
CREATE TABLE "public"."Product" (
    "id" UUID NOT NULL DEFAULT gen_random_uuid(),
    "sku" TEXT NOT NULL,
    "price" DECIMAL(10,2) NOT NULL,
    "data" JSONB NOT NULL DEFAULT '{}',
    "image" BYTEA,
    "location" point,
    "archived" BOOLEAN NOT NULL DEFAULT false,
    CONSTRAINT "Product_pkey" PRIMARY KEY ("id")
);
CREATE TABLE "billing"."Order" (
    "id" SERIAL NOT NULL,
    CONSTRAINT "Order_pkey" PRIMARY KEY ("id")
);
CREATE TABLE "billing"."OrderLine" (
    "orderId" INTEGER NOT NULL,
    "productId" UUID NOT NULL,
    "quantity" INTEGER NOT NULL DEFAULT 1,
    CONSTRAINT "OrderLine_pkey" PRIMARY KEY ("orderId","productId")
);
CREATE UNIQUE INDEX "Product_sku_key" ON "public"."Product"("sku");
CREATE INDEX "Product_sku_idx" ON "public"."Product"("sku") WHERE archived = false;
ALTER TABLE "billing"."OrderLine" ADD CONSTRAINT "OrderLine_orderId_fkey" FOREIGN KEY ("orderId") REFERENCES "billing"."Order"("id") ON DELETE CASCADE ON UPDATE NO ACTION;
ALTER TABLE "billing"."OrderLine" ADD CONSTRAINT "OrderLine_productId_fkey" FOREIGN KEY ("productId") REFERENCES "public"."Product"("id") ON DELETE RESTRICT ON UPDATE CASCADE;
"#;

#[tokio::test]
async fn ac_001_the_blog_database_matches_its_schema() {
    round_trip(
        include_str!("../../lumen-schema/tests/corpus/blog.prisma"),
        BLOG_SQL,
        &["public"],
    )
    .await;
}

#[tokio::test]
async fn ac_001_the_shop_database_matches_its_schema() {
    round_trip(
        include_str!("../../lumen-schema/tests/corpus/shop.prisma"),
        SHOP_SQL,
        &["public", "billing"],
    )
    .await;
}

#[tokio::test]
async fn the_migrations_table_is_never_part_of_the_shape() {
    let Some((admin, pool, name)) = fresh_database().await else {
        return;
    };
    sqlx::raw_sql(r#"CREATE TABLE "_prisma_migrations" ("id" VARCHAR(36) PRIMARY KEY); CREATE TABLE "a" ("id" INTEGER PRIMARY KEY);"#)
        .execute(&pool)
        .await
        .unwrap();
    let shape = describe(&pool, &["public".to_owned()]).await.unwrap();
    drop_database(&admin, pool, &name).await;
    let names: Vec<&str> = shape.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["a"]);
}

#[tokio::test]
async fn partial_expression_and_covering_indexes_are_read_as_they_are() {
    let Some((admin, pool, name)) = fresh_database().await else {
        return;
    };
    sqlx::raw_sql(
        r#"CREATE TABLE "a" ("id" INTEGER PRIMARY KEY, "email" TEXT, "gone" TIMESTAMP, "n" INTEGER);
CREATE UNIQUE INDEX "a_email_live" ON "a" ("email") WHERE "gone" IS NULL;
CREATE INDEX "a_lower" ON "a" (lower("email"), "n" DESC);
CREATE INDEX "a_n_cover" ON "a" ("n") INCLUDE ("email");"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let shape = describe(&pool, &["public".to_owned()]).await.unwrap();
    drop_database(&admin, pool, &name).await;
    let t = &shape.tables[0];
    let by = |n: &str| t.indexes.iter().find(|i| i.name == n).unwrap();
    let live = by("a_email_live");
    assert_eq!(live.predicate.as_deref(), Some("gone IS NULL"));
    assert!(live.unique && !live.expression);
    let lower = by("a_lower");
    assert!(lower.expression);
    let cols: Vec<(&str, lumen_migrate::shape::Sort)> = lower
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.sort))
        .collect();
    assert_eq!(
        cols,
        [
            ("lower(email)", lumen_migrate::shape::Sort::Asc),
            ("n", lumen_migrate::shape::Sort::Desc)
        ]
    );
    // INCLUDE columns are not keys
    assert_eq!(by("a_n_cover").columns.len(), 1);
}
