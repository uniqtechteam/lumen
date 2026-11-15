//! The checked schema: names resolved, types known, attributes read, relations paired. This is what
//! migrations and the client generator work from. It is always built, even when there are errors,
//! so tools keep working on a half-written file.

mod attributes;
mod defaults;
mod native_types;
mod relations;
mod resolve;

use crate::ast::{Arity, SchemaAst};
use crate::diagnostics::Diagnostics;
use crate::span::Span;

pub use native_types::NativeType;

/// Index of a model in [`Schema::models`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelId(pub usize);

/// Index of an enum in [`Schema::enums`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnumId(pub usize);

/// Index of a composite type in [`Schema::composite_types`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(pub usize);

/// Index of a relation in [`Schema::relations`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelationId(pub usize);

/// The database a schema targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// `postgresql` (or `postgres`).
    Postgres,
    /// `mongodb`.
    MongoDb,
}

/// A setting that is either written in the file or read from an environment variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StringOrEnv {
    /// `"postgres://..."`
    Literal(String),
    /// `env("DATABASE_URL")`
    Env(String),
}

/// The `datasource` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datasource {
    /// The block's name, also the prefix of native types (`@db.VarChar`).
    pub name: String,
    /// The database.
    pub provider: Provider,
    /// `url`.
    pub url: Option<StringOrEnv>,
    /// `directUrl`.
    pub direct_url: Option<StringOrEnv>,
    /// `shadowDatabaseUrl`.
    pub shadow_database_url: Option<StringOrEnv>,
    /// `schemas = [..]` (Postgres).
    pub schemas: Vec<String>,
    /// `extensions = [..]` (Postgres).
    pub extensions: Vec<String>,
}

/// A `generator` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generator {
    /// The block's name.
    pub name: String,
    /// `provider`.
    pub provider: Option<StringOrEnv>,
    /// `output`.
    pub output: Option<String>,
    /// Every other setting, as written.
    pub settings: Vec<(String, String)>,
}

/// The built-in scalar types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalarType {
    /// Text.
    String,
    /// 32-bit integer.
    Int,
    /// 64-bit integer.
    BigInt,
    /// 64-bit floating point.
    Float,
    /// Exact decimal.
    Decimal,
    /// `true` or `false`.
    Boolean,
    /// Date and time.
    DateTime,
    /// Any JSON value.
    Json,
    /// Raw bytes.
    Bytes,
}

impl ScalarType {
    /// The scalar named `name`, if it is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "String" => Self::String,
            "Int" => Self::Int,
            "BigInt" => Self::BigInt,
            "Float" => Self::Float,
            "Decimal" => Self::Decimal,
            "Boolean" => Self::Boolean,
            "DateTime" => Self::DateTime,
            "Json" => Self::Json,
            "Bytes" => Self::Bytes,
            _ => return None,
        })
    }

    /// The name as written in a schema.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::String => "String",
            Self::Int => "Int",
            Self::BigInt => "BigInt",
            Self::Float => "Float",
            Self::Decimal => "Decimal",
            Self::Boolean => "Boolean",
            Self::DateTime => "DateTime",
            Self::Json => "Json",
            Self::Bytes => "Bytes",
        }
    }
}

/// What a field holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    /// A built-in scalar.
    Scalar(ScalarType),
    /// One of the schema's enums.
    Enum(EnumId),
    /// An embedded composite type (MongoDB).
    Composite(TypeId),
    /// Another model: a relation field. Which relation is in [`Field::relation`].
    Model(ModelId),
    /// `Unsupported("..")`: a database type the client cannot read.
    Unsupported(String),
    /// The type could not be resolved (already reported).
    Unknown,
}

/// A default value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultValue {
    /// `autoincrement()`
    Autoincrement,
    /// `now()`
    Now,
    /// `uuid()` with its version (4 or 7).
    Uuid(u8),
    /// `cuid()` with its version (1 or 2).
    Cuid(u8),
    /// `nanoid()` with an optional length.
    Nanoid(Option<u32>),
    /// `ulid()`
    Ulid,
    /// `auto()`: the database makes it (MongoDB `ObjectId`).
    Auto,
    /// `dbgenerated("expr")`: SQL the database runs.
    DbGenerated(Option<String>),
    /// A literal: string, number, boolean, enum value, or a list of them, as written.
    Literal(Literal),
}

/// A literal default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    /// `"text"`
    String(String),
    /// `42`, `1.5`, as written.
    Number(String),
    /// `true` / `false`
    Boolean(bool),
    /// An enum value.
    EnumValue(String),
    /// `[..]` for list fields.
    List(Vec<Literal>),
}

/// One field of a model or composite type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The name in the schema (and in the generated client).
    pub name: String,
    /// `@map("..")`: the column or document key, when it differs.
    pub mapped_name: Option<String>,
    /// What it holds.
    pub kind: FieldKind,
    /// Required, optional or list.
    pub arity: Arity,
    /// `@default(..)`.
    pub default: Option<DefaultValue>,
    /// `@db.X(..)`.
    pub native_type: Option<NativeType>,
    /// `@updatedAt`.
    pub updated_at: bool,
    /// `@ignore`.
    pub ignored: bool,
    /// For relation fields: the relation it belongs to.
    pub relation: Option<RelationId>,
    /// `///` documentation.
    pub doc: Option<String>,
    /// `// lumen: renamed from old` hint for migrations.
    pub renamed_from: Option<String>,
    /// Where it is written.
    pub span: Span,
}

impl Field {
    /// The column or key name in the database.
    #[must_use]
    pub fn database_name(&self) -> &str {
        self.mapped_name.as_deref().unwrap_or(&self.name)
    }

    /// Whether this is a scalar field (not a relation, composite or unknown).
    #[must_use]
    pub fn is_scalar_like(&self) -> bool {
        matches!(
            self.kind,
            FieldKind::Scalar(_) | FieldKind::Enum(_) | FieldKind::Unsupported(_)
        )
    }
}

/// Sort order in an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

/// A field inside `@@index`, `@@unique` or `@@id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexField {
    /// Index of the field in [`Model::fields`].
    pub field: usize,
    /// `sort: Desc`.
    pub sort: Option<SortOrder>,
    /// `length: 10` (prefix length).
    pub length: Option<u32>,
}

/// What kind of index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexKind {
    /// The primary key (`@id` / `@@id`).
    PrimaryKey,
    /// `@unique` / `@@unique`.
    Unique,
    /// `@@index`.
    Normal,
    /// `@@fulltext`.
    Fulltext,
}

/// An index, unique constraint or primary key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// Which kind.
    pub kind: IndexKind,
    /// The fields, in order.
    pub fields: Vec<IndexField>,
    /// `name: ".."`: the name in the client (compound uniques and ids).
    pub client_name: Option<String>,
    /// `map: ".."`: the constraint name in the database.
    pub mapped_name: Option<String>,
    /// `type: Hash` and similar (Postgres index method).
    pub algorithm: Option<String>,
    /// `where: ..` for partial indexes, as written.
    pub where_clause: Option<String>,
    /// Where it is written.
    pub span: Span,
}

/// Model, view or composite type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// A table or collection.
    Model,
    /// A database view.
    View,
}

/// A model (or view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// The name in the schema.
    pub name: String,
    /// Model or view.
    pub kind: ModelKind,
    /// `@@map("..")`: the table or collection, when it differs.
    pub mapped_name: Option<String>,
    /// `@@schema("..")`.
    pub schema: Option<String>,
    /// The fields, in order.
    pub fields: Vec<Field>,
    /// Primary key, unique constraints and indexes, in the order written (`@id`/`@unique` on fields
    /// first).
    pub indexes: Vec<Index>,
    /// `@@ignore`.
    pub ignored: bool,
    /// `///` documentation.
    pub doc: Option<String>,
    /// `// lumen: renamed from old` hint for migrations.
    pub renamed_from: Option<String>,
    /// Where it is written.
    pub span: Span,
}

impl Model {
    /// The table or collection name.
    #[must_use]
    pub fn database_name(&self) -> &str {
        self.mapped_name.as_deref().unwrap_or(&self.name)
    }

    /// The primary key, if any.
    #[must_use]
    pub fn primary_key(&self) -> Option<&Index> {
        self.indexes
            .iter()
            .find(|i| i.kind == IndexKind::PrimaryKey)
    }

    /// The field named `name`.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<(usize, &Field)> {
        self.fields.iter().enumerate().find(|(_, f)| f.name == name)
    }

    /// Whether exactly this set of fields (in any order) is the primary key or unique.
    #[must_use]
    pub fn is_unique_set(&self, fields: &[usize]) -> bool {
        self.indexes.iter().any(|i| {
            matches!(i.kind, IndexKind::PrimaryKey | IndexKind::Unique)
                && i.fields.len() == fields.len()
                && i.fields.iter().all(|f| fields.contains(&f.field))
        })
    }
}

/// A composite type (embedded document, MongoDB).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeType {
    /// The name.
    pub name: String,
    /// The fields.
    pub fields: Vec<Field>,
    /// `///` documentation.
    pub doc: Option<String>,
    /// Where it is written.
    pub span: Span,
}

/// An enum value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumValue {
    /// The name in the schema.
    pub name: String,
    /// `@map("..")`.
    pub mapped_name: Option<String>,
    /// `///` documentation.
    pub doc: Option<String>,
}

/// An enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enum {
    /// The name in the schema.
    pub name: String,
    /// `@@map("..")`.
    pub mapped_name: Option<String>,
    /// `@@schema("..")`.
    pub schema: Option<String>,
    /// The values, in order.
    pub values: Vec<EnumValue>,
    /// `///` documentation.
    pub doc: Option<String>,
    /// Where it is written.
    pub span: Span,
}

/// What a database does to rows that point at a changed or removed row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferentialAction {
    /// Change or remove them too.
    Cascade,
    /// Refuse the change.
    Restrict,
    /// Refuse the change at the end of the statement.
    NoAction,
    /// Set the reference to null.
    SetNull,
    /// Set the reference to its default.
    SetDefault,
}

/// One-to-one, one-to-many or many-to-many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    /// Each side has at most one.
    OneToOne,
    /// One side has many.
    OneToMany,
    /// Both sides have many, through a join table the database keeps (`_AToB`).
    ManyToMany,
}

/// A relation field, by model and field index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldRef {
    /// The model.
    pub model: ModelId,
    /// Index into that model's fields.
    pub field: usize,
}

/// A relation between two models, with both of its fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    /// `@relation("Name")`, or the default name (`AToB`, models sorted).
    pub name: String,
    /// Which kind.
    pub kind: RelationKind,
    /// The side that holds the foreign key (for many-to-many, the first model by name).
    pub a: FieldRef,
    /// The other side.
    pub b: FieldRef,
    /// For one-to-one and one-to-many: the foreign key fields on `a`'s model.
    pub fields: Vec<usize>,
    /// For one-to-one and one-to-many: the referenced fields on `b`'s model.
    pub references: Vec<usize>,
    /// `onDelete`.
    pub on_delete: Option<ReferentialAction>,
    /// `onUpdate`.
    pub on_update: Option<ReferentialAction>,
    /// `map: ".."`: the foreign key constraint name.
    pub mapped_name: Option<String>,
}

/// A checked schema. See the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schema {
    /// The `datasource` block, if any.
    pub datasource: Option<Datasource>,
    /// The `generator` blocks.
    pub generators: Vec<Generator>,
    /// Models and views.
    pub models: Vec<Model>,
    /// Enums.
    pub enums: Vec<Enum>,
    /// Composite types.
    pub composite_types: Vec<CompositeType>,
    /// Relations, each once.
    pub relations: Vec<Relation>,
}

impl Schema {
    /// The provider, `Postgres` when there is no datasource.
    #[must_use]
    pub fn provider(&self) -> Provider {
        self.datasource
            .as_ref()
            .map_or(Provider::Postgres, |d| d.provider)
    }

    /// The model named `name`.
    #[must_use]
    pub fn model(&self, name: &str) -> Option<(ModelId, &Model)> {
        self.models
            .iter()
            .enumerate()
            .find(|(_, m)| m.name == name)
            .map(|(i, m)| (ModelId(i), m))
    }

    /// The field a [`FieldRef`] points at.
    #[must_use]
    pub fn field(&self, r: FieldRef) -> &Field {
        &self.models[r.model.0].fields[r.field]
    }
}

/// Check a parsed schema. The schema always comes back; `diagnostics` gets every problem.
pub fn check(ast: &SchemaAst, diagnostics: &mut Diagnostics) -> Schema {
    let mut schema = resolve::resolve(ast, diagnostics);
    attributes::read_all(ast, &mut schema, diagnostics);
    relations::pair_all(ast, &mut schema, diagnostics);
    attributes::check_models(&schema, diagnostics);
    schema
}
