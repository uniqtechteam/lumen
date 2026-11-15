//! The steps that turn one shape into another. Tables and columns are paired by name, or by a
//! rename hint (`// lumen: renamed from old`) so a rename is a rename and not drop + add. The order
//! of the [`Step`] variants is the order the steps run in.

use crate::shape::{Column, Default, EnumType, ForeignKey, Index, PrimaryKey, Shape, Table};

/// A table, by schema and name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TableRef {
    /// The schema.
    pub schema: String,
    /// The name.
    pub name: String,
}

impl TableRef {
    fn of(t: &Table) -> Self {
        Self {
            schema: t.schema.clone(),
            name: t.name.clone(),
        }
    }
}

/// What changes about one column.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ColumnChange {
    /// The column (its new name).
    pub name: String,
    /// The old and new type, when the type changes in place.
    pub ty: Option<(String, String)>,
    /// The new nullability, when it changes.
    pub nullable: Option<bool>,
    /// The new default (`Some(None)` drops it), when it changes.
    pub default: Option<Option<Default>>,
}

/// One change inside `ALTER TABLE`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TableChange {
    /// `RENAME COLUMN`.
    RenameColumn {
        /// Old name.
        from: String,
        /// New name.
        to: String,
    },
    /// `RENAME CONSTRAINT` of the primary key.
    RenamePrimaryKey {
        /// Old name.
        from: String,
        /// New name.
        to: String,
    },
    /// `DROP CONSTRAINT` of the primary key.
    DropPrimaryKey(String),
    /// `DROP COLUMN`.
    DropColumn(String),
    /// `ADD COLUMN`.
    AddColumn(ColumnDef),
    /// Type, nullability or default.
    AlterColumn(ColumnChange),
    /// A type change that cannot be cast: drop and add the column again.
    RecreateColumn(ColumnDef),
    /// `ADD CONSTRAINT .. PRIMARY KEY`.
    AddPrimaryKey(PrimaryKeyDef),
}

/// A column, orderable for the step list.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ColumnDef {
    /// The name.
    pub name: String,
    /// The type.
    pub ty: String,
    /// Whether NULL is allowed.
    pub nullable: bool,
    /// The default.
    pub default: Option<Default>,
}

impl From<&Column> for ColumnDef {
    fn from(c: &Column) -> Self {
        Self {
            name: c.name.clone(),
            ty: c.ty.clone(),
            nullable: c.nullable,
            default: c.default.clone(),
        }
    }
}

/// A primary key, orderable for the step list.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PrimaryKeyDef {
    /// The constraint name.
    pub name: String,
    /// The columns.
    pub columns: Vec<String>,
}

impl From<&PrimaryKey> for PrimaryKeyDef {
    fn from(p: &PrimaryKey) -> Self {
        Self {
            name: p.name.clone(),
            columns: p.columns.clone(),
        }
    }
}

/// One migration step. The declaration order is the order they run in: types first, then
/// removing what points at things, changing tables, removing tables, creating tables, and the
/// foreign keys last so tables that point at each other work in one migration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// `CREATE SCHEMA`.
    CreateSchema(String),
    /// `CREATE TYPE .. AS ENUM`.
    CreateEnum(EnumType),
    /// `ALTER TYPE .. ADD VALUE`.
    AddEnumValues(TableRef, Vec<String>),
    /// `ALTER TABLE .. DROP CONSTRAINT` of a foreign key.
    DropForeignKey(TableRef, String),
    /// `DROP INDEX`.
    DropIndex(TableRef, String),
    /// `ALTER TABLE .. RENAME TO`.
    RenameTable(TableRef, String),
    /// `ALTER TABLE` with column and primary key changes.
    AlterTable(TableRef, Vec<TableChange>),
    /// `DROP TABLE`.
    DropTable(TableRef),
    /// Enum values removed: rebuild the type and move every column that uses it.
    RewriteEnum {
        /// The enum after the change.
        to: EnumType,
        /// Columns using it: (table, column, default).
        columns: Vec<(TableRef, String, Option<Default>)>,
    },
    /// `DROP TYPE`.
    DropEnum(TableRef),
    /// `CREATE TABLE`.
    CreateTable(TableDef),
    /// `CREATE INDEX`.
    CreateIndex(TableRef, Index),
    /// `ALTER TABLE .. ADD CONSTRAINT .. FOREIGN KEY`.
    AddForeignKey(TableRef, ForeignKey),
    /// `ALTER INDEX .. RENAME TO`: same index, new name (for example after a table rename).
    RenameIndex(TableRef, String, String),
    /// `ALTER TABLE .. RENAME CONSTRAINT`: same foreign key, new name.
    RenameForeignKey(TableRef, String, String),
}

/// A table to create, orderable for the step list.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TableDef {
    /// Schema and name.
    pub table: TableRef,
    /// The columns.
    pub columns: Vec<ColumnDef>,
    /// The primary key.
    pub primary_key: Option<PrimaryKeyDef>,
}

/// Renames written as hints in the schema, by new name.
#[derive(Debug, Clone, Default)]
pub struct Renames {
    /// (schema, old table, new table).
    pub tables: Vec<(String, String, String)>,
    /// (schema, table after rename, old column, new column).
    pub columns: Vec<(String, String, String, String)>,
}

/// Whether a column type change can run in place: `(castable, safe)`.
#[must_use]
pub fn cast(from: &str, to: &str) -> (bool, bool) {
    fn family(t: &str) -> &'static str {
        let t = t.trim_end_matches("[]");
        if t.starts_with("smallint") || t.starts_with("integer") || t.starts_with("bigint") {
            "int"
        } else if t.starts_with("numeric")
            || t.starts_with("real")
            || t.starts_with("double")
            || t.starts_with("money")
        {
            "number"
        } else if t.starts_with("text") || t.starts_with("character") || t.starts_with("citext") {
            "text"
        } else if t.starts_with("timestamp") || t.starts_with("date") || t.starts_with("time") {
            "time"
        } else {
            "other"
        }
    }
    let rank = |t: &str| {
        ["smallint", "integer", "bigint"]
            .iter()
            .position(|p| t.starts_with(p))
    };
    let to_text = to == "text" || to == "character varying";
    match (family(from), family(to)) {
        (_, "text") if to_text && from.ends_with("[]") == to.ends_with("[]") => (true, true),
        ("int", "int") => (true, rank(to) >= rank(from)),
        ("int", "number") => (true, true),
        ("number", "number" | "int") | ("text", "text") | ("time", "time") => (true, false),
        _ => (false, false),
    }
}

fn column_change(from: &Column, to: &Column) -> Option<TableChange> {
    if from.ty != to.ty && !cast(&from.ty, &to.ty).0 {
        return Some(TableChange::RecreateColumn(to.into()));
    }
    let change = ColumnChange {
        name: to.name.clone(),
        ty: (from.ty != to.ty).then(|| (from.ty.clone(), to.ty.clone())),
        nullable: (from.nullable != to.nullable).then_some(to.nullable),
        default: (from.default != to.default).then(|| to.default.clone()),
    };
    let changed = change.ty.is_some() || change.nullable.is_some() || change.default.is_some();
    changed.then_some(TableChange::AlterColumn(change))
}

fn table_changes(from: &Table, to: &Table, renames: &Renames, steps: &mut Vec<Step>) {
    let tref = TableRef::of(to);
    let mut changes = Vec::new();
    let old_name_of = |new: &str| {
        renames
            .columns
            .iter()
            .find(|(s, t, old, n)| {
                *s == to.schema && *t == to.name && n == new && from.column(old).is_some()
            })
            .map(|(_, _, old, _)| old.clone())
    };
    let mut pk_changed = from.primary_key != to.primary_key;
    if let (Some(old), Some(new)) = (&from.primary_key, &to.primary_key)
        && old.name != new.name
        && same_columns(&old.columns, &new.columns, from, to, renames)
    {
        changes.push(TableChange::RenamePrimaryKey {
            from: old.name.clone(),
            to: new.name.clone(),
        });
        pk_changed = false;
    }
    if pk_changed && let Some(pk) = &from.primary_key {
        changes.push(TableChange::DropPrimaryKey(pk.name.clone()));
    }
    let mut kept: Vec<&str> = Vec::new();
    for c in &to.columns {
        let old_name = old_name_of(&c.name);
        let old = old_name
            .as_deref()
            .map_or_else(|| from.column(&c.name), |o| from.column(o));
        match old {
            Some(old) => {
                kept.push(&old.name);
                if old.name != c.name {
                    changes.push(TableChange::RenameColumn {
                        from: old.name.clone(),
                        to: c.name.clone(),
                    });
                }
                if let Some(change) = column_change(old, c) {
                    changes.push(change);
                }
            }
            None => changes.push(TableChange::AddColumn(c.into())),
        }
    }
    for c in &from.columns {
        if !kept.contains(&c.name.as_str()) {
            changes.push(TableChange::DropColumn(c.name.clone()));
        }
    }
    if pk_changed && let Some(pk) = &to.primary_key {
        changes.push(TableChange::AddPrimaryKey(pk.into()));
    }
    let recreated: Vec<String> = changes
        .iter()
        .filter_map(|c| match c {
            TableChange::RecreateColumn(d) => Some(d.name.clone()),
            _ => None,
        })
        .collect();
    if !changes.is_empty() {
        steps.push(Step::AlterTable(tref.clone(), changes));
    }
    index_and_key_steps(from, to, renames, &recreated, steps);
}

/// Indexes and foreign keys: kept, renamed (same definition, new name), or dropped and created.
fn index_and_key_steps(
    from: &Table,
    to: &Table,
    renames: &Renames,
    recreated: &[String],
    steps: &mut Vec<Step>,
) {
    let tref = TableRef::of(to);
    let touches = |cols: &[String]| cols.iter().any(|c| recreated.contains(c));
    let mut renamed_indexes: Vec<&str> = Vec::new();
    for j in &to.indexes {
        let cols: Vec<String> = j.columns.iter().map(|c| c.name.clone()).collect();
        if from.indexes.iter().any(|i| i == j) && !touches(&cols) {
            continue;
        }
        let twin = from.indexes.iter().find(|i| {
            i.name != j.name
                && !to.indexes.iter().any(|k| k.name == i.name)
                && i.unique == j.unique
                && i.method == j.method
                && i.columns
                    .iter()
                    .map(|c| c.sort)
                    .eq(j.columns.iter().map(|c| c.sort))
                && same_columns(
                    &i.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                    &cols,
                    from,
                    to,
                    renames,
                )
        });
        match twin {
            Some(i) if !touches(&cols) => {
                renamed_indexes.push(&i.name);
                steps.push(Step::RenameIndex(
                    tref.clone(),
                    i.name.clone(),
                    j.name.clone(),
                ));
            }
            _ => steps.push(Step::CreateIndex(tref.clone(), j.clone())),
        }
    }
    for i in &from.indexes {
        let cols: Vec<String> = i.columns.iter().map(|c| c.name.clone()).collect();
        let kept = to.indexes.iter().any(|j| j == i) && !touches(&cols);
        if !kept && !renamed_indexes.contains(&i.name.as_str()) {
            steps.push(Step::DropIndex(TableRef::of(from), i.name.clone()));
        }
    }
    let mut renamed_keys: Vec<&str> = Vec::new();
    for fk in &to.foreign_keys {
        if from.foreign_keys.contains(fk) && !touches(&fk.columns) {
            continue;
        }
        let twin = from.foreign_keys.iter().find(|old| {
            old.name != fk.name
                && !to.foreign_keys.iter().any(|k| k.name == old.name)
                && old.referenced_columns == fk.referenced_columns
                && old.on_delete == fk.on_delete
                && old.on_update == fk.on_update
                && same_columns(&old.columns, &fk.columns, from, to, renames)
        });
        match twin {
            Some(old) if !touches(&fk.columns) => {
                renamed_keys.push(&old.name);
                steps.push(Step::RenameForeignKey(
                    tref.clone(),
                    old.name.clone(),
                    fk.name.clone(),
                ));
            }
            _ => steps.push(Step::AddForeignKey(tref.clone(), fk.clone())),
        }
    }
    for fk in &from.foreign_keys {
        let kept = to.foreign_keys.contains(fk) && !touches(&fk.columns);
        if !kept && !renamed_keys.contains(&fk.name.as_str()) {
            steps.push(Step::DropForeignKey(TableRef::of(from), fk.name.clone()));
        }
    }
}

/// Whether two column lists name the same columns, allowing for column renames.
fn same_columns(
    old: &[String],
    new: &[String],
    from: &Table,
    to: &Table,
    renames: &Renames,
) -> bool {
    old.len() == new.len()
        && old.iter().zip(new).all(|(o, n)| {
            o == n
                || renames.columns.iter().any(|(s, t, ro, rn)| {
                    *s == to.schema
                        && *t == to.name
                        && ro == o
                        && rn == n
                        && from.column(o).is_some()
                })
        })
}

fn create_table(t: &Table, steps: &mut Vec<Step>) {
    let tref = TableRef::of(t);
    steps.push(Step::CreateTable(TableDef {
        table: tref.clone(),
        columns: t.columns.iter().map(ColumnDef::from).collect(),
        primary_key: t.primary_key.as_ref().map(PrimaryKeyDef::from),
    }));
    for i in &t.indexes {
        steps.push(Step::CreateIndex(tref.clone(), i.clone()));
    }
    for fk in &t.foreign_keys {
        steps.push(Step::AddForeignKey(tref.clone(), fk.clone()));
    }
}

fn enum_steps(from: &Shape, to: &Shape, steps: &mut Vec<Step>) {
    for e in &to.enums {
        let key = TableRef {
            schema: e.schema.clone(),
            name: e.name.clone(),
        };
        match from
            .enums
            .iter()
            .find(|f| f.schema == e.schema && f.name == e.name)
        {
            None => steps.push(Step::CreateEnum(e.clone())),
            Some(old) if old.values.iter().all(|v| e.values.contains(v)) => {
                let added: Vec<String> = e
                    .values
                    .iter()
                    .filter(|v| !old.values.contains(v))
                    .cloned()
                    .collect();
                if !added.is_empty() {
                    steps.push(Step::AddEnumValues(key, added));
                }
            }
            Some(_) => {
                let quoted = crate::postgres::types::type_name(&e.name);
                let columns = to
                    .tables
                    .iter()
                    .flat_map(|t| {
                        t.columns
                            .iter()
                            .filter(|c| c.ty.trim_end_matches("[]") == quoted)
                            .map(move |c| (TableRef::of(t), c.name.clone(), c.default.clone()))
                    })
                    .collect();
                steps.push(Step::RewriteEnum {
                    to: e.clone(),
                    columns,
                });
            }
        }
    }
    for e in &from.enums {
        if !to
            .enums
            .iter()
            .any(|t| t.schema == e.schema && t.name == e.name)
        {
            steps.push(Step::DropEnum(TableRef {
                schema: e.schema.clone(),
                name: e.name.clone(),
            }));
        }
    }
}

/// The steps from `from` to `to`, in the order they run.
#[must_use]
pub fn diff(from: &Shape, to: &Shape, renames: &Renames) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut schemas: Vec<&str> = to.tables.iter().map(|t| t.schema.as_str()).collect();
    schemas.extend(to.enums.iter().map(|e| e.schema.as_str()));
    schemas.sort_unstable();
    schemas.dedup();
    for s in schemas {
        let exists = s == "public"
            || from.tables.iter().any(|t| t.schema == s)
            || from.enums.iter().any(|e| e.schema == s);
        if !exists {
            steps.push(Step::CreateSchema(s.to_owned()));
        }
    }
    enum_steps(from, to, &mut steps);
    let mut matched: Vec<(&Table, &Table)> = Vec::new();
    for t in &to.tables {
        let hint = renames
            .tables
            .iter()
            .find(|(s, _, new)| *s == t.schema && *new == t.name)
            .and_then(|(s, old, _)| from.table(s, old));
        if let Some(old) = from.table(&t.schema, &t.name).or(hint) {
            if old.name != t.name {
                steps.push(Step::RenameTable(TableRef::of(old), t.name.clone()));
            }
            matched.push((old, t));
        } else {
            create_table(t, &mut steps);
        }
    }
    for t in &from.tables {
        if !matched.iter().any(|(old, _)| std::ptr::eq(*old, t)) {
            for fk in &t.foreign_keys {
                steps.push(Step::DropForeignKey(TableRef::of(t), fk.name.clone()));
            }
            steps.push(Step::DropTable(TableRef::of(t)));
        }
    }
    for (old, new) in matched {
        table_changes(old, new, renames, &mut steps);
    }
    steps.sort();
    steps.dedup();
    steps
}

#[cfg(test)]
mod tests {
    use super::cast;

    #[test]
    fn cast_tiers() {
        assert_eq!(cast("integer", "bigint"), (true, true));
        assert_eq!(cast("bigint", "integer"), (true, false));
        assert_eq!(cast("character varying(10)", "text"), (true, true));
        assert_eq!(cast("text", "character varying(10)"), (true, false));
        assert_eq!(cast("text", "integer"), (false, false));
        assert_eq!(cast("boolean", "integer"), (false, false));
    }
}
