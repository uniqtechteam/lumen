//! SPEC-QUERY-001 FR-002 relation filters in the generated client: one module per relation field,
//! next to the scalar field modules. A to-many field gets `some`, `every` and `none`
//! (`user::posts::some(post::published::equals(true))`); a to-one field gets `is` and `is_not`, and
//! when it is optional also `is_null` and `is_not_null`.

use lumen_schema::ast::Arity;
use lumen_schema::checked::{FieldKind, Model, ModelId, ModelKind, RelationKind, Schema};
use proc_macro2::TokenStream;
use quote::quote;

use crate::types::{ident, snake};

/// How a relation field reaches its related rows.
enum Link {
    /// `(related column, this column)` pairs.
    Direct(Vec<(String, String)>),
    /// Through the implicit many-to-many join table: its name, this side's column (`A` or `B`) and
    /// id, the related side's column and id.
    Through {
        schema: Option<String>,
        table: String,
        this: (String, String),
        related: (String, String),
    },
}

/// The single-column id of a model, as a column name.
fn single_id(model: &Model) -> Option<String> {
    match model.primary_key()?.fields.as_slice() {
        [f] => Some(model.fields[f.field].database_name().to_owned()),
        _ => None,
    }
}

fn link(schema: &Schema, model_id: ModelId, field: usize, mongo: bool) -> Option<Link> {
    let model = &schema.models[model_id.0];
    let r = &schema.relations[model.fields[field].relation?.0];
    let (a, b) = (&schema.models[r.a.model.0], &schema.models[r.b.model.0]);
    let on_a = r.a.model == model_id && r.a.field == field;
    if r.kind == RelationKind::ManyToMany {
        if mongo {
            return None;
        }
        let (ida, idb) = (single_id(a)?, single_id(b)?);
        let (this, related) = if on_a {
            (("A".to_owned(), ida), ("B".to_owned(), idb))
        } else {
            (("B".to_owned(), idb), ("A".to_owned(), ida))
        };
        // the join table lives with the first model, as migrations create it
        return Some(Link::Through {
            schema: a.schema.clone(),
            table: format!("_{}", r.name),
            this,
            related,
        });
    }
    let pairs = r
        .fields
        .iter()
        .zip(&r.references)
        .map(|(&fk, &referenced)| {
            let (fk, referenced) = (
                a.fields[fk].database_name().to_owned(),
                b.fields[referenced].database_name().to_owned(),
            );
            // this side holds the key: the related rows are the referenced ones
            if on_a {
                (referenced, fk)
            } else {
                (fk, referenced)
            }
        });
    Some(Link::Direct(pairs.collect()))
}

fn table_expr(schema: &Schema, target: ModelId, mongo: bool) -> TokenStream {
    let m = &schema.models[target.0];
    if mongo {
        let name = m.database_name();
        quote!(::lumen_orm::Table::new(#name))
    } else {
        let module = ident(&snake(&m.name));
        quote!(super::super::#module::table())
    }
}

fn related_expr(schema: &Schema, target: ModelId, link: &Link, mongo: bool) -> TokenStream {
    let table = table_expr(schema, target, mongo);
    match link {
        Link::Direct(pairs) => {
            let pairs = pairs.iter().map(|(r, t)| quote!((#r, #t)));
            quote!(::lumen_orm::Related::new(#table, &[#(#pairs),*]))
        }
        Link::Through {
            schema: join_schema,
            table: join,
            this: (jt, t),
            related: (jr, r),
        } => {
            let schema_name = join_schema
                .as_ref()
                .map_or_else(|| quote!(None), |s| quote!(Some(#s.to_owned())));
            quote!(::lumen_orm::Related::through(
                #table,
                ::lumen_orm::Table { schema: #schema_name, name: #join.to_owned() },
                &[(#jt, #t)],
                &[(#jr, #r)],
            ))
        }
    }
}

/// One module per relation field of `model_id` (`mongo`: collection names, no many-to-many).
pub(crate) fn modules(schema: &Schema, model_id: ModelId, mongo: bool) -> Vec<TokenStream> {
    let model = &schema.models[model_id.0];
    let mut out = Vec::new();
    for (i, f) in model.fields.iter().enumerate() {
        let FieldKind::Model(target) = f.kind else {
            continue;
        };
        let t = &schema.models[target.0];
        if f.ignored || t.ignored || t.kind != ModelKind::Model {
            continue;
        }
        let Some(link) = link(schema, model_id, i, mongo) else {
            continue;
        };
        let related = related_expr(schema, target, &link, mongo);
        let target_name = &t.name;
        let fns = if f.arity == Arity::List {
            quote! {
                /// At least one related record matches `f`.
                pub fn some(f: ::lumen_orm::Filter) -> ::lumen_orm::Filter { related().some(f) }
                /// Every related record matches `f` (true when there are none).
                pub fn every(f: ::lumen_orm::Filter) -> ::lumen_orm::Filter { related().every(f) }
                /// No related record matches `f`.
                pub fn none(f: ::lumen_orm::Filter) -> ::lumen_orm::Filter { related().none(f) }
            }
        } else {
            let optional = (f.arity == Arity::Optional).then(|| {
                quote! {
                    /// There is no related record.
                    pub fn is_null() -> ::lumen_orm::Filter { related().is_null() }
                    /// There is a related record.
                    pub fn is_not_null() -> ::lumen_orm::Filter { related().is_not_null() }
                }
            });
            quote! {
                /// The related record exists and matches `f`.
                pub fn is(f: ::lumen_orm::Filter) -> ::lumen_orm::Filter { related().is(f) }
                /// There is no related record, or it does not match `f`.
                pub fn is_not(f: ::lumen_orm::Filter) -> ::lumen_orm::Filter { related().is_not(f) }
                #optional
            }
        };
        let module = ident(&snake(&f.name));
        let doc = format!(
            " Conditions on the related `{target_name}` records of `{}`, built with `{}::..` filters.",
            f.name,
            snake(target_name)
        );
        out.push(quote! {
            #[doc = #doc]
            pub mod #module {
                fn related() -> ::lumen_orm::Related { #related }
                #fns
            }
        });
    }
    out
}
