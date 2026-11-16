//! Nested writes in the generated client (SPEC-QUERY-001 FR-005): `create_nested` creates a record
//! with related records created or connected, and `create_with_<relation>` creates a record under a
//! related record it creates or connects first. Everything runs in one transaction (a savepoint
//! inside one), and the order follows the foreign key: the record that holds it is written after
//! the record it points at. The same code serves Postgres and MongoDB, since every step is one of
//! the generated actions.

use lumen_schema::ast::Arity;
use lumen_schema::checked::{Field, FieldKind, ModelId, ModelKind, RelationKind, Schema};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::types::{base_type, ident, pascal, snake};

/// One relation a nested write can follow: one-to-one or one-to-many on a single-column key.
struct Link {
    /// The relation field on this model.
    field: String,
    target: ModelId,
    /// Whether this model holds the foreign key.
    holds_key: bool,
    /// A to-many relation (only on the side without the key).
    many: bool,
    /// The relation field on the side holding the key (names `CreateWithout<..>`).
    holder_field: String,
    /// The foreign key field, on the side holding it.
    fk: String,
    fk_optional: bool,
    /// The referenced field, on the other side.
    referenced: String,
}

fn generated(schema: &Schema, id: ModelId) -> bool {
    let m = &schema.models[id.0];
    m.kind == ModelKind::Model && !m.ignored
}

fn links(schema: &Schema, model_id: ModelId) -> Vec<Link> {
    let model = &schema.models[model_id.0];
    let mut out = Vec::new();
    for (index, f) in model.fields.iter().enumerate() {
        let (FieldKind::Model(target), Some(rel)) = (&f.kind, f.relation) else {
            continue;
        };
        let r = &schema.relations[rel.0];
        if f.ignored
            || r.kind == RelationKind::ManyToMany
            || r.fields.len() != 1
            || !generated(schema, *target)
        {
            continue;
        }
        let holder = &schema.models[r.a.model.0];
        let fk = &holder.fields[r.fields[0]];
        let referenced = &schema.models[r.b.model.0].fields[r.references[0]];
        let same_type = base_type(schema, fk).map(|t| t.to_string())
            == base_type(schema, referenced).map(|t| t.to_string());
        let typed = base_type(schema, fk).is_some();
        if !same_type || !typed || fk.ignored || referenced.arity != Arity::Required {
            continue;
        }
        let holds_key = r.a.model == model_id && r.a.field == index;
        out.push(Link {
            field: f.name.clone(),
            target: *target,
            holds_key,
            many: f.arity == Arity::List,
            holder_field: holder.fields[r.a.field].name.clone(),
            fk: fk.name.clone(),
            fk_optional: fk.arity == Arity::Optional,
            referenced: referenced.name.clone(),
        });
    }
    out
}

/// What differs between the providers: the traits and the transaction type.
struct Provider {
    run: TokenStream,
    atomic: TokenStream,
    tx: TokenStream,
}

impl Provider {
    fn of(mongo: bool) -> Self {
        if mongo {
            Self {
                run: quote!(::lumen_orm::mongo::MongoRun),
                atomic: quote!(::lumen_orm::nested::MongoAtomic),
                tx: quote!(::lumen_orm::mongo::MongoTx),
            }
        } else {
            Self {
                run: quote!(::lumen_orm::Run),
                atomic: quote!(::lumen_orm::nested::Atomic),
                tx: quote!(::lumen_orm::Tx<'_>),
            }
        }
    }
}

fn without_name(holder_field: &str) -> proc_macro2::Ident {
    format_ident!("CreateWithout{}", pascal(holder_field))
}

/// `CreateWithoutAuthor`: `Create` without the key of one relation, for records created inside a
/// nested write that fills it.
fn create_without(
    schema: &Schema,
    model_id: ModelId,
    fields: &[(&Field, TokenStream)],
    link: &Link,
) -> Option<TokenStream> {
    let model = &schema.models[model_id.0];
    let fk_field = model.fields.iter().find(|f| f.name == link.fk)?;
    let key_ty = base_type(schema, fk_field)?;
    let name = without_name(&link.holder_field);
    let fk = ident(&snake(&link.fk));
    let rest: Vec<_> = fields.iter().filter(|(f, _)| f.name != link.fk).collect();
    let decls = rest.iter().map(|(f, ty)| {
        let id = ident(&snake(&f.name));
        quote!(pub #id: #ty,)
    });
    let moves = rest.iter().map(|(f, _)| {
        let id = ident(&snake(&f.name));
        quote!(#id: self.#id,)
    });
    let key = if link.fk_optional {
        quote!(#fk: Some(#fk),)
    } else {
        quote!(#fk,)
    };
    let doc = format!(
        " `Create` without `{}`, for a record created through its `{}`, which fills the key.",
        link.fk, link.holder_field
    );
    let into_doc = format!(" The full `Create`, with `{}` set.", link.fk);
    Some(quote! {
        #[doc = #doc]
        #[derive(Debug, Clone, PartialEq)]
        pub struct #name { #(#decls)* }

        impl #name {
            #[doc = #into_doc]
            #[must_use]
            pub fn into_create(self, #fk: #key_ty) -> Create {
                Create { #key #(#moves)* }
            }
        }
    })
}

/// The steps for one relation on the side without the key, once `record` exists.
fn child_steps(schema: &Schema, link: &Link) -> TokenStream {
    let target = &schema.models[link.target.0];
    let module = ident(&snake(&target.name));
    let field = ident(&snake(&link.field));
    let referenced = ident(&snake(&link.referenced));
    let fk = ident(&snake(&link.fk));
    let set = if link.fk_optional {
        quote!(Some(record.#referenced.clone()))
    } else {
        quote!(record.#referenced.clone())
    };
    let missing = format!("{} record to connect", target.name);
    let connect = quote! {
        super::#module::on(&mut *tx)
            .update(w, super::#module::Update { #fk: ::lumen_orm::Set::To(#set), ..Default::default() })
            .await
            .map_err(|e| if e.is_not_found() { ::lumen_orm::Error::not_found(#missing) } else { e })?;
    };
    if link.many {
        quote! {
            if !#field.create.is_empty() {
                let rows = #field.create.into_iter().map(|c| c.into_create(record.#referenced.clone())).collect();
                super::#module::on(&mut *tx).create_many(rows).await?;
            }
            for w in #field.connect { #connect }
        }
    } else {
        quote! {
            match #field {
                Some(::lumen_orm::nested::One::Create(c)) => {
                    super::#module::on(&mut *tx).create(c.into_create(record.#referenced.clone())).await?;
                }
                Some(::lumen_orm::nested::One::Connect(w)) => { #connect }
                None => {}
            }
        }
    }
}

/// `Nested`, `create_nested` and its steps, for the relations whose key is on the other side.
fn create_nested(
    schema: &Schema,
    model_id: ModelId,
    children: &[&Link],
    p: &Provider,
) -> TokenStream {
    let model = &schema.models[model_id.0];
    let record = ident(&model.name);
    let (run, atomic, tx) = (&p.run, &p.atomic, &p.tx);
    let names: Vec<_> = children.iter().map(|l| ident(&snake(&l.field))).collect();
    let decls = children.iter().zip(&names).map(|(l, name)| {
        let target = &schema.models[l.target.0];
        let module = ident(&snake(&target.name));
        let without = without_name(&l.holder_field);
        let doc = if l.many {
            format!(" `{}`: related records to create or connect.", l.field)
        } else {
            format!(" `{}`: the related record to create or connect.", l.field)
        };
        let inner = quote!(super::#module::#without, super::#module::UniqueWhere);
        if l.many {
            quote!(#[doc = #doc] pub #name: ::lumen_orm::nested::Many<#inner>,)
        } else {
            quote!(#[doc = #doc] pub #name: Option<::lumen_orm::nested::One<#inner>>,)
        }
    });
    let steps = children.iter().map(|l| child_steps(schema, l));
    quote! {
        /// Related records written with [`Actions::create_nested`], in the same transaction. Empty
        /// by default.
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct Nested { #(#decls)* }

        async fn write_nested(tx: &mut #tx, data: Create, nested: Nested) -> ::lumen_orm::Result<#record> {
            let record = on(&mut *tx).create(data).await?;
            let Nested { #(#names),* } = nested;
            #(#steps)*
            Ok(record)
        }

        impl<R: #run + #atomic> Actions<R> {
            /// Store a new record and create or connect related records, in one transaction:
            /// when any step fails, nothing is kept.
            ///
            /// # Errors
            /// P2025 when a record to connect does not exist; P2002 when a unique field is taken;
            /// a coded error otherwise.
            pub async fn create_nested(mut self, data: Create, nested: Nested) -> ::lumen_orm::Result<#record> {
                #atomic::atomic(&mut self.run, async move |tx| write_nested(tx, data, nested).await).await
            }
        }
    }
}

/// `create_with_<relation>` for a relation whose key is on this model.
fn create_with(schema: &Schema, model_id: ModelId, link: &Link, p: &Provider) -> TokenStream {
    let record = ident(&schema.models[model_id.0].name);
    let target = &schema.models[link.target.0];
    let module = ident(&snake(&target.name));
    let referenced = ident(&snake(&link.referenced));
    let field = ident(&snake(&link.field));
    let method = format_ident!("create_with_{}", snake(&link.field));
    let without = without_name(&link.holder_field);
    let missing = format!("{} record to connect", target.name);
    let (run, atomic) = (&p.run, &p.atomic);
    let doc = format!(
        " Store a new record under its `{}`, which is created or connected first, in one",
        link.field
    );
    quote! {
        impl<R: #run + #atomic> Actions<R> {
            #[doc = #doc]
            /// transaction: when any step fails, nothing is kept.
            ///
            /// # Errors
            /// P2025 when the record to connect does not exist; P2002 when a unique field is taken;
            /// a coded error otherwise.
            pub async fn #method(
                mut self,
                data: #without,
                #field: ::lumen_orm::nested::One<super::#module::Create, super::#module::UniqueWhere>,
            ) -> ::lumen_orm::Result<#record> {
                #atomic::atomic(&mut self.run, async move |tx| {
                    let key = match #field {
                        ::lumen_orm::nested::One::Create(c) => super::#module::on(&mut *tx).create(c).await?.#referenced,
                        ::lumen_orm::nested::One::Connect(w) => super::#module::on(&mut *tx)
                            .find_unique(w)
                            .await?
                            .ok_or_else(|| ::lumen_orm::Error::not_found(#missing))?
                            .#referenced,
                    };
                    on(&mut *tx).create(data.into_create(key)).await
                })
                .await
            }
        }
    }
}

/// The nested-write code of one model's module. `fields` are the fields of its `Create` with their
/// types there.
pub(crate) fn code(
    schema: &Schema,
    model_id: ModelId,
    fields: &[(&Field, TokenStream)],
    mongo: bool,
) -> TokenStream {
    let p = Provider::of(mongo);
    let all = links(schema, model_id);
    let mut out = Vec::new();
    for link in all.iter().filter(|l| l.holds_key) {
        if let Some(without) = create_without(schema, model_id, fields, link) {
            out.push(without);
            out.push(create_with(schema, model_id, link, &p));
        }
    }
    let children: Vec<&Link> = all.iter().filter(|l| !l.holds_key).collect();
    if !children.is_empty() {
        out.push(create_nested(schema, model_id, &children, &p));
    }
    quote!(#(#out)*)
}
