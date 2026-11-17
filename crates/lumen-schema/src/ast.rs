//! The syntax tree of a schema file, exactly as written: nothing is resolved or checked here.

use crate::span::Span;

/// A name and where it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    /// The name.
    pub name: String,
    /// Where it is.
    pub span: Span,
}

/// Comments written around an item, kept for the formatter and for `// lumen:` hints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comments {
    /// `///` lines above the item, then a `///` comment at the end of its line: its documentation.
    pub doc: Vec<String>,
    /// `//` lines above the item.
    pub leading: Vec<String>,
    /// A `//` comment at the end of the item's line.
    pub trailing: Option<String>,
}

/// A whole schema file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchemaAst {
    /// The top-level blocks, in order.
    pub tops: Vec<Top>,
    /// `//` comments left at the end of the file.
    pub trailing_comments: Vec<String>,
}

/// One top-level block.
#[derive(Debug, Clone, PartialEq)]
pub enum Top {
    /// `model`, `view` or `type` (a composite type).
    Model(Model),
    /// `enum`.
    Enum(Enum),
    /// `datasource` or `generator`.
    Config(ConfigBlock),
}

/// Which keyword opened a model-like block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// `model`
    Model,
    /// `view`
    View,
    /// `type`: a composite type, embedded in documents (MongoDB).
    CompositeType,
}

/// `model User { ... }`.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// The keyword.
    pub kind: ModelKind,
    /// The name.
    pub name: Ident,
    /// The fields, in order.
    pub fields: Vec<Field>,
    /// The `@@` attributes, in order.
    pub attributes: Vec<Attribute>,
    /// Comments.
    pub comments: Comments,
    /// The whole block.
    pub span: Span,
}

/// `email String? @unique`.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The name.
    pub name: Ident,
    /// The type; `None` when it is missing (reported as an error).
    pub ty: Option<FieldType>,
    /// The `@` attributes, in order.
    pub attributes: Vec<Attribute>,
    /// Comments.
    pub comments: Comments,
    /// The whole line.
    pub span: Span,
}

/// Required, optional or list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    /// `String`
    Required,
    /// `String?`
    Optional,
    /// `String[]`
    List,
}

/// The type part of a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldType {
    /// `String`, `Int`, an enum, a model or a composite type. For `Unsupported("..")` this is
    /// `Unsupported`.
    pub name: Ident,
    /// The text inside `Unsupported("..")`.
    pub unsupported: Option<String>,
    /// Required, optional or list.
    pub arity: Arity,
    /// The type with its `?` or `[]`.
    pub span: Span,
}

/// `@unique`, `@db.VarChar(255)`, `@@index([email], name: "by_email")`.
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    /// The name with its dots: `default`, `db.VarChar`, `index`.
    pub name: Ident,
    /// The arguments, when there are parentheses.
    pub args: Vec<Argument>,
    /// Whether it has parentheses at all (`@id` vs `@id()`).
    pub has_parens: bool,
    /// The whole attribute.
    pub span: Span,
}

/// One argument: `[email]` or `name: "by_email"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Argument {
    /// The name before `:`, if any.
    pub name: Option<Ident>,
    /// The value.
    pub value: Expr,
    /// The whole argument.
    pub span: Span,
}

/// A value inside attributes and settings.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// `"text"`
    String(String, Span),
    /// `42`, as written.
    Number(String, Span),
    /// A bare name: `Cascade`, `true`, `email`, `Role.ADMIN` (with dots).
    Constant(String, Span),
    /// `now()`, `env("DATABASE_URL")`, `email(sort: Desc)`.
    Function(Ident, Vec<Argument>, Span),
    /// `[a, b]`
    Array(Vec<Expr>, Span),
    /// `{ published: true }`, for example in a partial index's `where`.
    Object(Vec<(Ident, Expr)>, Span),
}

impl Expr {
    /// Where the value is.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::String(_, s)
            | Self::Number(_, s)
            | Self::Constant(_, s)
            | Self::Function(_, _, s)
            | Self::Array(_, s)
            | Self::Object(_, s) => *s,
        }
    }
}

/// `enum Role { USER ADMIN }`.
#[derive(Debug, Clone, PartialEq)]
pub struct Enum {
    /// The name.
    pub name: Ident,
    /// The values, in order.
    pub values: Vec<EnumValue>,
    /// The `@@` attributes.
    pub attributes: Vec<Attribute>,
    /// Comments.
    pub comments: Comments,
    /// The whole block.
    pub span: Span,
}

/// One enum value, with its `@map` if any.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumValue {
    /// The name.
    pub name: Ident,
    /// Its attributes.
    pub attributes: Vec<Attribute>,
    /// Comments.
    pub comments: Comments,
    /// The whole line.
    pub span: Span,
}

/// Which keyword opened a settings block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    /// `datasource`
    Datasource,
    /// `generator`
    Generator,
}

/// `datasource db { provider = "postgresql" url = env("DATABASE_URL") }`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigBlock {
    /// The keyword.
    pub kind: ConfigKind,
    /// The name.
    pub name: Ident,
    /// The settings, in order.
    pub properties: Vec<Property>,
    /// Comments.
    pub comments: Comments,
    /// The whole block.
    pub span: Span,
}

/// `provider = "postgresql"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    /// The name.
    pub name: Ident,
    /// The value; `None` when it is missing (reported as an error).
    pub value: Option<Expr>,
    /// Comments.
    pub comments: Comments,
    /// The whole line.
    pub span: Span,
}
