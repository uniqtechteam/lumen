//! Pass 1: names and types. Builds the schema with every model, enum, composite type and field,
//! resolves field types, and reads the `datasource` and `generator` blocks.

use std::collections::{HashMap, HashSet};

use super::{
    CompositeType, Datasource, Enum, EnumId, EnumValue, Field, FieldKind, Generator, Model,
    ModelId, ModelKind, Provider, ScalarType, Schema, StringOrEnv, TypeId,
};
use crate::ast::{self, Comments, ConfigKind, Expr, SchemaAst, Top};
use crate::diagnostics::{Diagnostic, Diagnostics};

/// The models and views of the file, in the same order as [`Schema::models`].
pub(super) fn model_asts(ast: &SchemaAst) -> Vec<&ast::Model> {
    ast.tops
        .iter()
        .filter_map(|t| match t {
            Top::Model(m) if m.kind != ast::ModelKind::CompositeType => Some(m),
            _ => None,
        })
        .collect()
}

/// The composite types of the file, in the same order as [`Schema::composite_types`].
pub(super) fn type_asts(ast: &SchemaAst) -> Vec<&ast::Model> {
    ast.tops
        .iter()
        .filter_map(|t| match t {
            Top::Model(m) if m.kind == ast::ModelKind::CompositeType => Some(m),
            _ => None,
        })
        .collect()
}

/// The enums of the file, in the same order as [`Schema::enums`].
pub(super) fn enum_asts(ast: &SchemaAst) -> Vec<&ast::Enum> {
    ast.tops
        .iter()
        .filter_map(|t| match t {
            Top::Enum(e) => Some(e),
            _ => None,
        })
        .collect()
}

/// `///` lines joined, or `None`.
pub(super) fn doc(comments: &Comments) -> Option<String> {
    (!comments.doc.is_empty()).then(|| comments.doc.join("\n"))
}

/// `// lumen: renamed from old_name` among the comments above an item.
pub(super) fn renamed_from(comments: &Comments) -> Option<String> {
    comments.leading.iter().find_map(|c| {
        let rest = c.trim().strip_prefix("lumen:")?.trim();
        let name = rest.strip_prefix("renamed from")?.trim();
        (!name.is_empty()).then(|| name.to_owned())
    })
}

/// Levenshtein distance, for "did you mean" hints.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let diagonal = prev;
            prev = row[j + 1];
            row[j + 1] = (diagonal + cost).min(row[j] + 1).min(prev + 1);
        }
    }
    row[b.len()]
}

/// The closest name to `wanted` among `options`, if it is close enough to be a typo.
pub(super) fn closest<'a>(wanted: &str, options: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    options
        .map(|o| (distance(wanted, o), o))
        .filter(|(d, o)| *d <= 2.max(o.len() / 4) && *d > 0)
        .min_by_key(|(d, _)| *d)
        .map(|(_, o)| o)
}

#[derive(Clone, Copy)]
enum TopKind {
    Model(ModelId),
    Enum(EnumId),
    Type(TypeId),
}

fn names(ast: &SchemaAst, diagnostics: &mut Diagnostics) -> HashMap<String, TopKind> {
    let mut seen: HashMap<String, TopKind> = HashMap::new();
    let mut insert = |name: &ast::Ident, kind: TopKind, d: &mut Diagnostics| {
        if ScalarType::from_name(&name.name).is_some() || name.name == "Unsupported" {
            d.push(Diagnostic::reserved_name(name.span, &name.name));
        } else if seen.contains_key(&name.name) {
            d.push(Diagnostic::duplicate_top(name.span, &name.name));
        } else {
            seen.insert(name.name.clone(), kind);
        }
    };
    for (i, m) in model_asts(ast).iter().enumerate() {
        insert(&m.name, TopKind::Model(ModelId(i)), diagnostics);
    }
    for (i, e) in enum_asts(ast).iter().enumerate() {
        insert(&e.name, TopKind::Enum(EnumId(i)), diagnostics);
    }
    for (i, t) in type_asts(ast).iter().enumerate() {
        insert(&t.name, TopKind::Type(TypeId(i)), diagnostics);
    }
    seen
}

fn field_kind(
    ty: &ast::FieldType,
    tops: &HashMap<String, TopKind>,
    diagnostics: &mut Diagnostics,
) -> FieldKind {
    if let Some(raw) = &ty.unsupported {
        return FieldKind::Unsupported(raw.clone());
    }
    if let Some(scalar) = ScalarType::from_name(&ty.name.name) {
        return FieldKind::Scalar(scalar);
    }
    match tops.get(&ty.name.name) {
        Some(TopKind::Model(id)) => FieldKind::Model(*id),
        Some(TopKind::Enum(id)) => FieldKind::Enum(*id),
        Some(TopKind::Type(id)) => FieldKind::Composite(*id),
        None => {
            const SCALARS: [&str; 9] = [
                "String", "Int", "BigInt", "Float", "Decimal", "Boolean", "DateTime", "Json",
                "Bytes",
            ];
            let options = SCALARS
                .iter()
                .copied()
                .chain(tops.keys().map(String::as_str));
            let hint = closest(&ty.name.name, options);
            diagnostics.push(Diagnostic::unknown_type(ty.name.span, &ty.name.name, hint));
            FieldKind::Unknown
        }
    }
}

fn fields(
    block: &ast::Model,
    tops: &HashMap<String, TopKind>,
    diagnostics: &mut Diagnostics,
) -> Vec<Field> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for f in &block.fields {
        if !seen.insert(&f.name.name) {
            diagnostics.push(Diagnostic::duplicate_field(
                f.name.span,
                &f.name.name,
                &block.name.name,
            ));
        }
        let (kind, arity) = match &f.ty {
            Some(ty) => (field_kind(ty, tops, diagnostics), ty.arity),
            None => (FieldKind::Unknown, ast::Arity::Required),
        };
        out.push(Field {
            name: f.name.name.clone(),
            mapped_name: None,
            kind,
            arity,
            default: None,
            native_type: None,
            updated_at: false,
            ignored: false,
            relation: None,
            doc: doc(&f.comments),
            renamed_from: renamed_from(&f.comments),
            span: f.span,
        });
    }
    out
}

fn string_or_env(value: &Expr) -> Option<StringOrEnv> {
    match value {
        Expr::String(s, _) => Some(StringOrEnv::Literal(s.clone())),
        Expr::Function(name, args, _) if name.name == "env" => match args.first().map(|a| &a.value)
        {
            Some(Expr::String(var, _)) => Some(StringOrEnv::Env(var.clone())),
            _ => None,
        },
        _ => None,
    }
}

fn names_in_list(value: &Expr) -> Vec<String> {
    let Expr::Array(items, _) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|e| match e {
            Expr::String(s, _) | Expr::Constant(s, _) => Some(s.clone()),
            Expr::Function(name, _, _) => Some(name.name.clone()),
            _ => None,
        })
        .collect()
}

/// `schemas = ["a", "b"]`: strings, each once.
fn schema_names(value: &Expr, diagnostics: &mut Diagnostics) -> Vec<String> {
    let Expr::Array(items, _) = value else {
        diagnostics.push(Diagnostic::expected(
            value.span(),
            "a list of schema names",
            "something else",
        ));
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for item in items {
        match item {
            Expr::String(s, span) => {
                if out.contains(s) {
                    diagnostics.push(Diagnostic::invalid_attribute(
                        *span,
                        "schemas",
                        format!("`{s}` is listed twice"),
                    ));
                } else {
                    out.push(s.clone());
                }
            }
            other => diagnostics.push(Diagnostic::expected(
                other.span(),
                "a schema name in quotes",
                "something else",
            )),
        }
    }
    out
}

fn datasource(block: &ast::ConfigBlock, diagnostics: &mut Diagnostics) -> Option<Datasource> {
    let get = |key: &str| {
        block
            .properties
            .iter()
            .find(|p| p.name.name == key)
            .and_then(|p| p.value.as_ref())
    };
    let provider = match get("provider") {
        Some(Expr::String(p, span)) => match p.as_str() {
            "postgresql" | "postgres" => Provider::Postgres,
            "mongodb" => Provider::MongoDb,
            other => {
                diagnostics.push(Diagnostic::unsupported_provider(*span, other));
                return None;
            }
        },
        Some(other) => {
            diagnostics.push(Diagnostic::expected(
                other.span(),
                "a string such as \"postgresql\"",
                "something else",
            ));
            return None;
        }
        None => {
            diagnostics.push(Diagnostic::missing_provider(
                block.name.span,
                &block.name.name,
            ));
            return None;
        }
    };
    Some(Datasource {
        name: block.name.name.clone(),
        provider,
        url: get("url").and_then(string_or_env),
        direct_url: get("directUrl").and_then(string_or_env),
        shadow_database_url: get("shadowDatabaseUrl").and_then(string_or_env),
        schemas: get("schemas")
            .map(|v| schema_names(v, diagnostics))
            .unwrap_or_default(),
        extensions: get("extensions").map(names_in_list).unwrap_or_default(),
    })
}

fn generator(block: &ast::ConfigBlock) -> Generator {
    let mut g = Generator {
        name: block.name.name.clone(),
        provider: None,
        output: None,
        settings: Vec::new(),
    };
    for p in &block.properties {
        let Some(value) = &p.value else { continue };
        match p.name.name.as_str() {
            "provider" => g.provider = string_or_env(value),
            "output" => {
                if let Expr::String(s, _) = value {
                    g.output = Some(s.clone());
                }
            }
            other => {
                let text = match value {
                    Expr::String(s, _) | Expr::Number(s, _) | Expr::Constant(s, _) => s.clone(),
                    Expr::Array(items, _) => format!("{} items", items.len()),
                    _ => String::new(),
                };
                g.settings.push((other.to_owned(), text));
            }
        }
    }
    g
}

/// Pass 1. See the module docs.
pub(super) fn resolve(ast: &SchemaAst, diagnostics: &mut Diagnostics) -> Schema {
    let tops = names(ast, diagnostics);
    let mut schema = Schema::default();
    for top in &ast.tops {
        if let Top::Config(block) = top {
            match block.kind {
                ConfigKind::Datasource => {
                    if schema.datasource.is_some() {
                        diagnostics.push(Diagnostic::second_datasource(block.name.span));
                    } else {
                        schema.datasource = datasource(block, diagnostics);
                    }
                }
                ConfigKind::Generator => {
                    if schema.generators.iter().any(|g| g.name == block.name.name) {
                        diagnostics
                            .push(Diagnostic::duplicate_top(block.name.span, &block.name.name));
                    }
                    schema.generators.push(generator(block));
                }
            }
        }
    }
    for m in model_asts(ast) {
        schema.models.push(Model {
            name: m.name.name.clone(),
            kind: if m.kind == ast::ModelKind::View {
                ModelKind::View
            } else {
                ModelKind::Model
            },
            mapped_name: None,
            schema: None,
            fields: fields(m, &tops, diagnostics),
            indexes: Vec::new(),
            ignored: false,
            doc: doc(&m.comments),
            renamed_from: renamed_from(&m.comments),
            span: m.span,
        });
    }
    for t in type_asts(ast) {
        schema.composite_types.push(CompositeType {
            name: t.name.name.clone(),
            fields: fields(t, &tops, diagnostics),
            doc: doc(&t.comments),
            span: t.span,
        });
    }
    for e in enum_asts(ast) {
        let mut seen: HashSet<&str> = HashSet::new();
        for v in &e.values {
            if !seen.insert(&v.name.name) {
                diagnostics.push(Diagnostic::duplicate_enum_value(
                    v.name.span,
                    &v.name.name,
                    &e.name.name,
                ));
            }
        }
        if e.values.is_empty() {
            diagnostics.push(Diagnostic::empty_enum(e.name.span, &e.name.name));
        }
        schema.enums.push(Enum {
            name: e.name.name.clone(),
            mapped_name: None,
            schema: None,
            values: e
                .values
                .iter()
                .map(|v| EnumValue {
                    name: v.name.name.clone(),
                    mapped_name: None,
                    doc: doc(&v.comments),
                })
                .collect(),
            doc: doc(&e.comments),
            span: e.span,
        });
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::{closest, distance, renamed_from};
    use crate::ast::Comments;

    #[test]
    fn did_you_mean() {
        assert_eq!(distance("Strng", "String"), 1);
        let options = ["String", "Int", "User", "Post"];
        assert_eq!(closest("Strng", options.iter().copied()), Some("String"));
        assert_eq!(closest("user", options.iter().copied()), Some("User"));
        assert_eq!(closest("Banana", options.iter().copied()), None);
    }

    #[test]
    fn rename_hints() {
        let c = Comments {
            leading: vec!["note".into(), "lumen: renamed from full_name".into()],
            ..Comments::default()
        };
        assert_eq!(renamed_from(&c).as_deref(), Some("full_name"));
        assert_eq!(renamed_from(&Comments::default()), None);
    }
}
