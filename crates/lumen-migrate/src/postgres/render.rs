//! Migration steps as Postgres SQL, with a `-- Step` comment above each statement so a migration
//! file reads as a list of what it does.

use std::fmt::Write as _;

use super::types::{string_literal, type_name};
use crate::diff::{ColumnChange, ColumnDef, Step, TableChange, TableRef};
use crate::shape::{Default, ForeignKey, Index, Sort};

/// `"name"` with quotes doubled.
#[must_use]
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `"table"`, or `"schema"."table"` outside `public`.
fn qualified(t: &TableRef) -> String {
    if t.schema == "public" {
        ident(&t.name)
    } else {
        format!("{}.{}", ident(&t.schema), ident(&t.name))
    }
}

fn type_ref(schema: &str, name: &str) -> String {
    if schema == "public" {
        type_name(name)
    } else {
        format!("{}.{}", ident(schema), type_name(name))
    }
}

fn default_sql(d: &Default) -> Option<String> {
    match d {
        Default::Sequence => None,
        Default::Now => Some("CURRENT_TIMESTAMP".to_owned()),
        Default::Literal(s) | Default::Expression(s) => Some(s.clone()),
    }
}

/// `SERIAL` for an integer column with a sequence default, as Prisma writes it.
fn column_type(c: &ColumnDef) -> String {
    if c.default == Some(Default::Sequence) {
        match c.ty.as_str() {
            "smallint" => return "SMALLSERIAL".to_owned(),
            "integer" => return "SERIAL".to_owned(),
            "bigint" => return "BIGSERIAL".to_owned(),
            _ => {}
        }
    }
    upper_type(&c.ty)
}

/// `character varying(320)` → `VARCHAR(320)` and the like; other types as written.
fn upper_type(ty: &str) -> String {
    let (base, array) = ty.strip_suffix("[]").map_or((ty, ""), |b| (b, "[]"));
    let mapped = if let Some(rest) = base.strip_prefix("character varying") {
        format!("VARCHAR{rest}")
    } else if let Some(rest) = base.strip_prefix("timestamp") {
        let (p, zone) = rest.split_once(' ').unwrap_or((rest, ""));
        if zone == "with time zone" {
            format!("TIMESTAMPTZ{p}")
        } else {
            format!("TIMESTAMP{p}")
        }
    } else if let Some(rest) = base.strip_prefix("numeric") {
        format!("DECIMAL{rest}")
    } else {
        match base {
            "text" => "TEXT".to_owned(),
            "integer" => "INTEGER".to_owned(),
            "bigint" => "BIGINT".to_owned(),
            "smallint" => "SMALLINT".to_owned(),
            "boolean" => "BOOLEAN".to_owned(),
            "double precision" => "DOUBLE PRECISION".to_owned(),
            "real" => "REAL".to_owned(),
            "jsonb" => "JSONB".to_owned(),
            "json" => "JSON".to_owned(),
            "bytea" => "BYTEA".to_owned(),
            "uuid" => "UUID".to_owned(),
            "date" => "DATE".to_owned(),
            other => other.to_owned(),
        }
    };
    format!("{mapped}{array}")
}

fn column_sql(c: &ColumnDef) -> String {
    let mut out = format!("{} {}", ident(&c.name), column_type(c));
    if !c.nullable {
        out.push_str(" NOT NULL");
    }
    if let Some(d) = c.default.as_ref().and_then(default_sql) {
        let _ = write!(out, " DEFAULT {d}");
    }
    out
}

fn index_sql(t: &TableRef, i: &Index) -> String {
    let cols: Vec<String> = i
        .columns
        .iter()
        .map(|c| match c.sort {
            Sort::Asc => ident(&c.name),
            Sort::Desc => format!("{} DESC", ident(&c.name)),
        })
        .collect();
    let method = i
        .method
        .as_ref()
        .map_or_else(String::new, |m| format!(" USING {}", m.to_uppercase()));
    let filter = i
        .predicate
        .as_ref()
        .map_or_else(String::new, |p| format!(" WHERE {p}"));
    format!(
        "CREATE {}INDEX {} ON {}{method}({}){filter};",
        if i.unique { "UNIQUE " } else { "" },
        ident(&i.name),
        qualified(t),
        cols.join(", ")
    )
}

fn list(names: &[String]) -> String {
    names.iter().map(|n| ident(n)).collect::<Vec<_>>().join(",")
}

fn foreign_key_sql(t: &TableRef, fk: &ForeignKey) -> String {
    let target = TableRef {
        schema: fk.referenced_schema.clone(),
        name: fk.referenced_table.clone(),
    };
    format!(
        "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {}({}) ON DELETE {} ON UPDATE {};",
        qualified(t),
        ident(&fk.name),
        list(&fk.columns),
        qualified(&target),
        list(&fk.referenced_columns),
        fk.on_delete.sql(),
        fk.on_update.sql()
    )
}

fn alter_column(t: &TableRef, c: &ColumnChange, out: &mut Vec<String>) {
    let table = qualified(t);
    let col = ident(&c.name);
    if let Some((_, to)) = &c.ty {
        out.push(format!(
            "ALTER COLUMN {col} SET DATA TYPE {}",
            upper_type(to)
        ));
    }
    if let Some(nullable) = c.nullable {
        out.push(format!(
            "ALTER COLUMN {col} {} NOT NULL",
            if nullable { "DROP" } else { "SET" }
        ));
    }
    match &c.default {
        Some(Some(Default::Sequence)) => {
            // a sequence cannot be added inside ALTER TABLE; done around it
            let seq = format!("{}_{}_seq", t.name, c.name);
            out.push(format!(
                "@BEFORE CREATE SEQUENCE {};\n@AFTER ALTER SEQUENCE {} OWNED BY {table}.{col};",
                ident(&seq),
                ident(&seq)
            ));
            out.push(format!(
                "ALTER COLUMN {col} SET DEFAULT nextval({})",
                string_literal(&ident(&seq))
            ));
        }
        Some(Some(d)) => {
            if let Some(sql) = default_sql(d) {
                out.push(format!("ALTER COLUMN {col} SET DEFAULT {sql}"));
            }
        }
        Some(None) => out.push(format!("ALTER COLUMN {col} DROP DEFAULT")),
        None => {}
    }
}

fn alter_table(t: &TableRef, changes: &[TableChange]) -> Vec<String> {
    let table = qualified(t);
    let mut statements = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    for change in changes {
        match change {
            TableChange::RenameColumn { from, to } => {
                // RENAME cannot be combined with other actions
                statements.push(format!(
                    "ALTER TABLE {table} RENAME COLUMN {} TO {};",
                    ident(from),
                    ident(to)
                ));
            }
            TableChange::RenamePrimaryKey { from, to } => statements.push(format!(
                "ALTER TABLE {table} RENAME CONSTRAINT {} TO {};",
                ident(from),
                ident(to)
            )),
            TableChange::DropPrimaryKey(name) => {
                parts.push(format!("DROP CONSTRAINT {}", ident(name)));
            }
            TableChange::DropColumn(name) => parts.push(format!("DROP COLUMN {}", ident(name))),
            TableChange::AddColumn(c) => parts.push(format!("ADD COLUMN {}", column_sql(c))),
            TableChange::RecreateColumn(c) => {
                parts.push(format!("DROP COLUMN {}", ident(&c.name)));
                parts.push(format!("ADD COLUMN {}", column_sql(c)));
            }
            TableChange::AlterColumn(c) => alter_column(t, c, &mut parts),
            TableChange::AddPrimaryKey(pk) => parts.push(format!(
                "ADD CONSTRAINT {} PRIMARY KEY ({})",
                ident(&pk.name),
                list(&pk.columns)
            )),
        }
    }
    let mut before = Vec::new();
    let mut after = Vec::new();
    parts.retain(|p| {
        if let Some(rest) = p.strip_prefix("@BEFORE ") {
            let (b, a) = rest.split_once("\n@AFTER ").unwrap_or((rest, ""));
            before.push(b.to_owned());
            after.push(a.to_owned());
            false
        } else {
            true
        }
    });
    statements.extend(before);
    if !parts.is_empty() {
        statements.push(format!("ALTER TABLE {table} {};", parts.join(",\n")));
    }
    statements.extend(after);
    statements
}

fn rewrite_enum(step: &Step) -> Vec<String> {
    let Step::RewriteEnum { to, columns } = step else {
        return Vec::new();
    };
    let old = type_ref(&to.schema, &to.name);
    let new_name = format!("{}_new", to.name);
    let new = type_ref(&to.schema, &new_name);
    let values: Vec<String> = to.values.iter().map(|v| string_literal(v)).collect();
    let mut out = vec![
        "BEGIN;".to_owned(),
        format!("CREATE TYPE {new} AS ENUM ({});", values.join(", ")),
    ];
    for (t, col, _) in columns {
        out.push(format!(
            "ALTER TABLE {} ALTER COLUMN {} DROP DEFAULT;",
            qualified(t),
            ident(col)
        ));
        out.push(format!(
            "ALTER TABLE {} ALTER COLUMN {c} TYPE {new} USING ({c}::text::{new});",
            qualified(t),
            c = ident(col)
        ));
    }
    out.push(format!(
        "ALTER TYPE {old} RENAME TO {};",
        type_name(&format!("{}_old", to.name))
    ));
    out.push(format!(
        "ALTER TYPE {new} RENAME TO {};",
        type_name(&to.name)
    ));
    out.push(format!(
        "DROP TYPE {};",
        type_ref(&to.schema, &format!("{}_old", to.name))
    ));
    for (t, col, default) in columns {
        if let Some(sql) = default.as_ref().and_then(default_sql) {
            out.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {sql};",
                qualified(t),
                ident(col)
            ));
        }
    }
    out.push("COMMIT;".to_owned());
    out
}

/// The name Prisma puts above a step.
fn label(step: &Step) -> &'static str {
    match step {
        Step::CreateSchema(_) => "CreateSchema",
        Step::CreateEnum(_) => "CreateEnum",
        Step::AddEnumValues(..) | Step::RewriteEnum { .. } => "AlterEnum",
        Step::DropForeignKey(..) => "DropForeignKey",
        Step::DropIndex(..) => "DropIndex",
        Step::RenameTable(..) | Step::AlterTable(..) => "AlterTable",
        Step::DropTable(_) => "DropTable",
        Step::DropEnum(_) => "DropEnum",
        Step::CreateTable(_) => "CreateTable",
        Step::CreateIndex(..) => "CreateIndex",
        Step::AddForeignKey(..) => "AddForeignKey",
        Step::RenameIndex(..) => "RenameIndex",
        Step::RenameForeignKey(..) => "RenameForeignKey",
    }
}

/// The SQL statements of one step.
#[must_use]
pub fn statements(step: &Step) -> Vec<String> {
    match step {
        Step::CreateSchema(s) => vec![format!("CREATE SCHEMA IF NOT EXISTS {};", ident(s))],
        Step::CreateEnum(e) => {
            let values: Vec<String> = e.values.iter().map(|v| string_literal(v)).collect();
            vec![format!(
                "CREATE TYPE {} AS ENUM ({});",
                type_ref(&e.schema, &e.name),
                values.join(", ")
            )]
        }
        Step::AddEnumValues(e, values) => values
            .iter()
            .map(|v| {
                format!(
                    "ALTER TYPE {} ADD VALUE {};",
                    type_ref(&e.schema, &e.name),
                    string_literal(v)
                )
            })
            .collect(),
        Step::RewriteEnum { .. } => rewrite_enum(step),
        Step::DropForeignKey(t, name) => {
            vec![format!(
                "ALTER TABLE {} DROP CONSTRAINT {};",
                qualified(t),
                ident(name)
            )]
        }
        Step::DropIndex(t, name) => {
            let index = TableRef {
                schema: t.schema.clone(),
                name: name.clone(),
            };
            vec![format!("DROP INDEX {};", qualified(&index))]
        }
        Step::RenameTable(t, to) => vec![format!(
            "ALTER TABLE {} RENAME TO {};",
            qualified(t),
            ident(to)
        )],
        Step::AlterTable(t, changes) => alter_table(t, changes),
        Step::DropTable(t) => vec![format!("DROP TABLE {};", qualified(t))],
        Step::DropEnum(e) => vec![format!("DROP TYPE {};", type_ref(&e.schema, &e.name))],
        Step::CreateTable(t) => {
            let mut lines: Vec<String> = t
                .columns
                .iter()
                .map(|c| format!("    {}", column_sql(c)))
                .collect();
            if let Some(pk) = &t.primary_key {
                lines.push(format!(
                    "\n    CONSTRAINT {} PRIMARY KEY ({})",
                    ident(&pk.name),
                    list(&pk.columns)
                ));
            }
            vec![format!(
                "CREATE TABLE {} (\n{}\n);",
                qualified(&t.table),
                lines.join(",\n")
            )]
        }
        Step::CreateIndex(t, i) => vec![index_sql(t, i)],
        Step::AddForeignKey(t, fk) => vec![foreign_key_sql(t, fk)],
        Step::RenameIndex(t, from, to) => {
            let index = TableRef {
                schema: t.schema.clone(),
                name: from.clone(),
            };
            vec![format!(
                "ALTER INDEX {} RENAME TO {};",
                qualified(&index),
                ident(to)
            )]
        }
        Step::RenameForeignKey(t, from, to) => vec![format!(
            "ALTER TABLE {} RENAME CONSTRAINT {} TO {};",
            qualified(t),
            ident(from),
            ident(to)
        )],
    }
}

/// A whole migration script, with `warnings` as a comment at the top.
#[must_use]
pub fn script(steps: &[Step], warnings: &[String]) -> String {
    let mut out = String::new();
    if !warnings.is_empty() {
        out.push_str("/*\n  Warnings:\n\n");
        for w in warnings {
            let _ = writeln!(out, "  - {w}");
        }
        out.push_str("\n*/\n");
    }
    for step in steps {
        let _ = writeln!(out, "-- {}", label(step));
        for s in statements(step) {
            out.push_str(&s);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}
