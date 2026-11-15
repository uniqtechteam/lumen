//! SPEC-MIG-001 FR-004, FR-005, FR-007, FR-009: `migrate dev`, `deploy`, `status` and `reset` on a
//! real Postgres, with Prisma's folder layout and history table. Runs when
//! `LUMEN_TEST_POSTGRES_URL` is set.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a failure should stop the test"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use lumen_migrate::commands::{self, DevOptions, DevOutcome, Project};
use lumen_schema::{SourceFile, check};
use sqlx::{AssertSqlSafe, PgPool};

const DATASOURCE: &str = "datasource db {\n  provider = \"postgresql\"\n}\n";
const V1: &str =
    "model User {\n  id    Int    @id @default(autoincrement())\n  email String @unique\n}\n";
const V2: &str = "model User {\n  id    Int     @id @default(autoincrement())\n  email String  @unique\n  name  String?\n}\n";

struct Setup {
    admin: PgPool,
    db: String,
    url: String,
    dir: PathBuf,
}

impl Setup {
    async fn new() -> Option<Self> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let server = std::env::var("LUMEN_TEST_POSTGRES_URL").ok()?;
        let admin = PgPool::connect(&server).await.unwrap();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let db = format!("lumen_cmd_{}_{n}", std::process::id());
        sqlx::raw_sql(AssertSqlSafe(format!("drop database if exists {db}")))
            .execute(&admin)
            .await
            .unwrap();
        sqlx::raw_sql(AssertSqlSafe(format!("create database {db}")))
            .execute(&admin)
            .await
            .unwrap();
        let base = server.rsplit_once('/').map_or(server.as_str(), |(b, _)| b);
        let dir = std::env::temp_dir().join(format!("lumen-cmd-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Some(Self {
            admin,
            url: format!("{base}/{db}"),
            db,
            dir,
        })
    }

    fn project(&self, models: &str) -> Project {
        let (schema, d) = check(&SourceFile::new(
            "schema.prisma",
            format!("{DATASOURCE}{models}"),
        ));
        assert!(!d.has_errors());
        Project {
            schema,
            migrations_dir: self.dir.clone(),
            database_url: self.url.clone(),
            shadow_database_url: None,
        }
    }

    async fn sql(&self, sql: &str) {
        let pool = PgPool::connect(&self.url).await.unwrap();
        sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    async fn finish(self) {
        sqlx::raw_sql(AssertSqlSafe(format!(
            "drop database if exists {} with (force)",
            self.db
        )))
        .execute(&self.admin)
        .await
        .unwrap();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn opts(name: &str, now: i64) -> DevOptions {
    DevOptions {
        name: name.into(),
        now: Some(now),
        ..DevOptions::default()
    }
}

#[tokio::test]
async fn ac_fr_004_dev_writes_prisma_style_migrations_and_applies_them() {
    let Some(s) = Setup::new().await else { return };
    let DevOutcome::Created {
        folder, sql, ran, ..
    } = commands::dev(&s.project(V1), &opts("init", 1_763_199_000))
        .await
        .unwrap()
    else {
        panic!("expected a migration")
    };
    assert_eq!(folder, "20251115093000_init");
    assert!(ran);
    assert!(sql.contains("CREATE TABLE \"User\""));
    let lock = std::fs::read_to_string(s.dir.join("migration_lock.toml")).unwrap();
    assert!(lock.contains("provider = \"postgresql\""));
    assert!(s.dir.join(&folder).join("migration.sql").exists());

    let again = commands::dev(&s.project(V1), &opts("noop", 1_763_199_100))
        .await
        .unwrap();
    assert_eq!(again, DevOutcome::UpToDate { applied: vec![] });

    let DevOutcome::Created { folder: second, .. } =
        commands::dev(&s.project(V2), &opts("add name", 1_763_199_200))
            .await
            .unwrap()
    else {
        panic!("expected a second migration")
    };
    assert_eq!(second, "20251115093320_add_name");
    let status = commands::status(&s.project(V2)).await.unwrap();
    assert_eq!(
        status.applied,
        ["20251115093000_init", "20251115093320_add_name"]
    );
    assert!(status.pending.is_empty() && status.failed.is_empty() && status.edited.is_empty());

    let pool = PgPool::connect(&s.url).await.unwrap();
    let rows: Vec<(String, i32, bool)> = sqlx::query_as(
        "select migration_name, applied_steps_count, finished_at is not null from _prisma_migrations order by migration_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    pool.close().await;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|(_, steps, finished)| *steps == 1 && *finished)
    );
    s.finish().await;
}

#[tokio::test]
async fn ac_fr_007_drift_and_edited_migrations_need_a_reset() {
    let Some(s) = Setup::new().await else { return };
    commands::dev(&s.project(V1), &opts("init", 1_763_199_000))
        .await
        .unwrap();
    s.sql("ALTER TABLE \"User\" ADD COLUMN \"secret\" TEXT;")
        .await;
    let DevOutcome::NeedsReset(reason) = commands::dev(&s.project(V1), &opts("x", 1_763_199_100))
        .await
        .unwrap()
    else {
        panic!("drift must be reported")
    };
    assert!(
        reason.starts_with("the database has changes that are not in any migration"),
        "{reason}"
    );
    assert!(reason.contains("ADD COLUMN \"secret\" TEXT"), "{reason}");

    s.sql("ALTER TABLE \"User\" DROP COLUMN \"secret\";").await;
    let file = s.dir.join("20251115093000_init/migration.sql");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("{text}\n-- edited\n")).unwrap();
    let DevOutcome::NeedsReset(reason) = commands::dev(&s.project(V1), &opts("x", 1_763_199_100))
        .await
        .unwrap()
    else {
        panic!("an edited migration must be reported")
    };
    assert_eq!(
        reason,
        "the migration `20251115093000_init` was changed after it was applied"
    );
    assert_eq!(
        commands::status(&s.project(V1)).await.unwrap().edited,
        ["20251115093000_init"]
    );

    let applied = commands::reset(&s.project(V1)).await.unwrap();
    assert_eq!(applied, ["20251115093000_init"]);
    assert_eq!(
        commands::dev(&s.project(V1), &opts("x", 1)).await.unwrap(),
        DevOutcome::UpToDate { applied: vec![] }
    );
    s.finish().await;
}

#[tokio::test]
async fn ac_003_data_loss_needs_confirmation_and_is_written_in_the_file() {
    let Some(s) = Setup::new().await else { return };
    commands::dev(&s.project(V2), &opts("init", 1_763_199_000))
        .await
        .unwrap();
    s.sql("INSERT INTO \"User\" (email, name) VALUES ('a@b.c', 'Ana');")
        .await;
    let DevOutcome::NeedsConfirmation(findings) =
        commands::dev(&s.project(V1), &opts("drop name", 1_763_199_100))
            .await
            .unwrap()
    else {
        panic!("dropping a column with data must ask first")
    };
    assert_eq!(
        findings[0].message,
        "You are about to drop the column `name` on `User`, which still has 1 non-null values."
    );
    assert!(
        !s.dir.join("20251115093140_drop_name").exists(),
        "nothing is written before the answer"
    );

    let yes = DevOptions {
        accept_data_loss: true,
        ..opts("drop name", 1_763_199_100)
    };
    let DevOutcome::Created { sql, .. } = commands::dev(&s.project(V1), &yes).await.unwrap() else {
        panic!()
    };
    assert!(
        sql.starts_with(
            "/*\n  Warnings:\n\n  - You are about to drop the column `name` on `User`."
        ),
        "{sql}"
    );
    s.finish().await;
}

#[tokio::test]
async fn impossible_steps_are_blocked() {
    let Some(s) = Setup::new().await else { return };
    commands::dev(&s.project(V1), &opts("init", 1_763_199_000))
        .await
        .unwrap();
    s.sql("INSERT INTO \"User\" (email) VALUES ('a@b.c');")
        .await;
    let required = V1.replace(
        "email String @unique\n",
        "email String @unique\n  name  String\n",
    );
    let DevOutcome::Blocked(findings) =
        commands::dev(&s.project(&required), &opts("name", 1_763_199_100))
            .await
            .unwrap()
    else {
        panic!("must be blocked")
    };
    assert!(findings[0].message.contains("but the table has 1 rows"));
    s.finish().await;
}

#[tokio::test]
async fn ac_fr_005_deploy_applies_pending_migrations_and_records_failures() {
    let Some(s) = Setup::new().await else { return };
    commands::dev(
        &DevOptions::default()
            .name
            .is_empty()
            .then(|| s.project(V1))
            .unwrap(),
        &DevOptions {
            create_only: true,
            ..opts("init", 1_763_199_000)
        },
    )
    .await
    .unwrap();
    assert_eq!(
        commands::status(&s.project(V1)).await.unwrap().pending,
        ["20251115093000_init"]
    );
    assert_eq!(
        commands::deploy(&s.project(V1)).await.unwrap(),
        ["20251115093000_init"]
    );
    assert_eq!(
        commands::deploy(&s.project(V1)).await.unwrap(),
        Vec::<String>::new(),
        "nothing twice"
    );

    let broken = s.dir.join("20251115100000_broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join("migration.sql"),
        "ALTER TABLE \"Nope\" ADD COLUMN x INT;",
    )
    .unwrap();
    let err = commands::deploy(&s.project(V1)).await.unwrap_err();
    assert!(
        err.message
            .starts_with("the migration `20251115100000_broken` failed:"),
        "{err}"
    );
    assert_eq!(
        commands::status(&s.project(V1)).await.unwrap().failed,
        ["20251115100000_broken"]
    );
    let again = commands::deploy(&s.project(V1)).await.unwrap_err();
    assert!(again.message.contains("failed earlier"), "{again}");
    s.finish().await;
}
