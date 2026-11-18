//! `db pull` for Postgres: read the database and write the schema that describes it
//! (SPEC-CLI-001 FR-005). A second pull keeps what the file says where the database agrees: names,
//! order, comments and attributes the database cannot know.
//!
//! The new schema is checked before it is returned, and the tables it asks for are compared with
//! the database, so whatever `lumen migrate dev` would still change is reported.

mod build;
mod existing;
mod names;
mod render;
mod types;

use lumen_schema::ast::Top;
use lumen_schema::{SourceFile, check, parse};
use sqlx::{PgPool, Row};

use crate::MigrateError;
use crate::calculate::calculate;
use crate::diff::{Renames, diff};
use crate::postgres::describe;
use crate::postgres::render::script;
use crate::shape::Shape;

pub use names::{field_name, type_name};

/// A view and its columns (name, type as `format_type` prints it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// The schema.
    pub schema: String,
    /// The name.
    pub name: String,
    /// The columns, in order.
    pub columns: Vec<(String, String)>,
}

/// A check constraint, which the schema cannot say; it becomes a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The table's schema.
    pub schema: String,
    /// The table.
    pub table: String,
    /// The constraint name.
    pub name: String,
    /// `CHECK (price > 0)`, as Postgres prints it.
    pub definition: String,
}

/// Everything `db pull` reads from a database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Database {
    /// The schemas (namespaces) read.
    pub schemas: Vec<String>,
    /// Tables, keys, indexes, foreign keys and enums.
    pub shape: Shape,
    /// Views.
    pub views: Vec<View>,
    /// Check constraints.
    pub checks: Vec<Check>,
}

const VIEWS: &str = "
select n.nspname as schema, c.relname as name, a.attname as column,
       format_type(a.atttypid, a.atttypmod) as ty
from pg_class c
join pg_namespace n on n.oid = c.relnamespace
join pg_attribute a on a.attrelid = c.oid and a.attnum > 0 and not a.attisdropped
where c.relkind = 'v' and n.nspname = any($1)
order by 1, 2, a.attnum";

const CHECKS: &str = "
select n.nspname as schema, c.relname as table, con.conname as name,
       pg_get_constraintdef(con.oid, true) as definition
from pg_constraint con
join pg_class c on c.oid = con.conrelid
join pg_namespace n on n.oid = c.relnamespace
where con.contype = 'c' and n.nspname = any($1)
order by 1, 2, 3";

impl Database {
    /// Read the database behind `pool`, in `schemas`.
    ///
    /// # Errors
    /// When a catalog query fails.
    pub async fn read(pool: &PgPool, schemas: &[String]) -> Result<Self, sqlx::Error> {
        let shape = describe(pool, schemas).await?;
        let mut views: Vec<View> = Vec::new();
        for row in sqlx::query(VIEWS).bind(schemas).fetch_all(pool).await? {
            let (schema, name): (String, String) = (row.try_get("schema")?, row.try_get("name")?);
            let column = (row.try_get("column")?, row.try_get("ty")?);
            match views.last_mut() {
                Some(v) if v.schema == schema && v.name == name => v.columns.push(column),
                _ => views.push(View {
                    schema,
                    name,
                    columns: vec![column],
                }),
            }
        }
        let mut checks = Vec::new();
        for row in sqlx::query(CHECKS).bind(schemas).fetch_all(pool).await? {
            checks.push(Check {
                schema: row.try_get("schema")?,
                table: row.try_get("table")?,
                name: row.try_get("name")?,
                definition: row.try_get("definition")?,
            });
        }
        Ok(Self {
            schemas: schemas.to_vec(),
            shape,
            views,
            checks,
        })
    }
}

/// What `db pull` wrote.
#[derive(Debug, Clone)]
pub struct Pulled {
    /// The new schema file, formatted.
    pub text: String,
    /// How many models, views and enums it has.
    pub counts: (usize, usize, usize),
    /// What the schema cannot say (also written into the file as `// lumen:` notes).
    pub notes: Vec<String>,
    /// The SQL `lumen migrate dev` would still run to make the database match the new schema;
    /// empty when the schema describes the database exactly.
    pub drift: String,
}

/// The schemas (namespaces) the schema file's datasource lists, else `public`.
#[must_use]
pub fn schemas_of(file: &SourceFile) -> Vec<String> {
    let (schema, _) = check(file);
    match schema.datasource {
        Some(ds) if !ds.schemas.is_empty() => ds.schemas,
        _ => vec!["public".to_owned()],
    }
}

/// The schema for `db`, written over the schema file `file`: its datasource and generators are
/// kept as written; with `force`, nothing else of it is.
///
/// # Errors
/// When the file has errors, or (a bug) when the new schema does not check.
pub fn pull(db: &Database, file: &SourceFile, force: bool) -> Result<Pulled, MigrateError> {
    let (ast, _) = parse(file);
    let (schema, diagnostics) = check(file);
    if diagnostics.has_errors() && !force {
        return Err(MigrateError::new(format!(
            "{}\nfix the schema first, or pass --force to write it again from the database",
            diagnostics.render(file).trim_end()
        )));
    }
    let datasource = schema
        .datasource
        .as_ref()
        .map_or_else(|| "db".to_owned(), |d| d.name.clone());
    let multi_schema = schema
        .datasource
        .as_ref()
        .is_some_and(|d| !d.schemas.is_empty());
    let old = existing::Existing::new(&file.text, &ast, &schema);
    let built = build::build(db, (!force).then_some(&old), &datasource, multi_schema);
    // the datasource and generators where they were, then the blocks
    let mut parts: Vec<(usize, String)> = Vec::new();
    for (i, top) in ast.tops.iter().enumerate() {
        if let Top::Config(_) = top {
            parts.push((i, format!("{}{}\n", old.above_top(i), old.top_text(i))));
        }
    }
    let mut notes = built.notes.clone();
    let mut counts = (0, 0, 0);
    for (k, b) in built.blocks.iter().enumerate() {
        match b.keyword {
            "model" => counts.0 += 1,
            "view" => counts.1 += 1,
            _ => counts.2 += 1,
        }
        notes.extend(b.notes.iter().map(|n| format!("{}: {n}", b.name)));
        parts.push((
            b.old.unwrap_or(usize::MAX - built.blocks.len() + k),
            render::block(b),
        ));
    }
    parts.sort_by_key(|(i, _)| *i);
    let mut text = parts
        .into_iter()
        .map(|(_, t)| t)
        .collect::<Vec<_>>()
        .join("\n");
    for c in &ast.trailing_comments {
        text.push_str(&format!("\n// {c}\n"));
    }
    let new = SourceFile::new(file.name.clone(), text);
    let (formatted, _) = lumen_schema::format(&new);
    let new = SourceFile::new(file.name.clone(), formatted);
    let (checked, diagnostics) = check(&new);
    if diagnostics.has_errors() {
        return Err(MigrateError::new(format!(
            "the pulled schema does not check, which is a bug in `lumen db pull`; please report it with this output:\n{}\n---\n{}",
            diagnostics.render(&new).trim_end(),
            new.text
        )));
    }
    let wanted = calculate(&checked);
    let steps = diff(&db.shape, &wanted, &Renames::default());
    let drift = if steps.is_empty() {
        String::new()
    } else {
        script(&steps, &[])
    };
    Ok(Pulled {
        text: new.text,
        counts,
        notes,
        drift,
    })
}
