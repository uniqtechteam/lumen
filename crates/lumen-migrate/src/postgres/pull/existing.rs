//! What the schema file already says, for a second pull: its blocks matched by database name,
//! their comments as written, and the shape it asks for, so `db pull` can tell which of the
//! file's attributes still describe the database.

use lumen_schema::ast::{self, SchemaAst, Top};
use lumen_schema::checked::{FieldKind, Model, ModelKind, Relation, RelationKind, Schema};
use lumen_schema::span::Span;

use crate::calculate::calculate;
use crate::postgres::types::{column_type, type_name};
use crate::shape::{Shape, Table};

/// The schema file before the pull.
pub struct Existing<'a> {
    /// Its text.
    pub text: &'a str,
    /// Its syntax tree.
    pub ast: &'a SchemaAst,
    /// Its checked form.
    pub schema: &'a Schema,
    /// The tables it asks for.
    pub shape: Shape,
    /// For each checked model, its block and top-level index.
    models: Vec<(&'a ast::Model, usize)>,
    /// For each checked enum, its block and top-level index.
    enums: Vec<(&'a ast::Enum, usize)>,
}

/// The start of the line `pos` is on.
fn line_start(text: &str, pos: usize) -> usize {
    text[..pos].rfind('\n').map_or(0, |i| i + 1)
}

/// The start of the line after the one `pos` is on.
fn next_line(text: &str, pos: usize) -> usize {
    text[pos..].find('\n').map_or(text.len(), |i| pos + i + 1)
}

/// The comment lines of `slice`, without blank lines at either end and without lumen's own
/// `// lumen:` notes (they are written again); `// lumen: renamed from` hints are kept. Up to
/// `indent` spaces are taken off each line, so the inside of a `/** */` keeps its own indent. A
/// blank line between the comments and the item stays.
fn comments(slice: &str, indent: usize) -> String {
    let lines: Vec<&str> = slice
        .lines()
        .map(str::trim_end)
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("// lumen:")
                && !t["// lumen:".len()..]
                    .trim_start()
                    .starts_with("renamed from"))
        })
        .collect();
    let first = lines.iter().position(|l| !l.trim().is_empty());
    let last = lines.iter().rposition(|l| !l.trim().is_empty());
    let (Some(a), Some(b)) = (first, last) else {
        return String::new();
    };
    let mut out = String::new();
    let mut blank = false;
    for l in &lines[a..=b] {
        if l.trim().is_empty() {
            blank = true;
            continue;
        }
        if blank {
            out.push('\n');
            blank = false;
        }
        let spaces = l.len() - l.trim_start().len();
        out.push_str(&l[spaces.min(indent)..]);
        out.push('\n');
    }
    if b + 1 < lines.len() {
        out.push('\n');
    }
    out
}

/// The comments and blank lines written around one item of a block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Around {
    /// Comment lines above it, each ending in a newline; a blank line in between stays.
    pub above: String,
    /// Whether a blank line comes just before it (it starts a new group).
    pub blank_before: bool,
    /// The comment at the end of its line, with the `//`.
    pub trailing: String,
}

impl<'a> Existing<'a> {
    /// The file `text`, parsed as `ast` and checked as `schema`.
    #[must_use]
    pub fn new(text: &'a str, ast: &'a SchemaAst, schema: &'a Schema) -> Self {
        let mut models = Vec::new();
        let mut enums = Vec::new();
        for (i, top) in ast.tops.iter().enumerate() {
            match top {
                Top::Model(m) if m.kind != ast::ModelKind::CompositeType => models.push((m, i)),
                Top::Enum(e) => enums.push((e, i)),
                _ => {}
            }
        }
        // the checker makes one model per block, in order; anything else means it gave up
        if models.len() != schema.models.len()
            || schema
                .models
                .iter()
                .zip(&models)
                .any(|(c, (a, _))| c.name != a.name.name || c.fields.len() != a.fields.len())
        {
            models.clear();
        }
        if enums.len() != schema.enums.len() {
            enums.clear();
        }
        Self {
            text,
            ast,
            schema,
            shape: calculate(schema),
            models,
            enums,
        }
    }

    fn schema_of(&self, schema: Option<&String>) -> String {
        schema.cloned().unwrap_or_else(|| "public".to_owned())
    }

    /// The model (or view, when `view`) for the table `schema.name`.
    #[must_use]
    pub fn model(&self, schema: &str, name: &str, view: bool) -> Option<usize> {
        if self.models.is_empty() {
            return None;
        }
        self.schema.models.iter().position(|m| {
            (m.kind == ModelKind::View) == view
                && m.database_name() == name
                && self.schema_of(m.schema.as_ref()) == schema
        })
    }

    /// The enum for the type `schema.name`.
    #[must_use]
    pub fn enumeration(&self, schema: &str, name: &str) -> Option<usize> {
        if self.enums.is_empty() {
            return None;
        }
        self.schema.enums.iter().position(|e| {
            e.mapped_name.as_deref().unwrap_or(&e.name) == name
                && self.schema_of(e.schema.as_ref()) == schema
        })
    }

    /// The checked model `i`.
    #[must_use]
    pub fn checked(&self, i: usize) -> &'a Model {
        &self.schema.models[i]
    }

    /// The block of model `i`.
    #[must_use]
    pub fn block(&self, i: usize) -> &'a ast::Model {
        self.models[i].0
    }

    /// The top-level position of model `i`.
    #[must_use]
    pub fn top_of_model(&self, i: usize) -> usize {
        self.models[i].1
    }

    /// The top-level position of enum `i`.
    #[must_use]
    pub fn top_of_enum(&self, i: usize) -> usize {
        self.enums[i].1
    }

    /// The block of enum `i`.
    #[must_use]
    pub fn enum_block(&self, i: usize) -> &'a ast::Enum {
        self.enums[i].0
    }

    /// The table model `i` asks for.
    #[must_use]
    pub fn table(&self, i: usize) -> Option<&Table> {
        let m = &self.schema.models[i];
        self.shape
            .table(&self.schema_of(m.schema.as_ref()), m.database_name())
    }

    /// The scalar field of model `i` stored in `column`.
    #[must_use]
    pub fn field(&self, i: usize, column: &str) -> Option<usize> {
        self.schema.models[i]
            .fields
            .iter()
            .position(|f| f.is_scalar_like() && f.database_name() == column)
    }

    /// The column type field `f` of model `i` asks for, as `format_type` prints it.
    #[must_use]
    pub fn column_type(&self, i: usize, f: usize) -> Option<String> {
        let field = &self.schema.models[i].fields[f];
        let base = match &field.kind {
            FieldKind::Scalar(s) => column_type(*s, field.native_type.as_ref()),
            FieldKind::Enum(id) => {
                let e = &self.schema.enums[id.0];
                type_name(e.mapped_name.as_deref().unwrap_or(&e.name))
            }
            FieldKind::Unsupported(raw) => raw.clone(),
            _ => return None,
        };
        Some(if field.arity == ast::Arity::List {
            format!("{base}[]")
        } else {
            base
        })
    }

    /// The text of `span`.
    #[must_use]
    pub fn source(&self, span: Span) -> &'a str {
        &self.text[span.start..span.end]
    }

    /// The comments above top-level block `top`.
    #[must_use]
    pub fn above_top(&self, top: usize) -> String {
        let start = self.top_span(top).start;
        let from = if top == 0 {
            0
        } else {
            next_line(self.text, self.top_span(top - 1).end.saturating_sub(1))
        };
        let to = line_start(self.text, start);
        if from >= to {
            return String::new();
        }
        comments(&self.text[from..to], 0)
    }

    /// The whole text of top-level block `top`, as written.
    #[must_use]
    pub fn top_text(&self, top: usize) -> &'a str {
        self.source(self.top_span(top))
    }

    fn top_span(&self, top: usize) -> Span {
        match &self.ast.tops[top] {
            Top::Model(m) => m.span,
            Top::Enum(e) => e.span,
            Top::Config(c) => c.span,
        }
    }

    /// The comments around each line of a block (fields or values, then `@@` attributes, by
    /// the start of their spans), and the comments just before its `}`.
    #[must_use]
    pub fn around(&self, header: Span, items: &[Span], block: Span) -> (Vec<Around>, String) {
        let mut order: Vec<usize> = (0..items.len()).collect();
        order.sort_by_key(|&i| items[i].start);
        let mut out = vec![Around::default(); items.len()];
        let first = next_line(self.text, header.end);
        let mut from = first;
        for &i in &order {
            let to = line_start(self.text, items[i].start);
            if from < to {
                let slice = &self.text[from..to];
                let indent = items[i].start - line_start(self.text, items[i].start);
                out[i].above = comments(slice, indent);
                // a blank line before the comments (or before the item, with none)
                out[i].blank_before = from != first
                    && slice
                        .lines()
                        .take_while(|l| !l.trim().starts_with("//") && !l.trim().starts_with("/*"))
                        .any(|l| l.trim().is_empty());
            }
            let end = items[i].end;
            let line_end = self.text[end..]
                .find('\n')
                .map_or(self.text.len(), |p| end + p);
            let trailing = self.text[end..line_end].trim();
            if trailing.starts_with("//") {
                out[i].trailing = trailing.to_owned();
            }
            from = next_line(self.text, end);
        }
        let close = line_start(self.text, block.end.saturating_sub(1));
        let tail = if from < close {
            comments(&self.text[from..close], 2)
        } else {
            String::new()
        };
        (out, tail)
    }

    /// The relation whose foreign key is on model `holder` in `columns`, pointing at `target`.
    #[must_use]
    pub fn relation(
        &self,
        holder: usize,
        columns: &[String],
        target: usize,
    ) -> Option<&'a Relation> {
        self.schema.relations.iter().find(|r| {
            r.kind != RelationKind::ManyToMany
                && r.a.model.0 == holder
                && r.b.model.0 == target
                && r.fields.len() == columns.len()
                && r.fields
                    .iter()
                    .zip(columns)
                    .all(|(&f, c)| self.schema.models[holder].fields[f].database_name() == c)
        })
    }

    /// The many-to-many relation kept in the join table `_name`.
    #[must_use]
    pub fn many_to_many(&self, name: &str) -> Option<&'a Relation> {
        self.schema
            .relations
            .iter()
            .find(|r| r.kind == RelationKind::ManyToMany && r.name == name)
    }

    /// Whether the relation field `field` of model `model` names its relation itself.
    #[must_use]
    pub fn names_relation(&self, model: usize, field: usize) -> bool {
        self.block(model).fields[field].attributes.iter().any(|a| {
            a.name.name == "relation"
                && a.args.iter().any(|arg| {
                    arg.name.as_ref().is_none_or(|n| n.name == "name")
                        && matches!(arg.value, ast::Expr::String(..))
                })
        })
    }
}
