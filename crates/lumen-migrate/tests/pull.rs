//! SPEC-CLI-001 FR-005, AC-003: `db pull` on Postgres gives back the schema a database was built
//! from. For lumen's own schemas and every Postgres schema of the corpus in
//! `lumen-schema/tests/corpus/`: build the database (views too), pull into the same file and the
//! file is unchanged after `lumen format`; pull with `--force` and the new schema describes the
//! database exactly; pull again over that and nothing changes. Runs when
//! `LUMEN_TEST_POSTGRES_URL` is set.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a failure should stop the test"
)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use lumen_migrate::postgres::pull::{Database, pull};
use lumen_migrate::postgres::script;
use lumen_migrate::postgres::types::{column_type, type_name};
use lumen_migrate::{Renames, Shape, calculate, diff};
use lumen_schema::ast::Arity;
use lumen_schema::checked::{FieldKind, ModelKind, Provider, Schema};
use lumen_schema::{SourceFile, check};
use sqlx::{AssertSqlSafe, PgPool};

/// Schemas that need something a plain Postgres image does not have.
const NEEDS: &[&str] = &["prisma-examples/orm-postgis-express.prisma"];

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
            "lumen_pull_{}_{}",
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

    async fn run(&self, sql: &str) -> Result<(), String> {
        sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|e| format!("{e}\n---\n{sql}"))
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

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The schemas (namespaces) the datasource lists, else `public`.
fn namespaces(schema: &Schema) -> Vec<String> {
    match &schema.datasource {
        Some(ds) if !ds.schemas.is_empty() => ds.schemas.clone(),
        _ => vec!["public".to_owned()],
    }
}

/// `CREATE VIEW` for each view block, with a typed NULL for every field, so the view's columns
/// have exactly the types the fields ask for.
fn views_sql(schema: &Schema) -> String {
    let mut out = String::new();
    for m in schema.models.iter().filter(|m| m.kind == ModelKind::View) {
        let columns: Vec<String> = m
            .fields
            .iter()
            .filter_map(|f| {
                let base = match &f.kind {
                    FieldKind::Scalar(s) => column_type(*s, f.native_type.as_ref()),
                    FieldKind::Enum(id) => {
                        let e = &schema.enums[id.0];
                        let ns = e.schema.as_deref().unwrap_or("public");
                        format!(
                            "{}.{}",
                            quoted(ns),
                            type_name(e.mapped_name.as_deref().unwrap_or(&e.name))
                        )
                    }
                    FieldKind::Unsupported(raw) => raw.clone(),
                    _ => return None,
                };
                let ty = if f.arity == Arity::List {
                    format!("{base}[]")
                } else {
                    base
                };
                Some(format!("NULL::{ty} AS {}", quoted(f.database_name())))
            })
            .collect();
        let ns = m.schema.as_deref().unwrap_or("public");
        let _ = writeln!(
            out,
            "CREATE VIEW {}.{} AS SELECT {};",
            quoted(ns),
            quoted(m.database_name()),
            columns.join(", ")
        );
    }
    out
}

/// A unified-style list of the lines that differ, for failure messages.
fn line_diff(want: &str, got: &str) -> String {
    let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
    let mut out = String::new();
    for i in 0..w.len().max(g.len()) {
        match (w.get(i), g.get(i)) {
            (Some(a), Some(b)) if a == b => {}
            (a, b) => {
                let _ = writeln!(
                    out,
                    "  line {}:\n    want: {}\n    got:  {}",
                    i + 1,
                    a.unwrap_or(&"<none>"),
                    b.unwrap_or(&"<none>")
                );
                if out.lines().count() > 30 {
                    break;
                }
            }
        }
    }
    out
}

/// AC-003 for one schema; the problems found, if any.
async fn round_trip(url: &str, name: &str, text: &str) -> Result<(), String> {
    let (formatted, _) = lumen_schema::format(&SourceFile::new(name, text));
    let (schema, d) = check(&SourceFile::new(name, formatted.clone()));
    assert!(!d.has_errors(), "{name}");
    let shape = calculate(&schema);
    let db = Db::fresh(url).await;
    let result = async {
        let sql = script(&diff(&Shape::default(), &shape, &Renames::default()), &[]);
        db.run(&sql).await?;
        db.run(&views_sql(&schema)).await?;
        let read = Database::read(&db.pool, &namespaces(&schema))
            .await
            .map_err(|e| e.to_string())?;
        let file = SourceFile::new(name, formatted.clone());
        let again = pull(&read, &file, false).map_err(|e| e.message)?;
        if again.text != formatted {
            return Err(format!(
                "a second pull changed the file:\n{}",
                line_diff(&formatted, &again.text)
            ));
        }
        if !again.drift.is_empty() {
            return Err(format!("drift after a second pull:\n{}", again.drift));
        }
        let forced = pull(&read, &file, true).map_err(|e| e.message)?;
        if !forced.drift.is_empty() {
            return Err(format!(
                "drift after --force:\n{}\n---\n{}",
                forced.drift, forced.text
            ));
        }
        let over = SourceFile::new(name, forced.text.clone());
        let twice = pull(&read, &over, false).map_err(|e| e.message)?;
        if twice.text != forced.text {
            return Err(format!(
                "pulling over a forced pull changed it:\n{}",
                line_diff(&forced.text, &twice.text)
            ));
        }
        Ok(())
    }
    .await;
    db.drop().await;
    result
}

fn corpus() -> Vec<(String, String)> {
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
        let (schema, _) = check(&SourceFile::new(&name, &text));
        let postgres = schema
            .datasource
            .as_ref()
            .is_some_and(|ds| ds.provider == Provider::Postgres);
        if postgres && !NEEDS.contains(&name.as_str()) {
            out.push((name, text));
        }
    }
    out
}

/// Several relations between two models, a self-relation in both forms, one-to-one, a named and
/// an unnamed many-to-many, a composite key and foreign key, and mapped names.
pub const RELATIONS: &str = r#"datasource db {
  provider = "postgresql"
  url      = env("DATABASE_URL")
}

model Person {
  id        Int      @id @default(autoincrement())
  name      String
  manager   Person?  @relation("PersonManager", fields: [managerId], references: [id])
  managerId Int?
  reports   Person[] @relation("PersonManager")
  mentor    Person?  @relation("Mentoring", fields: [mentorId], references: [id])
  mentorId  Int?     @unique
  mentee    Person?  @relation("Mentoring")
  bought    Order[]  @relation("OrderBuyer")
  sold      Order[]  @relation("OrderSeller")
  groups    Group[]
  friends   Person[] @relation("Friends")
  friendOf  Person[] @relation("Friends")
  badges    Badge[]  @relation("Awards")
}

model Order {
  id       Int     @id @default(autoincrement())
  buyer    Person  @relation("OrderBuyer", fields: [buyerId], references: [id], onDelete: Cascade)
  buyerId  Int
  seller   Person  @relation("OrderSeller", fields: [sellerId], references: [id])
  sellerId Int     @map("seller_id")
  lines    Line[]
  invoice  Invoice?

  @@index([buyerId, sellerId])
  @@map("orders")
}

model Line {
  order    Order  @relation(fields: [orderId], references: [id])
  orderId  Int
  position Int
  note     String @default("")
  refunds  Refund[]

  @@id([orderId, position])
}

/// Points at a line by its two-column key.
model Refund {
  id           Int  @id @default(autoincrement())
  line         Line @relation(fields: [lineOrderId, linePosition], references: [orderId, position], map: "refund_line")
  lineOrderId  Int
  linePosition Int
}

model Invoice {
  id      String @id @default(dbgenerated("gen_random_uuid()")) @db.Uuid
  order   Order  @relation(fields: [orderId], references: [id])
  orderId Int    @unique
}

model Group {
  slug    String   @id
  members Person[]
}

model Badge {
  code    String   @id @db.VarChar(16)
  holders Person[] @relation("Awards")
}
"#;

#[tokio::test]
async fn ac_003_lumen_schemas_come_back_as_written() {
    let Ok(url) = std::env::var("LUMEN_TEST_POSTGRES_URL") else {
        eprintln!("skipped: LUMEN_TEST_POSTGRES_URL is not set");
        return;
    };
    for (name, text) in [
        (
            "blog.prisma",
            include_str!("../../lumen-schema/tests/corpus/blog.prisma"),
        ),
        (
            "shop.prisma",
            include_str!("../../lumen-schema/tests/corpus/shop.prisma"),
        ),
        ("relations.prisma", RELATIONS),
    ] {
        if let Err(e) = round_trip(&url, name, text).await {
            panic!("{name}: {e}");
        }
    }
}

#[tokio::test]
async fn ac_003_every_postgres_corpus_schema_comes_back() {
    let Ok(url) = std::env::var("LUMEN_TEST_POSTGRES_URL") else {
        eprintln!("skipped: LUMEN_TEST_POSTGRES_URL is not set");
        return;
    };
    let corpus = corpus();
    assert!(corpus.len() >= 50, "{} Postgres schemas", corpus.len());
    let mut failures = Vec::new();
    for (name, text) in &corpus {
        if let Err(e) = round_trip(&url, name, text).await {
            failures.push(format!("{name}: {e}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} schemas did not come back:\n\n{}",
        failures.len(),
        corpus.len(),
        failures.join("\n\n")
    );
}
