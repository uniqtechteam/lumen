//! SPEC-SCHEMA-001 AC-001, SPEC-MIG-001 FR-001: the Postgres schemas of the corpus in
//! `lumen-schema/tests/corpus/` give a migration from nothing. With `LUMEN_TEST_POSTGRES_URL` set,
//! each migration runs on a fresh database, the database described afterwards equals the shape
//! the schema asks for, and a second diff is empty. With `LUMEN_TEST_PRISMA=1` as well, Prisma
//! 6.19.3 writes the migration instead (`npx`), and lumen finds nothing to change in the result.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a failure should stop the test"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use lumen_migrate::postgres::{describe, script};
use lumen_migrate::{Renames, Shape, Step, calculate, diff};
use lumen_schema::checked::{Provider, Schema, StringOrEnv};
use lumen_schema::{SourceFile, check};
use sqlx::{AssertSqlSafe, PgPool};

const PRISMA: &str = "prisma@6.19.3";

/// Schemas that need something a plain Postgres image does not have.
const NEEDS: &[(&str, &str)] = &[("prisma-examples/orm-postgis-express.prisma", "PostGIS")];

struct Entry {
    /// The path relative to the corpus, with `/`.
    name: String,
    path: PathBuf,
    schema: Schema,
}

/// Every Postgres schema of the corpus, checked.
fn postgres_corpus() -> Vec<Entry> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "prisma") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lumen-schema/tests/corpus");
    let mut paths = Vec::new();
    walk(&root, &mut paths);
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        let name = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");
        let file = SourceFile::new(&name, &text);
        let (schema, d) = check(&file);
        assert!(!d.has_errors(), "{}", d.render(&file));
        let postgres = schema
            .datasource
            .as_ref()
            .is_some_and(|ds| ds.provider == Provider::Postgres);
        let needs = NEEDS.iter().find(|(n, _)| *n == name);
        if let Some((_, what)) = needs {
            eprintln!("skipped {name}: needs {what}");
        } else if postgres {
            out.push(Entry { name, path, schema });
        }
    }
    out
}

fn from_nothing(shape: &Shape) -> Vec<Step> {
    diff(&Shape::default(), shape, &Renames::default())
}

/// A fresh, empty database on the test server.
struct Db {
    admin: PgPool,
    pool: PgPool,
    name: String,
}

impl Db {
    async fn fresh(url: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let admin = PgPool::connect(url).await.unwrap();
        let name = format!(
            "lumen_corpus_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        sqlx::raw_sql(AssertSqlSafe(format!("create database {name}")))
            .execute(&admin)
            .await
            .unwrap();
        let base = url.rsplit_once('/').map_or(url, |(b, _)| b);
        let pool = PgPool::connect(&format!("{base}/{name}")).await.unwrap();
        Self { admin, pool, name }
    }

    async fn run(&self, what: &str, sql: &str) {
        sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
            .execute(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("{what}: {e}\n---\n{sql}"));
    }

    /// The database as lumen describes it, in the schemas `shape` uses.
    async fn shape_like(&self, shape: &Shape) -> Shape {
        let mut schemas: Vec<String> = shape.tables.iter().map(|t| t.schema.clone()).collect();
        schemas.extend(shape.enums.iter().map(|e| e.schema.clone()));
        schemas.sort();
        schemas.dedup();
        describe(&self.pool, &schemas).await.unwrap()
    }

    async fn drop(self) {
        self.pool.close().await;
        sqlx::raw_sql(AssertSqlSafe(format!(
            "drop database if exists {} with (force)",
            self.name
        )))
        .execute(&self.admin)
        .await
        .unwrap();
    }
}

/// The database holds exactly what the schema asks for.
async fn assert_matches(db: &Db, name: &str, shape: &Shape) {
    let described = db.shape_like(shape).await;
    assert_eq!(
        diff(&described, shape, &Renames::default()),
        Vec::<Step>::new(),
        "{name}: nothing left to do"
    );
    assert_eq!(described, *shape, "{name}: the database is the schema");
}

#[test]
fn ac_001_every_postgres_corpus_schema_gives_a_migration() {
    let corpus = postgres_corpus();
    assert!(corpus.len() >= 50, "{} Postgres schemas", corpus.len());
    for Entry { name, schema, .. } in corpus {
        let shape = calculate(&schema);
        let sql = script(&from_nothing(&shape), &[]);
        let creates = sql.matches("CREATE TABLE ").count();
        assert_eq!(creates, shape.tables.len(), "{name}\n{sql}");
    }
}

#[tokio::test]
async fn ac_001_every_postgres_corpus_schema_builds_a_database() {
    let Ok(url) = std::env::var("LUMEN_TEST_POSTGRES_URL") else {
        eprintln!("skipped: LUMEN_TEST_POSTGRES_URL is not set");
        return;
    };
    for Entry { name, schema, .. } in postgres_corpus() {
        let db = Db::fresh(&url).await;
        let shape = calculate(&schema);
        db.run(&name, &script(&from_nothing(&shape), &[])).await;
        assert_matches(&db, &name, &shape).await;
        db.drop().await;
    }
}

/// `prisma migrate diff --from-empty` for the schema: the SQL Prisma would write.
fn prisma_sql(entry: &Entry) -> String {
    let mut cmd = Command::new(if cfg!(windows) { "npx.cmd" } else { "npx" });
    cmd.args(["--yes", PRISMA, "migrate", "diff", "--from-empty"])
        .arg("--to-schema-datamodel")
        .arg(&entry.path)
        .arg("--script");
    // Prisma wants every `env(..)` of the datasource set; nothing connects to them
    let ds = entry.schema.datasource.as_ref().unwrap();
    for value in [&ds.url, &ds.direct_url, &ds.shadow_database_url]
        .into_iter()
        .flatten()
    {
        if let StringOrEnv::Env(var) = value {
            cmd.env(
                var,
                format!("postgresql://localhost:5432/{}", var.to_lowercase()),
            );
        }
    }
    let out = cmd.output().expect("npx is installed");
    assert!(
        out.status.success(),
        "{}: {}",
        entry.name,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[tokio::test]
async fn ac_001_lumen_agrees_with_the_database_prisma_builds() {
    if std::env::var("LUMEN_TEST_PRISMA").ok().as_deref() != Some("1") {
        eprintln!("skipped: set LUMEN_TEST_PRISMA=1 to run against the Prisma CLI");
        return;
    }
    let Ok(url) = std::env::var("LUMEN_TEST_POSTGRES_URL") else {
        eprintln!("skipped: LUMEN_TEST_POSTGRES_URL is not set");
        return;
    };
    // the vendored schemas (lumen's own use partial indexes, which Prisma does not have), and
    // only those with `url` in the datasource: Prisma 6 needs it, Prisma 7 schemas leave it out
    let corpus: Vec<Entry> = postgres_corpus()
        .into_iter()
        .filter(|e| e.name.contains('/'))
        .filter(|e| e.schema.datasource.as_ref().unwrap().url.is_some())
        .collect();
    assert!(corpus.len() >= 8, "{} schemas for Prisma 6", corpus.len());
    for entry in corpus {
        let sql = prisma_sql(&entry);
        let db = Db::fresh(&url).await;
        db.run(&entry.name, &sql).await;
        assert_matches(&db, &entry.name, &calculate(&entry.schema)).await;
        db.drop().await;
    }
}
