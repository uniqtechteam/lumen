//! Pass 3: pair relation fields and check them, with the same rules as Prisma so a schema means the
//! same thing in both: every relation field needs its opposite, the side with `fields` holds the
//! foreign key, list ↔ list is an implicit many-to-many through `_AToB`.

use std::collections::BTreeMap;

use super::resolve::model_asts;
use super::{
    FieldKind, FieldRef, ModelId, Provider, ReferentialAction, Relation, RelationId, RelationKind,
    Schema,
};
use crate::ast::{self, Arity, Expr};
use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::span::Span;

/// What one relation field says in its `@relation(..)`.
#[derive(Debug, Clone)]
struct Side {
    at: FieldRef,
    target: ModelId,
    name: Option<String>,
    fields: Vec<(String, Span)>,
    references: Vec<(String, Span)>,
    on_delete: Option<(ReferentialAction, Span)>,
    on_update: Option<(ReferentialAction, Span)>,
    map: Option<String>,
    span: Span,
}

fn names(expr: Option<&Expr>) -> Vec<(String, Span)> {
    match expr {
        Some(Expr::Array(items, _)) => items
            .iter()
            .filter_map(|e| match e {
                Expr::Constant(c, s) => Some((c.clone(), *s)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn action(expr: Option<&Expr>, d: &mut Diagnostics) -> Option<(ReferentialAction, Span)> {
    let Expr::Constant(c, span) = expr? else {
        return None;
    };
    let a = match c.as_str() {
        "Cascade" => ReferentialAction::Cascade,
        "Restrict" => ReferentialAction::Restrict,
        "NoAction" => ReferentialAction::NoAction,
        "SetNull" => ReferentialAction::SetNull,
        "SetDefault" => ReferentialAction::SetDefault,
        other => {
            d.push(Diagnostic::invalid_attribute(
                *span,
                "@relation",
                format!("`{other}` is not a referential action (Cascade, Restrict, NoAction, SetNull, SetDefault)"),
            ));
            return None;
        }
    };
    Some((a, *span))
}

fn read_side(at: FieldRef, target: ModelId, field: &ast::Field, d: &mut Diagnostics) -> Side {
    let mut side = Side {
        at,
        target,
        name: None,
        fields: Vec::new(),
        references: Vec::new(),
        on_delete: None,
        on_update: None,
        map: None,
        span: field.span,
    };
    let Some(attr) = field.attributes.iter().find(|a| a.name.name == "relation") else {
        return side;
    };
    for (i, a) in attr.args.iter().enumerate() {
        let key = a.name.as_ref().map(|n| n.name.as_str());
        match (key, &a.value) {
            (None | Some("name"), Expr::String(s, _)) if key.is_some() || i == 0 => {
                side.name = Some(s.clone());
            }
            (Some("fields"), v) => side.fields = names(Some(v)),
            (Some("references"), v) => side.references = names(Some(v)),
            (Some("onDelete"), v) => side.on_delete = action(Some(v), d),
            (Some("onUpdate"), v) => side.on_update = action(Some(v), d),
            (Some("map"), Expr::String(s, _)) => side.map = Some(s.clone()),
            _ => d.push(Diagnostic::invalid_attribute(
                a.span,
                "@relation",
                "unexpected argument",
            )),
        }
    }
    side
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_lowercase().chain(c).collect())
}

/// What to add on the other model: `posts Post[]` or `author User?`.
fn suggestion(schema: &Schema, side: &Side) -> String {
    let source = &schema.models[side.at.model.0].name;
    let field = schema.field(side.at);
    if field.arity == Arity::List {
        format!("{} {source}?", lower_first(source))
    } else {
        format!("{}s {source}[]", lower_first(source))
    }
}

fn resolve_names(
    schema: &Schema,
    model: ModelId,
    names: &[(String, Span)],
    d: &mut Diagnostics,
) -> Option<Vec<usize>> {
    let m = &schema.models[model.0];
    let mut out = Vec::new();
    for (name, span) in names {
        match m.field(name) {
            Some((i, f)) if f.is_scalar_like() => out.push(i),
            Some(_) => {
                d.push(Diagnostic::relation(
                    *span,
                    format!("`{name}` on `{}` is not a scalar field", m.name),
                ));
                return None;
            }
            None => {
                d.push(Diagnostic::relation(
                    *span,
                    format!("`{}` has no field `{name}`", m.name),
                ));
                return None;
            }
        }
    }
    Some(out)
}

/// Check the foreign key on `a` (the side with `fields`) pointing at `b`.
fn check_foreign_key(
    schema: &Schema,
    a: &Side,
    kind: RelationKind,
    d: &mut Diagnostics,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let label = |s: &Schema| {
        format!(
            "`{}` on `{}`",
            s.field(a.at).name,
            s.models[a.at.model.0].name
        )
    };
    if a.fields.len() != a.references.len() {
        d.push(Diagnostic::relation(
            a.span,
            format!(
                "{}: `fields` and `references` must list the same number of fields",
                label(schema)
            ),
        ));
        return None;
    }
    let fields = resolve_names(schema, a.at.model, &a.fields, d)?;
    let references = resolve_names(schema, a.target, &a.references, d)?;
    let target = &schema.models[a.target.0];
    if !target.is_unique_set(&references) {
        d.push(Diagnostic::relation_help(
            a.span,
            format!(
                "{}: the referenced fields are not unique on `{}`",
                label(schema),
                target.name
            ),
            "reference the id, or fields marked `@unique` / `@@unique` on the other model",
        ));
    }
    let own = &schema.models[a.at.model.0];
    for (&f, &r) in fields.iter().zip(&references) {
        if own.fields[f].kind != target.fields[r].kind {
            d.push(Diagnostic::relation(
                a.span,
                format!(
                    "{}: `{}` and `{}.{}` have different types",
                    label(schema),
                    own.fields[f].name,
                    target.name,
                    target.fields[r].name
                ),
            ));
        }
    }
    let relation_required = schema.field(a.at).arity == Arity::Required;
    let any_fk_required = fields
        .iter()
        .any(|&f| own.fields[f].arity == Arity::Required);
    if relation_required
        && fields
            .iter()
            .any(|&f| own.fields[f].arity != Arity::Required)
    {
        d.push(Diagnostic::relation_help(
            a.span,
            format!(
                "{}: the relation field is required but a foreign key field is optional",
                label(schema)
            ),
            "make the foreign key fields required, or the relation field optional",
        ));
    }
    if kind == RelationKind::OneToOne && !own.is_unique_set(&fields) {
        d.push(Diagnostic::relation_help(
            a.span,
            format!(
                "{}: a one-to-one relation needs its foreign key to be unique",
                label(schema)
            ),
            "add `@unique` to the foreign key field (or `@@unique` for several)",
        ));
    }
    for (action, name) in [(a.on_delete, "onDelete"), (a.on_update, "onUpdate")] {
        if let Some((ReferentialAction::SetNull, span)) = action
            && any_fk_required
        {
            d.push(Diagnostic::warning_help(
                span,
                format!("`{name}: SetNull` will fail at run time because a foreign key field is required"),
                "choose another action, or make the foreign key fields optional",
            ));
        }
    }
    Some((fields, references))
}

/// Check one side of a MongoDB many-to-many: `fields` names one list field on its own model holding
/// the values of one unique field of the other model (`fields: [tagIds], references: [id]`).
fn check_id_list(
    schema: &Schema,
    side: &Side,
    d: &mut Diagnostics,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let label = format!(
        "`{}` on `{}`",
        schema.field(side.at).name,
        schema.models[side.at.model.0].name
    );
    if side.fields.len() != 1 || side.references.len() != 1 {
        d.push(Diagnostic::relation_help(
            side.span,
            format!("{label}: a many-to-many relation on MongoDB names one id list and one referenced field"),
            "write `@relation(fields: [otherIds], references: [id])`",
        ));
        return None;
    }
    let fields = resolve_names(schema, side.at.model, &side.fields, d)?;
    let references = resolve_names(schema, side.target, &side.references, d)?;
    let list = &schema.models[side.at.model.0].fields[fields[0]];
    let target = &schema.models[side.target.0];
    let key = &target.fields[references[0]];
    if list.arity != Arity::List {
        d.push(Diagnostic::relation_help(
            side.span,
            format!("{label}: `{}` must be a list of ids", list.name),
            format!(
                "declare it as a list, for example `{} String[] @db.ObjectId`",
                list.name
            ),
        ));
        return None;
    }
    if list.kind != key.kind {
        d.push(Diagnostic::relation(
            side.span,
            format!(
                "{label}: `{}` and `{}.{}` have different types",
                list.name, target.name, key.name
            ),
        ));
        return None;
    }
    if !target.is_unique_set(&references) {
        d.push(Diagnostic::relation_help(
            side.span,
            format!(
                "{label}: the referenced field is not unique on `{}`",
                target.name
            ),
            "reference the id, or a field marked `@unique` on the other model",
        ));
    }
    Some((fields, references))
}

fn has_fk(side: &Side) -> bool {
    !side.fields.is_empty() || !side.references.is_empty()
}

/// Decide the kind and which side holds the foreign key; `None` after reporting a problem.
fn classify(
    schema: &Schema,
    x: &Side,
    y: &Side,
    d: &mut Diagnostics,
) -> Option<(RelationKind, usize)> {
    let x_list = schema.field(x.at).arity == Arity::List;
    let y_list = schema.field(y.at).arity == Arity::List;
    let model_name = |s: &Side| &schema.models[s.at.model.0].name;
    match (x_list, y_list) {
        (true, true) => {
            if [x, y]
                .iter()
                .any(|s| schema.models[s.at.model.0].kind == super::ModelKind::View)
            {
                d.push(Diagnostic::relation(
                    x.span,
                    "views cannot be part of a many-to-many relation",
                ));
                return None;
            }
            if schema.provider() == Provider::MongoDb {
                if let Some(side) = [x, y].into_iter().find(|s| !has_fk(s)) {
                    let target = &schema.models[side.target.0];
                    let list = format!("{}Ids", lower_first(&target.name));
                    d.push(Diagnostic::relation_help(
                        side.span,
                        "a many-to-many relation on MongoDB keeps a list of ids on both sides",
                        format!(
                            "add a `{list}` list field and `@relation(fields: [{list}], references: [id])`"
                        ),
                    ));
                    return None;
                }
            } else if has_fk(x) || has_fk(y) {
                d.push(Diagnostic::relation(
                    x.span,
                    "an implicit many-to-many relation cannot have `fields` or `references`",
                ));
                return None;
            }
            // model A is the first model by name
            let a = usize::from(model_name(x) > model_name(y));
            Some((RelationKind::ManyToMany, a))
        }
        (false, true) | (true, false) => {
            let (single, list, a) = if x_list { (y, x, 1) } else { (x, y, 0) };
            if has_fk(list) {
                d.push(Diagnostic::relation(
                    list.span,
                    "the list side of a relation cannot have `fields` or `references`",
                ));
                return None;
            }
            if !has_fk(single) {
                let target = &schema.models[single.target.0];
                let pk = target
                    .primary_key()
                    .and_then(|p| p.fields.first())
                    .map_or("id", |f| target.fields[f.field].name.as_str());
                let fk = format!("{}{}", schema.field(single.at).name, capitalize(pk));
                d.push(Diagnostic::relation_help(
                    single.span,
                    format!(
                        "the relation field `{}` on `{}` needs `fields` and `references`",
                        schema.field(single.at).name,
                        model_name(single)
                    ),
                    format!(
                        "add `@relation(fields: [{fk}], references: [{pk}])` and a `{fk}` field"
                    ),
                ));
                return None;
            }
            Some((RelationKind::OneToMany, a))
        }
        (false, false) => match (has_fk(x), has_fk(y)) {
            (true, false) => Some((RelationKind::OneToOne, 0)),
            (false, true) => Some((RelationKind::OneToOne, 1)),
            (true, true) => {
                d.push(Diagnostic::relation(
                    y.span,
                    "only one side of a one-to-one relation may have `fields` and `references`",
                ));
                None
            }
            (false, false) => {
                d.push(Diagnostic::relation_help(
                    x.span,
                    "one side of a one-to-one relation must have `fields` and `references`",
                    "add `@relation(fields: [..], references: [..])` to the side that should hold the foreign key",
                ));
                None
            }
        },
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// Pass 3. See the module docs.
pub(super) fn pair_all(ast: &ast::SchemaAst, schema: &mut Schema, d: &mut Diagnostics) {
    // (lower model, higher model, relation name) -> sides
    let mut groups: BTreeMap<(ModelId, ModelId, String), Vec<Side>> = BTreeMap::new();
    for (mi, block) in model_asts(ast).iter().enumerate() {
        for (fi, f) in block.fields.iter().enumerate() {
            let FieldKind::Model(target) = schema.models[mi].fields[fi].kind else {
                continue;
            };
            let side = read_side(
                FieldRef {
                    model: ModelId(mi),
                    field: fi,
                },
                target,
                f,
                d,
            );
            let low = side.at.model.min(target);
            let high = side.at.model.max(target);
            groups
                .entry((low, high, side.name.clone().unwrap_or_default()))
                .or_default()
                .push(side);
        }
    }
    for ((low, high, name), sides) in groups {
        let (left, right) = match sides.as_slice() {
            [l, r] if low == high || l.at.model != r.at.model => (l.clone(), r.clone()),
            [only] => {
                let target = &schema.models[only.target.0].name;
                d.push(Diagnostic::missing_opposite(
                    only.span,
                    &schema.field(only.at).name,
                    &schema.models[only.at.model.0].name,
                    target,
                    &suggestion(schema, only),
                ));
                continue;
            }
            _ => {
                for s in &sides {
                    d.push(Diagnostic::relation_help(
                        s.span,
                        format!(
                            "`{}` and `{}` have several relations between them; this one is ambiguous",
                            schema.models[low.0].name, schema.models[high.0].name
                        ),
                        "give each pair of fields the same name with `@relation(\"..\")`",
                    ));
                }
                continue;
            }
        };
        let Some((kind, a_index)) = classify(schema, &left, &right, d) else {
            continue;
        };
        let (holder, other) = if a_index == 0 {
            (left, right)
        } else {
            (right, left)
        };
        let ((fields, references), (b_fields, b_references)) = if kind != RelationKind::ManyToMany {
            match check_foreign_key(schema, &holder, kind, d) {
                Some(pair) => (pair, (Vec::new(), Vec::new())),
                None => continue,
            }
        } else if schema.provider() == Provider::MongoDb {
            let a = check_id_list(schema, &holder, d);
            let b = check_id_list(schema, &other, d);
            match (a, b) {
                (Some(a), Some(b)) => (a, b),
                _ => continue,
            }
        } else {
            ((Vec::new(), Vec::new()), (Vec::new(), Vec::new()))
        };
        let default_name = {
            let mut models = [
                &schema.models[holder.at.model.0].name,
                &schema.models[other.at.model.0].name,
            ];
            models.sort();
            format!("{}To{}", models[0], models[1])
        };
        let id = RelationId(schema.relations.len());
        schema.models[holder.at.model.0].fields[holder.at.field].relation = Some(id);
        schema.models[other.at.model.0].fields[other.at.field].relation = Some(id);
        schema.relations.push(Relation {
            name: if name.is_empty() { default_name } else { name },
            kind,
            a: holder.at,
            b: other.at,
            fields,
            references,
            b_fields,
            b_references,
            on_delete: holder.on_delete.map(|(action, _)| action),
            on_update: holder.on_update.map(|(action, _)| action),
            mapped_name: holder.map,
        });
    }
}
