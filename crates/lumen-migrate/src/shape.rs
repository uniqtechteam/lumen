//! The shape of a database: tables, columns, keys, indexes and enums. Both sides of a migration
//! are a `Shape`: one calculated from the schema, one described from the live database, so every
//! comparison (migrations, drift, `db push`) is `diff(a, b)`.
//!
//! Column types are the exact text Postgres's `format_type()` prints (`character varying(320)`,
//! `timestamp(3) without time zone`), and defaults are normalised, so two shapes of the same
//! database compare equal.

/// What happens to rows that point at a changed or removed row.
#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
)]
pub enum Action {
    /// `CASCADE`
    Cascade,
    /// `RESTRICT`
    Restrict,
    /// `NO ACTION`
    NoAction,
    /// `SET NULL`
    SetNull,
    /// `SET DEFAULT`
    SetDefault,
}

impl Action {
    /// The SQL words.
    #[must_use]
    pub fn sql(self) -> &'static str {
        match self {
            Self::Cascade => "CASCADE",
            Self::Restrict => "RESTRICT",
            Self::NoAction => "NO ACTION",
            Self::SetNull => "SET NULL",
            Self::SetDefault => "SET DEFAULT",
        }
    }
}

/// A column default, normalised.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Default {
    /// A sequence (`SERIAL` / `nextval(..)` / identity): `autoincrement()`.
    Sequence,
    /// `CURRENT_TIMESTAMP` / `now()`.
    Now,
    /// A literal as SQL, without casts: `'READER'`, `0`, `false`, `'{}'`.
    Literal(String),
    /// Any other expression, as SQL: `gen_random_uuid()`.
    Expression(String),
}

/// One column.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// The name.
    pub name: String,
    /// The type as `format_type()` prints it, `[]` included for arrays.
    pub ty: String,
    /// Whether NULL is allowed.
    pub nullable: bool,
    /// The default, if any.
    pub default: Option<Default>,
}

/// Sort order of an index column.
#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
)]
pub enum Sort {
    /// Ascending (the default).
    Asc,
    /// Descending.
    Desc,
}

/// One column of an index.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexColumn {
    /// The column name.
    pub name: String,
    /// Its order.
    pub sort: Sort,
}

/// An index or unique index.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Index {
    /// The name.
    pub name: String,
    /// The columns, in order.
    pub columns: Vec<IndexColumn>,
    /// Whether it is unique.
    pub unique: bool,
    /// The method when it is not `btree` (`hash`, `gin`, ...).
    pub method: Option<String>,
    /// The `WHERE` of a partial index, as Postgres prints it (`deleted_at IS NULL`).
    #[serde(default)]
    pub predicate: Option<String>,
    /// Whether a key is an expression (`lower(email)`); its column name is then the expression.
    #[serde(default)]
    pub expression: bool,
}

/// A primary key.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PrimaryKey {
    /// The constraint name.
    pub name: String,
    /// The columns, in order.
    pub columns: Vec<String>,
}

/// A foreign key.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ForeignKey {
    /// The constraint name.
    pub name: String,
    /// The columns on this table, in order.
    pub columns: Vec<String>,
    /// The referenced table's schema.
    pub referenced_schema: String,
    /// The referenced table.
    pub referenced_table: String,
    /// The referenced columns, in order.
    pub referenced_columns: Vec<String>,
    /// `ON DELETE`.
    pub on_delete: Action,
    /// `ON UPDATE`.
    pub on_update: Action,
}

/// A table.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The schema (namespace), `public` by default.
    pub schema: String,
    /// The name.
    pub name: String,
    /// The columns, in order.
    pub columns: Vec<Column>,
    /// The primary key.
    pub primary_key: Option<PrimaryKey>,
    /// Indexes and unique indexes, by name.
    pub indexes: Vec<Index>,
    /// Foreign keys, by name.
    pub foreign_keys: Vec<ForeignKey>,
}

impl Table {
    /// The column named `name`.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }
}

/// An enum type.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnumType {
    /// The schema.
    pub schema: String,
    /// The name.
    pub name: String,
    /// The values, in order.
    pub values: Vec<String>,
}

/// A whole database.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Shape {
    /// Tables, by schema and name.
    pub tables: Vec<Table>,
    /// Enum types, by schema and name.
    pub enums: Vec<EnumType>,
}

impl Shape {
    /// Put everything in a fixed order, so equal databases give equal shapes.
    pub fn normalise(&mut self) {
        self.tables
            .sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        for t in &mut self.tables {
            t.indexes.sort();
            t.foreign_keys.sort();
        }
        self.enums.sort();
    }

    /// The table `schema.name`.
    #[must_use]
    pub fn table(&self, schema: &str, name: &str) -> Option<&Table> {
        self.tables
            .iter()
            .find(|t| t.schema == schema && t.name == name)
    }
}
