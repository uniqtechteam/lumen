//! Pass 2: field and block attributes (`@id`, `@unique`, `@default`, `@map`, `@updatedAt`,
//! `@ignore`, `@db.X`, `@@id`, `@@unique`, `@@index`, `@@fulltext`, `@@map`, `@@schema`,
//! `@@ignore`), and the checks on whole models.

use std::collections::{HashMap, HashSet};

use super::resolve::{closest, enum_asts, model_asts, type_asts};
use super::{
    Enum, Field, FieldKind, Index, IndexField, IndexKind, ModelKind, NativeType, Provider, Schema,
    SortOrder, defaults, native_types,
};
use crate::ast::{self, Arity, Attribute, Expr};
use crate::diagnostics::{Diagnostic, Diagnostics};

const FIELD_ATTRIBUTES: [&str; 8] = [
    "id",
    "unique",
    "default",
    "map",
    "updatedAt",
    "relation",
    "ignore",
    "db",
];
const MODEL_ATTRIBUTES: [&str; 7] = [
    "id", "unique", "index", "fulltext", "map", "schema", "ignore",
];

/// The value of argument `name`, or the positional argument at `position`.
fn arg<'a>(attr: &'a Attribute, name: &str, position: Option<usize>) -> Option<&'a Expr> {
    attr.args
        .iter()
        .find(|a| a.name.as_ref().is_some_and(|n| n.name == name))
        .or_else(|| position.and_then(|p| attr.args.iter().filter(|a| a.name.is_none()).nth(p)))
        .map(|a| &a.value)
}

fn string_arg(
    attr: &Attribute,
    name: &str,
    position: Option<usize>,
    d: &mut Diagnostics,
) -> Option<String> {
    match arg(attr, name, position)? {
        Expr::String(s, _) => Some(s.clone()),
        other => {
            d.push(Diagnostic::invalid_attribute(
                other.span(),
                &format!("@{}", attr.name.name),
                format!("`{name}` must be a string"),
            ));
            None
        }
    }
}

fn sort_and_length(args: &[ast::Argument]) -> (Option<SortOrder>, Option<u32>) {
    let mut sort = None;
    let mut length = None;
    for a in args {
        match (a.name.as_ref().map(|n| n.name.as_str()), &a.value) {
            (Some("sort"), Expr::Constant(c, _)) => {
                sort = Some(if c == "Desc" {
                    SortOrder::Desc
                } else {
                    SortOrder::Asc
                });
            }
            (Some("length"), Expr::Number(n, _)) => length = n.parse().ok(),
            _ => {}
        }
    }
    (sort, length)
}

/// `[a, b(sort: Desc)]` as index fields of the model with `fields`.
fn index_fields(
    attr: &Attribute,
    fields: &[Field],
    model: &str,
    provider: Provider,
    d: &mut Diagnostics,
) -> Option<Vec<IndexField>> {
    let label = format!("@@{}", attr.name.name);
    let Some(Expr::Array(items, span)) = arg(attr, "fields", Some(0)) else {
        d.push(Diagnostic::invalid_attribute_help(
            attr.span,
            &label,
            "the fields are missing",
            format!("list them, for example `{label}([email])`"),
        ));
        return None;
    };
    if items.is_empty() {
        d.push(Diagnostic::invalid_attribute(
            *span,
            &label,
            "the list of fields is empty",
        ));
        return None;
    }
    let mut out = Vec::new();
    for item in items {
        let (name, args, span) = match item {
            Expr::Constant(name, span) => (name.as_str(), &[][..], *span),
            Expr::Function(name, args, span) => (name.name.as_str(), args.as_slice(), *span),
            other => {
                d.push(Diagnostic::invalid_attribute(
                    other.span(),
                    &label,
                    "expected a field name",
                ));
                return None;
            }
        };
        // `address.city` reaches into a composite type on MongoDB
        let root = name.split('.').next().unwrap_or(name);
        let Some(index) = fields.iter().position(|f| f.name == root) else {
            let hint = closest(root, fields.iter().map(|f| f.name.as_str()));
            let help = hint.map_or_else(String::new, |h| format!("did you mean `{h}`?"));
            d.push(Diagnostic::invalid_attribute_help(
                span,
                &label,
                format!("`{model}` has no field `{root}`"),
                help,
            ));
            return None;
        };
        let field = &fields[index];
        let nested_ok =
            provider == Provider::MongoDb && matches!(field.kind, FieldKind::Composite(_));
        if !field.is_scalar_like() && !nested_ok {
            d.push(Diagnostic::invalid_attribute(
                span,
                &label,
                format!("`{root}` is a relation field; use its scalar fields instead"),
            ));
            return None;
        }
        let (sort, length) = sort_and_length(args);
        out.push(IndexField {
            field: index,
            sort,
            length,
        });
    }
    Some(out)
}

fn native_type(
    attr: &Attribute,
    field: &Field,
    provider: Provider,
    datasource: &str,
    d: &mut Diagnostics,
) -> Option<NativeType> {
    let full = &attr.name.name;
    let (prefix, name) = full.split_once('.')?;
    if prefix != datasource {
        d.push(Diagnostic::invalid_attribute_help(
            attr.name.span,
            &format!("@{full}"),
            format!("native types use the datasource's name, `{datasource}`"),
            format!("write `@{datasource}.{name}`"),
        ));
        return None;
    }
    let mut args = Vec::new();
    for a in &attr.args {
        match &a.value {
            Expr::Number(n, span) => {
                if let Ok(v) = n.parse() {
                    args.push(v);
                } else {
                    d.push(Diagnostic::invalid_attribute(
                        *span,
                        &format!("@{full}"),
                        "arguments are whole numbers of zero or more",
                    ));
                    return None;
                }
            }
            Expr::Constant(c, _) if c == "Max" => args.push(u32::MAX),
            other => {
                d.push(Diagnostic::invalid_attribute(
                    other.span(),
                    &format!("@{full}"),
                    "arguments are numbers",
                ));
                return None;
            }
        }
    }
    let scalar = match &field.kind {
        FieldKind::Scalar(s) => Some(*s),
        _ => None,
    };
    match native_types::check(provider, name, args.len(), scalar) {
        Ok(()) => Some(NativeType {
            name: name.to_owned(),
            args,
        }),
        Err(native_types::Misfit::Unknown(known)) => {
            let hint = closest(name, known.iter().copied()).map_or_else(
                || format!("known types: {}", known.join(", ")),
                |h| format!("did you mean `@{datasource}.{h}`?"),
            );
            d.push(Diagnostic::invalid_attribute_help(
                attr.name.span,
                &format!("@{full}"),
                "unknown native type for this database",
                hint,
            ));
            None
        }
        Err(native_types::Misfit::Arguments(min, max)) => {
            let wanted = if min == max {
                format!("{min}")
            } else {
                format!("{min} to {max}")
            };
            d.push(Diagnostic::invalid_attribute(
                attr.span,
                &format!("@{full}"),
                format!("takes {wanted} arguments, got {}", args.len()),
            ));
            None
        }
        Err(native_types::Misfit::Scalar(allowed)) => {
            d.push(Diagnostic::invalid_attribute(
                attr.span,
                &format!("@{full}"),
                format!("can only be used on {} fields", allowed.join(" or ")),
            ));
            None
        }
    }
}

struct Context<'a> {
    provider: Provider,
    datasource: String,
    enums: &'a [Enum],
}

/// `@id` or `@unique` on one field.
fn single_field_index(
    field: &Field,
    index: usize,
    attr: &Attribute,
    label: &str,
    d: &mut Diagnostics,
) -> Option<Index> {
    let is_id = attr.name.name == "id";
    if is_id && field.arity != Arity::Required {
        d.push(Diagnostic::invalid_attribute(
            attr.span,
            label,
            "an id field must be required, not optional or a list",
        ));
    }
    if !field.is_scalar_like() {
        d.push(Diagnostic::invalid_attribute(
            attr.span,
            label,
            "only scalar fields can be ids or unique",
        ));
        return None;
    }
    let (sort, length) = sort_and_length(&attr.args);
    Some(Index {
        kind: if is_id {
            IndexKind::PrimaryKey
        } else {
            IndexKind::Unique
        },
        fields: vec![IndexField {
            field: index,
            sort,
            length,
        }],
        client_name: None,
        mapped_name: string_arg(attr, "map", None, d),
        algorithm: None,
        where_clause: None,
        span: attr.span,
    })
}

/// Read one field's attributes; ids and uniques go to `indexes`.
fn field_attributes(
    field: &mut Field,
    index: usize,
    attrs: &[Attribute],
    in_model: bool,
    cx: &Context<'_>,
    indexes: &mut Vec<Index>,
    d: &mut Diagnostics,
) {
    let mut seen: HashSet<String> = HashSet::new();
    for attr in attrs {
        let name = attr.name.name.as_str();
        let base = name.split('.').next().unwrap_or(name);
        let label = format!("@{name}");
        if !seen.insert(name.to_owned()) && base != "db" {
            d.push(Diagnostic::duplicate_attribute(attr.span, &label));
            continue;
        }
        // `@x.Y` is always a native type; the prefix is checked against the datasource there
        let native = name.contains('.');
        let known_here = native
            || if in_model {
                FIELD_ATTRIBUTES.contains(&base) && base != "db"
            } else {
                ["default", "map"].contains(&base)
            };
        if !known_here {
            let hint = closest(name, FIELD_ATTRIBUTES.iter().copied());
            d.push(Diagnostic::unknown_attribute(attr.name.span, &label, hint));
            continue;
        }
        if native {
            field.native_type = native_type(attr, field, cx.provider, &cx.datasource, d);
            continue;
        }
        match base {
            "id" | "unique" => {
                if let Some(i) = single_field_index(field, index, attr, &label, d) {
                    indexes.push(i);
                }
            }
            "default" => match arg(attr, "value", Some(0)) {
                Some(value) => {
                    match defaults::read(value, &field.kind, field.arity, cx.provider, cx.enums) {
                        Ok(v) => field.default = Some(v),
                        Err(msg) => {
                            d.push(Diagnostic::invalid_attribute(value.span(), &label, msg));
                        }
                    }
                }
                None => d.push(Diagnostic::invalid_attribute(
                    attr.span,
                    &label,
                    "the value is missing",
                )),
            },
            "map" => {
                field.mapped_name = string_arg(attr, "name", Some(0), d);
                if cx.provider == Provider::MongoDb
                    && let Some(m) = &field.mapped_name
                    && (m.contains('.') || m.starts_with('$'))
                {
                    d.push(Diagnostic::invalid_attribute(
                        attr.span,
                        &label,
                        "a MongoDB field name cannot contain `.` or start with `$`",
                    ));
                }
            }
            "updatedAt" => {
                if field.kind == FieldKind::Scalar(super::ScalarType::DateTime) {
                    field.updated_at = true;
                } else {
                    d.push(Diagnostic::invalid_attribute(
                        attr.span,
                        &label,
                        "can only be used on `DateTime` fields",
                    ));
                }
            }
            "ignore" => field.ignored = true,
            "relation" => {
                if !matches!(field.kind, FieldKind::Model(_)) {
                    d.push(Diagnostic::invalid_attribute(
                        attr.span,
                        &label,
                        "can only be used on relation fields",
                    ));
                }
            }
            _ => field.native_type = native_type(attr, field, cx.provider, &cx.datasource, d),
        }
    }
}

fn block_attributes(
    model: &mut super::Model,
    attrs: &[Attribute],
    cx: &Context<'_>,
    d: &mut Diagnostics,
) {
    let mut seen: HashSet<&str> = HashSet::new();
    for attr in attrs {
        let name = attr.name.name.as_str();
        let label = format!("@@{name}");
        if ["map", "schema", "ignore", "id"].contains(&name) && !seen.insert(name) {
            d.push(Diagnostic::duplicate_attribute(attr.span, &label));
            continue;
        }
        if arg(attr, "clustered", None).is_some() {
            d.push(Diagnostic::invalid_attribute(
                attr.span,
                &label,
                "clustering is not available on this database",
            ));
        }
        if !MODEL_ATTRIBUTES.contains(&name) {
            let hint = closest(name, MODEL_ATTRIBUTES.iter().copied());
            d.push(Diagnostic::unknown_attribute(attr.name.span, &label, hint));
            continue;
        }
        match name {
            "map" => model.mapped_name = string_arg(attr, "name", Some(0), d),
            "schema" => model.schema = string_arg(attr, "name", Some(0), d),
            "ignore" => model.ignored = true,
            _ => {
                let kind = match name {
                    "id" => IndexKind::PrimaryKey,
                    "unique" => IndexKind::Unique,
                    "fulltext" => IndexKind::Fulltext,
                    _ => IndexKind::Normal,
                };
                let Some(fields) = index_fields(attr, &model.fields, &model.name, cx.provider, d)
                else {
                    continue;
                };
                if kind == IndexKind::PrimaryKey {
                    for f in &fields {
                        if model.fields[f.field].arity != Arity::Required {
                            d.push(Diagnostic::invalid_attribute(
                                attr.span,
                                &label,
                                format!(
                                    "`{}` must be required to be part of the id",
                                    model.fields[f.field].name
                                ),
                            ));
                        }
                    }
                }
                let algorithm = match arg(attr, "type", None) {
                    Some(Expr::Constant(c, _)) => Some(c.clone()),
                    _ => None,
                };
                let where_clause = arg(attr, "where", None).map(|e| format!("{e:?}"));
                model.indexes.push(Index {
                    kind,
                    fields,
                    client_name: string_arg(attr, "name", None, d),
                    mapped_name: string_arg(attr, "map", None, d),
                    algorithm,
                    where_clause,
                    span: attr.span,
                });
            }
        }
    }
    if model.kind == ModelKind::View {
        view_checks(model, d);
    }
    let ids = model
        .indexes
        .iter()
        .filter(|i| i.kind == IndexKind::PrimaryKey)
        .count();
    if ids > 1 {
        d.push(Diagnostic::invalid_attribute(
            model.span,
            "@id",
            format!(
                "`{}` has more than one primary key; use one `@id` or one `@@id([..])`",
                model.name
            ),
        ));
    }
}

/// Pass 2. See the module docs.
pub(super) fn read_all(ast: &ast::SchemaAst, schema: &mut Schema, d: &mut Diagnostics) {
    let enums = schema.enums.clone();
    let cx = Context {
        provider: schema.provider(),
        datasource: schema
            .datasource
            .as_ref()
            .map_or_else(|| "db".to_owned(), |s| s.name.clone()),
        enums: &enums,
    };
    for (model, block) in schema.models.iter_mut().zip(model_asts(ast)) {
        let mut indexes = Vec::new();
        for (i, (field, f_ast)) in model.fields.iter_mut().zip(&block.fields).enumerate() {
            field_attributes(field, i, &f_ast.attributes, true, &cx, &mut indexes, d);
        }
        model.indexes = indexes;
        block_attributes(model, &block.attributes, &cx, d);
    }
    for (ty, block) in schema.composite_types.iter_mut().zip(type_asts(ast)) {
        let mut ignored = Vec::new();
        for (i, (field, f_ast)) in ty.fields.iter_mut().zip(&block.fields).enumerate() {
            field_attributes(field, i, &f_ast.attributes, false, &cx, &mut ignored, d);
        }
    }
    for (e, block) in schema.enums.iter_mut().zip(enum_asts(ast)) {
        for (value, v_ast) in e.values.iter_mut().zip(&block.values) {
            for attr in &v_ast.attributes {
                if attr.name.name == "map" {
                    value.mapped_name = string_arg(attr, "name", Some(0), d);
                } else {
                    d.push(Diagnostic::unknown_attribute(
                        attr.name.span,
                        &format!("@{}", attr.name.name),
                        Some("@map"),
                    ));
                }
            }
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for attr in &block.attributes {
            if !seen.insert(attr.name.name.as_str()) {
                d.push(Diagnostic::duplicate_attribute(
                    attr.span,
                    &format!("@@{}", attr.name.name),
                ));
                continue;
            }
            match attr.name.name.as_str() {
                "map" => e.mapped_name = string_arg(attr, "name", Some(0), d),
                "schema" => e.schema = string_arg(attr, "name", Some(0), d),
                other => d.push(Diagnostic::unknown_attribute(
                    attr.name.span,
                    &format!("@@{other}"),
                    None,
                )),
            }
        }
    }
}

/// Checks on whole models, after relations are known.
fn view_checks(model: &super::Model, d: &mut Diagnostics) {
    for index in &model.indexes {
        let problem = match index.kind {
            IndexKind::PrimaryKey => Some(
                "views have no primary key; use `@unique` to mark a field that identifies a row",
            ),
            IndexKind::Normal | IndexKind::Fulltext => Some("views cannot have indexes"),
            IndexKind::Unique
                if index.mapped_name.is_some()
                    || index
                        .fields
                        .iter()
                        .any(|f| f.length.is_some() || f.sort.is_some()) =>
            {
                Some(
                    "a unique field on a view is not a database index, so it takes no `map`, `length` or `sort`",
                )
            }
            IndexKind::Unique => None,
        };
        if let Some(message) = problem {
            d.push(Diagnostic::invalid_attribute(index.span, "view", message));
        }
    }
}

pub(super) fn check_models(schema: &Schema, d: &mut Diagnostics) {
    let mut enum_names: HashMap<(Option<&str>, &str), &str> = HashMap::new();
    for e in &schema.enums {
        let key = (
            e.schema.as_deref(),
            e.mapped_name.as_deref().unwrap_or(&e.name),
        );
        if let Some(other) = enum_names.insert(key, &e.name) {
            d.push(Diagnostic::invalid_attribute(
                e.span,
                "@@map",
                format!("`{other}` already uses the database name `{}`", key.1),
            ));
        }
    }
    let mut tables: HashMap<(Option<&str>, &str), &str> = HashMap::new();
    let schemas = schema.datasource.as_ref().map(|s| &s.schemas);
    for model in &schema.models {
        let required_unique = model.indexes.iter().any(|i| {
            matches!(i.kind, IndexKind::PrimaryKey | IndexKind::Unique)
                && i.fields
                    .iter()
                    .all(|f| model.fields[f.field].arity == Arity::Required)
        });
        if model.kind == ModelKind::Model && !model.ignored && !required_unique {
            d.push(Diagnostic::no_unique_criteria(model.span, &model.name));
        }
        let key = (model.schema.as_deref(), model.database_name());
        if let Some(other) = tables.insert(key, &model.name) {
            d.push(Diagnostic::duplicate_table(
                model.span,
                model.database_name(),
                other,
            ));
        }
        if let Some(list) = schemas.filter(|l| !l.is_empty()) {
            match &model.schema {
                None => d.push(Diagnostic::invalid_attribute_help(
                    model.span,
                    "@@schema",
                    format!(
                        "`{}` needs `@@schema(\"..\")` because the datasource lists schemas",
                        model.name
                    ),
                    format!("add `@@schema(\"{}\")`", list[0]),
                )),
                Some(s) if !list.contains(s) => d.push(Diagnostic::invalid_attribute(
                    model.span,
                    "@@schema",
                    format!("`{s}` is not in the datasource's `schemas`"),
                )),
                Some(_) => {}
            }
        }
        if schema.provider() == Provider::MongoDb
            && let Some(pk) = model.primary_key()
            && pk.fields.len() == 1
            && model.fields[pk.fields[0].field].database_name() != "_id"
        {
            d.push(Diagnostic::invalid_attribute_help(
                model.fields[pk.fields[0].field].span,
                "@id",
                "a MongoDB id field is stored as `_id`",
                "add `@map(\"_id\")` to the id field",
            ));
        }
    }
}
