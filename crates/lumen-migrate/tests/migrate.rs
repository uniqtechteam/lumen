//! SPEC-MIG-001 AC-001 and AC-003: for each kind of change, schema A → schema B on a real Postgres
//! gives a database whose shape is B, a second diff is empty, and destructive steps are reported
//! with the counts that matter. Runs when `LUMEN_TEST_POSTGRES_URL` is set.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a failure should stop the test"
)]

use std::sync::atomic::{AtomicUsize, Ordering};

use lumen_migrate::check::{self, Finding, Severity};
use lumen_migrate::postgres::{describe, script};
use lumen_migrate::{Renames, Shape, Step, calculate, diff, renames};
use lumen_schema::{SourceFile, check as check_schema};
use sqlx::{AssertSqlSafe, PgPool};

const DATASOURCE: &str = "datasource db {\n  provider = \"postgresql\"\n}\n";

struct Db {
    admin: PgPool,
    pool: PgPool,
    name: String,
}

impl Db {
    async fn fresh() -> Option<Self> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let url = std::env::var("LUMEN_TEST_POSTGRES_URL").ok()?;
        let admin = PgPool::connect(&url).await.unwrap();
        let name = format!(
            "lumen_mig_{}_{}",
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
        Some(Self { admin, pool, name })
    }

    async fn run(&self, sql: &str) {
        sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
            .execute(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("{e}\n---\n{sql}"));
    }

    async fn shape(&self) -> Shape {
        describe(&self.pool, &["public".to_owned()]).await.unwrap()
    }

    async fn drop(self) {
        self.pool.close().await;
        sqlx::raw_sql(AssertSqlSafe(format!(
            "drop database if exists {}",
            self.name
        )))
        .execute(&self.admin)
        .await
        .unwrap();
    }
}

fn schema(models: &str) -> lumen_schema::checked::Schema {
    let text = format!("{DATASOURCE}{models}");
    let file = SourceFile::new("schema.prisma", &text);
    let (s, d) = check_schema(&file);
    assert!(!d.has_errors(), "{}", d.render(&file));
    s
}

/// The result of migrating A → B.
struct Migrated {
    sql: String,
    findings: Vec<Finding>,
}

/// Create A from nothing, run `data`, migrate to B, and check the result is exactly B.
async fn migrate(a: &str, data: &str, b: &str) -> Option<Migrated> {
    let db = Db::fresh().await?;
    let shape_a = calculate(&schema(a));
    db.run(&script(
        &diff(&Shape::default(), &shape_a, &Renames::default()),
        &[],
    ))
    .await;
    assert_eq!(db.shape().await, shape_a, "creating A from nothing");
    if !data.is_empty() {
        db.run(data).await;
    }
    let target = schema(b);
    let shape_b = calculate(&target);
    let steps = diff(&db.shape().await, &shape_b, &renames(&target));
    let findings = check::against(&db.pool, &steps).await.unwrap();
    let sql = script(&steps, &[]);
    let blocked = findings
        .iter()
        .any(|f| f.severity == Severity::Unexecutable);
    if !blocked {
        db.run(&sql).await;
        let after = db.shape().await;
        assert_eq!(after, shape_b, "the database is B after migrating\n{sql}");
        assert_eq!(
            diff(&after, &shape_b, &Renames::default()),
            Vec::<Step>::new(),
            "nothing left to do"
        );
    }
    db.drop().await;
    Some(Migrated { sql, findings })
}

fn messages(m: &Migrated) -> Vec<&str> {
    m.findings.iter().map(|f| f.message.as_str()).collect()
}

const USER: &str =
    "model User {\n  id    Int    @id @default(autoincrement())\n  email String @unique\n}\n";

#[tokio::test]
async fn ac_001_the_corpus_schemas_build_from_nothing() {
    for text in [include_str!("../../lumen-schema/tests/corpus/blog.prisma")] {
        let Some(db) = Db::fresh().await else { return };
        let (s, _) = check_schema(&SourceFile::new("blog.prisma", text));
        let shape = calculate(&s);
        db.run(&script(
            &diff(&Shape::default(), &shape, &Renames::default()),
            &[],
        ))
        .await;
        assert_eq!(db.shape().await, shape);
        db.drop().await;
    }
}

#[tokio::test]
async fn tables_and_relations_are_added_and_dropped() {
    let b = format!("{USER}model Post {{\n  id       Int  @id @default(autoincrement())\n  author   User @relation(fields: [authorId], references: [id])\n  authorId Int\n}}\n").replace("email String @unique\n", "email String @unique\n  posts Post[]\n");
    let Some(m) = migrate(USER, "", &b).await else {
        return;
    };
    assert!(
        m.sql.contains("-- CreateTable\nCREATE TABLE \"Post\""),
        "{}",
        m.sql
    );
    assert!(m.sql.contains("FOREIGN KEY (\"authorId\") REFERENCES \"User\"(\"id\") ON DELETE RESTRICT ON UPDATE CASCADE"), "{}", m.sql);
    assert_eq!(m.findings, Vec::<Finding>::new());

    let data = "INSERT INTO \"User\" (email) VALUES ('a@b.c'); INSERT INTO \"Post\" (\"authorId\") VALUES (1), (1);";
    let m = migrate(&b, data, USER).await.unwrap();
    assert_eq!(
        messages(&m),
        ["You are about to drop the `Post` table, which has 2 rows."]
    );
}

#[tokio::test]
async fn columns_are_added_dropped_and_changed() {
    let with_name = USER.replace(
        "email String @unique\n",
        "email String @unique\n  name  String?\n  score Int     @default(0)\n",
    );
    let Some(m) = migrate(
        USER,
        "INSERT INTO \"User\" (email) VALUES ('a@b.c');",
        &with_name,
    )
    .await
    else {
        return;
    };
    assert_eq!(
        m.findings,
        Vec::<Finding>::new(),
        "optional and defaulted columns are safe: {:?}",
        messages(&m)
    );

    let data = "INSERT INTO \"User\" (email, name) VALUES ('a@b.c', 'Ana'), ('d@e.f', NULL);";
    let m = migrate(&with_name, data, USER).await.unwrap();
    assert_eq!(
        messages(&m),
        [
            "You are about to drop the column `name` on `User`, which still has 1 non-null values.",
            "You are about to drop the column `score` on `User`, which still has 2 non-null values."
        ]
    );

    let bigger = with_name.replace("score Int     @default(0)", "score BigInt  @default(5)");
    let m = migrate(&with_name, data, &bigger).await.unwrap();
    assert_eq!(
        m.findings,
        Vec::<Finding>::new(),
        "integer to bigint is a safe cast"
    );
    assert!(
        m.sql
            .contains("ALTER COLUMN \"score\" SET DATA TYPE BIGINT"),
        "{}",
        m.sql
    );
}

#[tokio::test]
async fn required_columns_on_tables_with_rows_are_refused() {
    let required = USER.replace(
        "email String @unique\n",
        "email String @unique\n  name  String\n",
    );
    let rows = "INSERT INTO \"User\" (email) VALUES ('a@b.c'), ('d@e.f');";
    let Some(m) = migrate(USER, rows, &required).await else {
        return;
    };
    assert_eq!(m.findings[0].severity, Severity::Unexecutable);
    assert_eq!(
        m.findings[0].message,
        "The required column `name` is added to `User` without a default, but the table has 2 rows."
    );
    assert_eq!(
        migrate(USER, "", &required).await.unwrap().findings,
        Vec::<Finding>::new(),
        "fine on an empty table"
    );

    let optional = USER.replace(
        "email String @unique\n",
        "email String @unique\n  name  String?\n",
    );
    let m = migrate(
        &optional,
        "INSERT INTO \"User\" (email) VALUES ('a@b.c');",
        &required,
    )
    .await
    .unwrap();
    assert_eq!(
        messages(&m),
        ["The column `name` on `User` becomes required, but 1 rows have NULL in it."]
    );
}

#[tokio::test]
async fn type_changes_that_cannot_cast_recreate_the_column() {
    let text = USER.replace(
        "email String @unique\n",
        "email String @unique\n  code  String?\n",
    );
    let number = text.replace("code  String?", "code  Int?");
    let Some(m) = migrate(
        &text,
        "INSERT INTO \"User\" (email, code) VALUES ('a@b.c', 'x1');",
        &number,
    )
    .await
    else {
        return;
    };
    assert!(
        m.sql
            .contains("DROP COLUMN \"code\",\nADD COLUMN \"code\" INTEGER"),
        "{}",
        m.sql
    );
    assert_eq!(m.findings.len(), 1);
    assert!(
        m.findings[0]
            .message
            .contains("dropped and added again; 1 values will be lost")
    );

    let short = text.replace("code  String?", "code  String? @db.VarChar(5)");
    let m = migrate(
        &text,
        "INSERT INTO \"User\" (email, code) VALUES ('a@b.c', 'x1');",
        &short,
    )
    .await
    .unwrap();
    assert!(
        m.findings[0]
            .message
            .contains("changes from `text` to `character varying(5)`")
    );
}

#[tokio::test]
async fn ac_fr_008_rename_hints_keep_the_data() {
    let before = USER.replace(
        "email String @unique\n",
        "email    String @unique\n  fullName String?\n",
    );
    let after = "// lumen: renamed from User\nmodel Account {\n  id   Int    @id @default(autoincrement())\n  email String @unique\n  // lumen: renamed from fullName\n  name String?\n}\n";
    let Some(db) = Db::fresh().await else { return };
    let shape_a = calculate(&schema(&before));
    db.run(&script(
        &diff(&Shape::default(), &shape_a, &Renames::default()),
        &[],
    ))
    .await;
    db.run("INSERT INTO \"User\" (email, \"fullName\") VALUES ('a@b.c', 'Ana');")
        .await;
    let target = schema(after);
    let steps = diff(&db.shape().await, &calculate(&target), &renames(&target));
    let sql = script(&steps, &[]);
    assert!(
        sql.contains("ALTER TABLE \"User\" RENAME TO \"Account\";"),
        "{sql}"
    );
    assert!(
        sql.contains("RENAME COLUMN \"fullName\" TO \"name\""),
        "{sql}"
    );
    assert!(
        sql.contains("RENAME CONSTRAINT \"User_pkey\" TO \"Account_pkey\""),
        "{sql}"
    );
    assert!(
        sql.contains("ALTER INDEX \"User_email_key\" RENAME TO \"Account_email_key\""),
        "{sql}"
    );
    assert!(!sql.contains("DROP"), "a rename never drops: {sql}");
    db.run(&sql).await;
    let name: String = sqlx::query_scalar("SELECT name FROM \"Account\"")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(name, "Ana");
    db.drop().await;
}

#[tokio::test]
async fn enums_gain_and_lose_values() {
    let a = "enum Role {\n  A\n  B\n}\nmodel M {\n  id   Int  @id\n  role Role @default(A)\n}\n";
    let more = a.replace("  B\n", "  B\n  C\n");
    let Some(m) = migrate(a, "", &more).await else {
        return;
    };
    assert!(
        m.sql.contains("ALTER TYPE \"Role\" ADD VALUE 'C';"),
        "{}",
        m.sql
    );
    let fewer = a.replace("  B\n", "");
    let m = migrate(a, "INSERT INTO \"M\" (id) VALUES (1);", &fewer)
        .await
        .unwrap();
    assert!(
        m.sql.contains("CREATE TYPE \"Role_new\" AS ENUM ('A');"),
        "{}",
        m.sql
    );
    assert!(
        m.sql.contains("USING (\"role\"::text::\"Role_new\")"),
        "{}",
        m.sql
    );
    assert!(
        m.findings[0]
            .message
            .contains("removed from the enum `Role`")
    );
}

#[tokio::test]
async fn indexes_keys_and_actions_change() {
    let base = "model User {\n  id    Int    @id\n  posts Post[]\n}\nmodel Post {\n  id     Int    @id\n  title  String\n  user   User   @relation(fields: [userId], references: [id])\n  userId Int\n}\n";
    let changed = base
        .replace(
            "user   User   @relation(fields: [userId], references: [id])",
            "user   User   @relation(fields: [userId], references: [id], onDelete: Cascade)",
        )
        .replace(
            "  userId Int\n}",
            "  userId Int\n\n  @@unique([title])\n  @@index([userId])\n}",
        );
    let Some(m) = migrate(base, "", &changed).await else {
        return;
    };
    assert!(
        m.sql.contains("CREATE UNIQUE INDEX \"Post_title_key\""),
        "{}",
        m.sql
    );
    assert!(m.sql.contains("ON DELETE CASCADE"), "{}", m.sql);
    assert!(m.sql.contains("-- DropForeignKey"), "{}", m.sql);
    let back = migrate(&changed, "", base).await.unwrap();
    assert!(
        back.sql.contains("DROP INDEX \"Post_title_key\";"),
        "{}",
        back.sql
    );

    let many = "model Post {\n  id   Int   @id\n  tags Tag[]\n}\nmodel Tag {\n  id    Int    @id\n  posts Post[]\n}\n";
    let m = migrate(
        "model Post {\n  id Int @id\n}\nmodel Tag {\n  id Int @id\n}\n",
        "",
        many,
    )
    .await
    .unwrap();
    assert!(m.sql.contains("CREATE TABLE \"_PostToTag\""), "{}", m.sql);

    let compound = "model Line {\n  a Int\n  b Int\n\n  @@id([a, b])\n}\n";
    let single = "model Line {\n  a Int @id\n  b Int\n}\n";
    let m = migrate(single, "", compound).await.unwrap();
    assert!(m.sql.contains("DROP CONSTRAINT \"Line_pkey\""), "{}", m.sql);
    assert!(
        m.sql
            .contains("ADD CONSTRAINT \"Line_pkey\" PRIMARY KEY (\"a\",\"b\")"),
        "{}",
        m.sql
    );
}
