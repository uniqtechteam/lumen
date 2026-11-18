//! SPEC-CLI-001 FR-002: the `lumen` binary, run the way people run it, in a fresh folder. The
//! database flows run when `LUMEN_TEST_POSTGRES_URL` / `LUMEN_TEST_MONGO_URL` are set.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a failure should stop the test"
)]

use std::path::Path;
use std::process::{Command, Output};

use sqlx::{AssertSqlSafe, PgPool};

fn lumen(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lumen"))
        .args(args)
        .current_dir(dir)
        .env_remove("DATABASE_URL")
        .output()
        .unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[track_caller]
fn ok(dir: &Path, args: &[&str]) -> Output {
    let out = lumen(dir, args);
    assert!(
        out.status.success(),
        "lumen {args:?} failed:\n{}",
        stderr(&out)
    );
    out
}

fn models(dir: &Path, extra: &str) {
    let path = dir.join("prisma/schema.prisma");
    let text = std::fs::read_to_string(&path).unwrap();
    let head = text.split("\nmodel ").next().unwrap();
    let head = format!("{head}\n");
    std::fs::write(&path, format!("{head}{extra}")).unwrap();
}

#[test]
fn ac_001_init_then_validate_format_and_generate() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    ok(d, &["init"]);
    assert!(d.join("prisma/schema.prisma").exists());
    assert!(d.join("prisma/migrations").is_dir());
    let env = std::fs::read_to_string(d.join(".env")).unwrap();
    assert!(env.contains("DATABASE_URL=\"postgresql://"), "{env}");

    let again = lumen(d, &["init"]);
    assert!(!again.status.success());
    assert!(stderr(&again).contains("already here"));

    ok(d, &["validate"]);
    ok(d, &["format", "--check"]);
    ok(d, &["generate"]);
    let client = std::fs::read_to_string(d.join("src/db.rs")).unwrap();
    assert!(client.contains("pub mod user"), "{client}");
    assert!(stderr(&ok(d, &["generate"])).contains("up to date"));

    ok(d, &["generate", "--output", "out/client.rs"]);
    assert!(d.join("out/client.rs").exists());
}

#[test]
fn ac_002_validate_and_format_report_problems() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    ok(d, &["init"]);
    models(d, "model Post {\n  id Int @id\n  title Strng\n}\n");
    let out = lumen(d, &["validate"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("Strng") && err.contains("String"), "{err}");

    models(d, "model Post {\n  id Int @id\n  title String\n}\n");
    ok(d, &["validate"]);
    let check = lumen(d, &["format", "--check"]);
    assert!(!check.status.success());
    ok(d, &["format"]);
    ok(d, &["format", "--check"]);
}

#[test]
fn ac_003_missing_schema_and_url_are_explained() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let out = lumen(d, &["validate"]);
    assert!(stderr(&out).contains("lumen init"), "{}", stderr(&out));

    ok(d, &["init"]);
    std::fs::write(d.join(".env"), "# no url\n").unwrap();
    let out = lumen(d, &["migrate", "status"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("DATABASE_URL is not set"),
        "{}",
        stderr(&out)
    );

    ok(d, &["migrate", "diff", "--from-empty"]);
    let sql = String::from_utf8(ok(d, &["migrate", "diff", "--from-empty"]).stdout).unwrap();
    assert!(sql.contains("CREATE TABLE \"User\""), "{sql}");
}

async fn fresh_postgres(name: &str) -> Option<String> {
    let server = std::env::var("LUMEN_TEST_POSTGRES_URL").ok()?;
    let admin = PgPool::connect(&server).await.unwrap();
    let db = format!("lumen_cli_{name}_{}", std::process::id());
    for sql in [
        format!("drop database if exists {db} with (force)"),
        format!("create database {db}"),
    ] {
        sqlx::raw_sql(AssertSqlSafe(sql))
            .execute(&admin)
            .await
            .unwrap();
    }
    admin.close().await;
    let base = server.rsplit_once('/').map_or(server.as_str(), |(b, _)| b);
    Some(format!("{base}/{db}"))
}

#[tokio::test]
async fn ac_004_postgres_migrate_dev_status_deploy_reset() {
    let Some(url) = fresh_postgres("flow").await else {
        eprintln!("skipped: LUMEN_TEST_POSTGRES_URL is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    ok(d, &["init", "--url", &url]);
    ok(d, &["migrate", "dev", "--name", "init"]);
    assert!(d.join("src/db.rs").exists(), "migrate dev runs generate");
    ok(d, &["migrate", "status"]);
    let again = stderr(&ok(d, &["migrate", "dev", "--name", "nothing"]));
    assert!(again.contains("in sync"), "{again}");

    // dropping a column that has data: refused without a terminal, accepted with the flag
    let pool = PgPool::connect(&url).await.unwrap();
    sqlx::query("insert into \"User\" (email, name) values ('a@b.c', 'Ann')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    models(
        d,
        "model User {\n  id    Int    @id @default(autoincrement())\n  email String @unique\n}\n",
    );
    let refused = lumen(d, &["migrate", "dev", "--name", "drop_name"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("--accept-data-loss"),
        "{}",
        stderr(&refused)
    );
    ok(
        d,
        &[
            "migrate",
            "dev",
            "--name",
            "drop_name",
            "--accept-data-loss",
        ],
    );
    let folders = std::fs::read_dir(d.join("prisma/migrations"))
        .unwrap()
        .count();
    assert_eq!(folders, 3, "two migrations and migration_lock.toml");

    // written but not applied: status says so, deploy applies it
    models(
        d,
        "model User {\n  id    Int     @id @default(autoincrement())\n  email String  @unique\n  bio   String?\n}\n",
    );
    ok(d, &["migrate", "dev", "--name", "bio", "--create-only"]);
    let pending = lumen(d, &["migrate", "status"]);
    assert!(!pending.status.success());
    assert!(stderr(&pending).contains("bio"), "{}", stderr(&pending));
    ok(d, &["migrate", "deploy"]);
    ok(d, &["migrate", "status"]);

    let no = lumen(d, &["migrate", "reset"]);
    assert!(!no.status.success(), "reset asks first");
    ok(d, &["migrate", "reset", "--force"]);
    ok(d, &["migrate", "status"]);
    let diff = ok(d, &["migrate", "diff"]);
    assert!(
        diff.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&diff.stdout)
    );
}

#[tokio::test]
async fn ac_005_postgres_db_push() {
    let Some(url) = fresh_postgres("push").await else {
        eprintln!("skipped: LUMEN_TEST_POSTGRES_URL is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    ok(d, &["init", "--url", &url]);
    ok(d, &["db", "push", "--skip-generate"]);
    assert!(!d.join("src/db.rs").exists());
    assert!(stderr(&ok(d, &["db", "push"])).contains("in sync"));
    let pool = PgPool::connect(&url).await.unwrap();
    let n: i64 = sqlx::query_scalar("select count(*) from \"User\"")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    pool.close().await;
}

#[test]
fn ac_006_mongo_db_push_and_pull() {
    let Ok(server) = std::env::var("LUMEN_TEST_MONGO_URL") else {
        eprintln!("skipped: LUMEN_TEST_MONGO_URL is not set");
        return;
    };
    let (base, query) = server
        .split_once('?')
        .map_or((server.as_str(), ""), |(b, q)| (b, q));
    let base = base.trim_end_matches('/');
    let url = format!("{base}/lumen_cli_{}?{query}", std::process::id());
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    ok(d, &["init", "--provider", "mongodb", "--url", &url]);
    let pushed = stderr(&ok(d, &["db", "push"]));
    assert!(
        pushed.contains("created index User_email_key on User"),
        "{pushed}"
    );
    assert!(stderr(&ok(d, &["db", "push"])).contains("in sync"));

    // SPEC-MONGO-001 FR-002: an index the schema no longer has is reported, not dropped
    let user = |extra: &str| {
        format!(
            "model User {{\n  id    String  @id @default(auto()) @map(\"_id\") @db.ObjectId\n  email String  @unique\n  name  String?\n{extra}}}\n"
        )
    };
    models(d, &user("\n  @@index([name])\n"));
    let pushed = stderr(&ok(d, &["db", "push"]));
    assert!(
        pushed.contains("created index User_name_idx on User"),
        "{pushed}"
    );
    models(d, &user(""));
    let pushed = stderr(&ok(d, &["db", "push"]));
    assert!(
        pushed.contains("not in the schema") && pushed.contains("User_name_idx on User (name)"),
        "{pushed}"
    );
    assert!(!pushed.contains("dropped"), "{pushed}");
    assert!(
        stderr(&ok(d, &["db", "push"])).contains("User_name_idx"),
        "still there, so reported again"
    );

    let pulled = String::from_utf8(ok(d, &["db", "pull", "--print"]).stdout).unwrap();
    assert!(pulled.contains("model User"), "{pulled}");
    assert!(
        pulled.contains("generator client"),
        "the generator block is kept:\n{pulled}"
    );

    let migrate = lumen(d, &["migrate", "dev"]);
    assert!(stderr(&migrate).contains("db push"), "{}", stderr(&migrate));
}

#[tokio::test]
async fn ac_007_postgres_resolve_a_failed_migration() {
    let Some(url) = fresh_postgres("resolve").await else {
        eprintln!("skipped: LUMEN_TEST_POSTGRES_URL is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    ok(d, &["init", "--url", &url]);
    ok(d, &["migrate", "dev", "--name", "init", "--create-only"]);
    let folder = std::fs::read_dir(d.join("prisma/migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.is_dir())
        .unwrap();
    let name = folder.file_name().unwrap().to_string_lossy().into_owned();
    let sql_path = folder.join("migration.sql");
    let good = std::fs::read_to_string(&sql_path).unwrap();
    std::fs::write(&sql_path, format!("{good}\nSELECT broken FROM nowhere;\n")).unwrap();

    let failed = lumen(d, &["migrate", "deploy"]);
    assert!(!failed.status.success());
    let status = stderr(&lumen(d, &["migrate", "status"]));
    assert!(
        status.contains("Failed") && status.contains(&name),
        "{status}"
    );
    let blocked = lumen(d, &["migrate", "deploy"]);
    assert!(stderr(&blocked).contains("resolve"), "{}", stderr(&blocked));

    let wrong = lumen(d, &["migrate", "resolve", "--rolled-back", "nope"]);
    assert!(!wrong.status.success());
    ok(d, &["migrate", "resolve", "--rolled-back", &name]);
    std::fs::write(&sql_path, &good).unwrap();
    // clear anything the failed run may have left behind
    let pool = PgPool::connect(&url).await.unwrap();
    sqlx::raw_sql("drop table if exists \"User\"")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    ok(d, &["migrate", "deploy"]);
    ok(d, &["migrate", "status"]);

    // an applied migration edited afterwards: deploy still succeeds, but says so
    std::fs::write(&sql_path, format!("{good}\n-- edited\n")).unwrap();
    let warned = ok(d, &["migrate", "deploy"]);
    assert!(
        stderr(&warned).contains("changed after they were applied")
            && stderr(&warned).contains(&name),
        "{}",
        stderr(&warned)
    );
}
