//! SPEC-MIG-001 AC-002: lumen and the real Prisma CLI take turns on one project. Runs when
//! `LUMEN_TEST_PRISMA=1` and `LUMEN_TEST_POSTGRES_URL` are set and `npx` is installed (CI's
//! interop job); it downloads Prisma 6.19.3.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a failure should stop the test"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use lumen_migrate::commands::{self, DevOptions, DevOutcome, Project};
use lumen_schema::{SourceFile, check};
use sqlx::{AssertSqlSafe, PgPool};

const PRISMA: &str = "prisma@6.19.3";

const V1: &str = "datasource db {\n  provider = \"postgresql\"\n  url      = env(\"DATABASE_URL\")\n}\n\nmodel User {\n  id    Int     @id @default(autoincrement())\n  email String  @unique\n  name  String?\n  posts Post[]\n}\n\nmodel Post {\n  id       Int    @id @default(autoincrement())\n  title    String\n  author   User   @relation(fields: [authorId], references: [id])\n  authorId Int\n}\n";

fn v2() -> String {
    V1.replace("  name  String?\n", "  name  String?\n  bio   String?\n")
}

struct Env {
    admin: PgPool,
    base: String,
    dir: PathBuf,
}

impl Env {
    async fn new(tag: &str) -> Option<Self> {
        if std::env::var("LUMEN_TEST_PRISMA").ok().as_deref() != Some("1") {
            eprintln!("skipped: set LUMEN_TEST_PRISMA=1 to run against the Prisma CLI");
            return None;
        }
        let server = std::env::var("LUMEN_TEST_POSTGRES_URL").ok()?;
        let admin = PgPool::connect(&server).await.unwrap();
        let base = server
            .rsplit_once('/')
            .map_or(server.as_str(), |(b, _)| b)
            .to_owned();
        let dir = std::env::temp_dir().join(format!("lumen-prisma-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Some(Self { admin, base, dir })
    }

    async fn database(&self, name: &str) -> String {
        for sql in [
            format!("drop database if exists {name} with (force)"),
            format!("create database {name}"),
        ] {
            sqlx::raw_sql(AssertSqlSafe(sql))
                .execute(&self.admin)
                .await
                .unwrap();
        }
        format!("{}/{name}", self.base)
    }

    fn write_schema(&self, text: &str) {
        std::fs::write(self.dir.join("schema.prisma"), text).unwrap();
    }

    fn project(&self, text: &str, url: &str) -> Project {
        let (schema, d) = check(&SourceFile::new("schema.prisma", text));
        assert!(!d.has_errors());
        Project {
            schema,
            migrations_dir: self.dir.join("migrations"),
            database_url: url.to_owned(),
            shadow_database_url: None,
        }
    }

    /// Run the Prisma CLI; returns (success, output).
    fn prisma(&self, url: &str, args: &[&str]) -> (bool, String) {
        let out = Command::new("npx")
            .args(["--yes", PRISMA])
            .args(args)
            .args(
                if args.first() == Some(&"migrate") && args.get(1) == Some(&"diff") {
                    &[][..]
                } else {
                    &["--schema", "schema.prisma"][..]
                },
            )
            .env("DATABASE_URL", url)
            .current_dir(&self.dir)
            .output()
            .expect("npx is installed");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }
}

fn folders(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.join("migrations"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn ac_002_a_prisma_project_continues_with_lumen() {
    let Some(env) = Env::new("p2l").await else {
        return;
    };
    let url = env.database("lumen_interop_p2l").await;
    env.write_schema(V1);
    let (ok, out) = env.prisma(
        &url,
        &["migrate", "dev", "--name", "init", "--skip-generate"],
    );
    assert!(ok, "{out}");

    // lumen reads Prisma's history and calculates the same database: nothing to do, no drift
    let up = commands::dev(
        &env.project(V1, &url),
        &DevOptions {
            name: "x".into(),
            ..DevOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        up,
        DevOutcome::UpToDate { applied: vec![] },
        "lumen agrees with Prisma's database"
    );

    let DevOutcome::Created { folder, .. } = commands::dev(
        &env.project(&v2(), &url),
        &DevOptions {
            name: "add bio".into(),
            ..DevOptions::default()
        },
    )
    .await
    .unwrap() else {
        panic!("lumen writes the next migration")
    };
    env.write_schema(&v2());
    let (ok, out) = env.prisma(&url, &["migrate", "status"]);
    assert!(
        ok && out.contains("Database schema is up to date"),
        "Prisma accepts lumen's migration {folder}:\n{out}"
    );
    let shadow = env.database("lumen_interop_p2l_shadow").await;
    let (ok, out) = env.prisma(
        &url,
        &[
            "migrate",
            "diff",
            "--from-migrations",
            "migrations",
            "--to-schema-datamodel",
            "schema.prisma",
            "--shadow-database-url",
            &shadow,
            "--exit-code",
        ],
    );
    assert!(
        ok,
        "Prisma finds no difference between the migrations and the schema:\n{out}"
    );
}

#[tokio::test]
async fn ac_002_a_lumen_project_continues_with_prisma() {
    let Some(env) = Env::new("l2p").await else {
        return;
    };
    let url = env.database("lumen_interop_l2p").await;
    env.write_schema(V1);
    let made = commands::dev(
        &env.project(V1, &url),
        &DevOptions {
            name: "init".into(),
            ..DevOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(matches!(made, DevOutcome::Created { ran: true, .. }));
    let (ok, out) = env.prisma(&url, &["migrate", "status"]);
    assert!(ok && out.contains("Database schema is up to date"), "{out}");
    let before = folders(&env.dir);
    let (ok, out) = env.prisma(
        &url,
        &["migrate", "dev", "--name", "check", "--skip-generate"],
    );
    assert!(ok, "{out}");
    assert_eq!(
        folders(&env.dir),
        before,
        "Prisma finds nothing to add to lumen's migration:\n{out}"
    );
}
