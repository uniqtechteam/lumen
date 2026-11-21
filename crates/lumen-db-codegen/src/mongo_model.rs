//! The MongoDB client: the same API as on Postgres (`find_many().filter(..).include_x()`, `create`,
//! `update`, ...), with records that are plain serde documents, defaults filled by the client
//! (MongoDB has none) and relations loaded with `$in` per level.

use lumen_schema::ast::Arity;
use lumen_schema::checked::{
    CompositeType, DefaultValue, Field, FieldKind, Literal, Model, ModelId, ScalarType, Schema,
};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::model::{Include, doc, field_module, includes, unique_where, update_input};
use crate::types::{base_type, field_type, ident, pascal, snake};

fn serde_field(schema: &Schema, f: &Field, depth: usize) -> Option<TokenStream> {
    let ty = match f.arity {
        Arity::Required => crate::types::base_type_at(schema, f, depth)?,
        Arity::Optional => {
            let b = crate::types::base_type_at(schema, f, depth)?;
            quote!(Option<#b>)
        }
        Arity::List => {
            let b = crate::types::base_type_at(schema, f, depth)?;
            quote!(Vec<#b>)
        }
    };
    let id = ident(&snake(&f.name));
    let key = f.database_name();
    let d = doc(f.doc.as_deref());
    let missing_ok = (f.arity != Arity::Required).then(|| quote!(#[serde(default)]));
    Some(quote!(#d #[serde(rename = #key)] #missing_ok pub #id: #ty,))
}

/// A composite type: an embedded document.
fn composite(schema: &Schema, t: &CompositeType) -> TokenStream {
    let name = ident(&t.name);
    let d = doc(t.doc.as_deref());
    let fields = t.fields.iter().filter_map(|f| serde_field(schema, f, 0));
    quote! {
        #d
        #[derive(Debug, Clone, PartialEq, ::lumen_orm::serde::Serialize, ::lumen_orm::serde::Deserialize)]
        #[serde(crate = "::lumen_orm::serde")]
        pub struct #name { #(#fields)* }

        impl From<#name> for ::lumen_orm::Value {
            fn from(v: #name) -> Self {
                ::lumen_orm::serde_json::to_value(v).map_or(::lumen_orm::Value::Null, ::lumen_orm::Value::Json)
            }
        }
    }
}

/// The composite types, at the top of the file.
pub fn composites(schema: &Schema) -> TokenStream {
    let items = schema.composite_types.iter().map(|t| composite(schema, t));
    quote!(#(#items)*)
}

fn record(schema: &Schema, model: &Model) -> TokenStream {
    let name = ident(&model.name);
    let d = doc(model.doc.as_deref());
    let fields = model
        .fields
        .iter()
        .filter(|f| !f.ignored)
        .filter_map(|f| serde_field(schema, f, 1));
    quote! {
        #d
        #[derive(Debug, Clone, PartialEq, ::lumen_orm::serde::Serialize, ::lumen_orm::serde::Deserialize)]
        #[serde(crate = "::lumen_orm::serde")]
        pub struct #name { #(#fields)* }
    }
}

/// A literal default as a Rust expression of the field's type.
fn literal(schema: &Schema, f: &Field, l: &Literal) -> Option<TokenStream> {
    Some(match (l, &f.kind) {
        (Literal::String(s), FieldKind::Scalar(ScalarType::String)) => quote!(#s.to_owned()),
        (Literal::Number(n), FieldKind::Scalar(ScalarType::Int)) => {
            let v: i32 = n.parse().ok()?;
            quote!(#v)
        }
        (Literal::Number(n), FieldKind::Scalar(ScalarType::BigInt)) => {
            let v: i64 = n.parse().ok()?;
            quote!(#v)
        }
        (Literal::Number(n), FieldKind::Scalar(ScalarType::Float)) => {
            let v: f64 = n.parse().ok()?;
            quote!(#v)
        }
        (Literal::Boolean(b), _) => quote!(#b),
        (Literal::EnumValue(v), FieldKind::Enum(id)) => {
            let e = ident(&schema.enums[id.0].name);
            let variant = format_ident!("{}", pascal(v));
            quote!(super::#e::#variant)
        }
        (Literal::List(items), _) => {
            let parts = items
                .iter()
                .map(|i| literal(schema, f, i))
                .collect::<Option<Vec<_>>>()?;
            quote!(vec![#(#parts),*])
        }
        _ => return None,
    })
}

/// How the client fills a field left out of `Create`.
fn filled(schema: &Schema, f: &Field) -> Option<TokenStream> {
    if f.updated_at {
        return Some(quote!(::lumen_orm::mongo::DateTime::now()));
    }
    match &f.default {
        Some(DefaultValue::Now) => Some(quote!(::lumen_orm::mongo::DateTime::now())),
        Some(DefaultValue::Literal(l)) => literal(schema, f, l),
        // ids made by the client, as Prisma does on MongoDB; v7 sorts by creation time
        Some(DefaultValue::Uuid(7)) if is_string(f) => {
            Some(quote!(::lumen_orm::uuid::Uuid::now_v7().to_string()))
        }
        Some(DefaultValue::Uuid(_)) if is_string(f) => {
            Some(quote!(::lumen_orm::uuid::Uuid::new_v4().to_string()))
        }
        _ => None,
    }
}

fn is_string(f: &Field) -> bool {
    f.kind == FieldKind::Scalar(ScalarType::String)
        && f.native_type.as_ref().is_none_or(|n| n.name != "ObjectId")
}

/// The fields of `Create` with their types there: the id and fields the client fills are
/// `Option`.
fn create_fields<'a>(schema: &'a Schema, model: &'a Model) -> Vec<(&'a Field, TokenStream)> {
    let mut out = Vec::new();
    for f in model.fields.iter().filter(|f| !f.ignored) {
        let Some(ty) = field_type(schema, f) else {
            continue;
        };
        let auto = matches!(f.default, Some(DefaultValue::Auto));
        let required = f.arity == Arity::Required && !auto && filled(schema, f).is_none();
        if required || f.arity == Arity::Optional {
            out.push((f, ty));
        } else {
            out.push((f, quote!(Option<#ty>)));
        }
    }
    out
}

fn create_input(schema: &Schema, model: &Model) -> TokenStream {
    let mut user = Vec::new();
    let mut stored = Vec::new();
    let mut fill = Vec::new();
    for f in model.fields.iter().filter(|f| !f.ignored) {
        let Some(ty) = field_type(schema, f) else {
            continue;
        };
        let id = ident(&snake(&f.name));
        let key = f.database_name();
        let auto = matches!(f.default, Some(DefaultValue::Auto));
        let fills = filled(schema, f);
        let required = f.arity == Arity::Required && !auto && fills.is_none();
        if required {
            user.push(quote!(pub #id: #ty,));
            stored.push(quote!(#[serde(rename = #key)] #id: #ty,));
            fill.push(quote!(#id: self.#id,));
        } else if f.arity == Arity::Optional {
            user.push(quote!(pub #id: #ty,));
            stored.push(
                quote!(#[serde(rename = #key, skip_serializing_if = "Option::is_none")] #id: #ty,),
            );
            fill.push(quote!(#id: self.#id,));
        } else {
            user.push(quote!(pub #id: Option<#ty>,));
            stored.push(quote!(#[serde(rename = #key, skip_serializing_if = "Option::is_none")] #id: Option<#ty>,));
            fill.push(if let Some(expr) = fills {
                quote!(#id: Some(self.#id.unwrap_or_else(|| #expr)),)
            } else {
                quote!(#id: self.#id,)
            });
        }
    }
    quote! {
        /// The data for a new document. The id (`auto()`) and fields with defaults are `Option`;
        /// the client fills the defaults, since MongoDB has none.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Create { #(#user)* }

        #[derive(::lumen_orm::serde::Serialize)]
        #[serde(crate = "::lumen_orm::serde")]
        struct NewDocument { #(#stored)* }

        impl Create {
            fn into_document(self) -> NewDocument {
                NewDocument { #(#fill)* }
            }
        }
    }
}

/// The id field's Rust name and document key.
fn id_filter(model: &Model) -> Option<(TokenStream, String)> {
    let pk = model.primary_key()?;
    let [f] = pk.fields.as_slice() else {
        return None;
    };
    let field = &model.fields[f.field];
    let id = ident(&snake(&field.name));
    Some((quote!(#id), field.database_name().to_owned()))
}

#[allow(
    clippy::too_many_lines,
    reason = "one quote! template: the generated actions, kept together"
)]
fn actions(model: &Model) -> TokenStream {
    let record = ident(&model.name);
    let collection = model.database_name();
    let missing = format!("{} record", model.name);
    let (id_field, id_key) = id_filter(model).unwrap_or_else(|| (quote!(id), "_id".to_owned()));
    quote! {
        fn by_id(r: &#record) -> ::lumen_orm::Filter {
            ::lumen_orm::col(#id_key).equals(::lumen_orm::Value::from(r.#id_field.clone()))
        }

        /// The operations on this model, on the database or in a transaction.
        pub struct Actions<R> { run: R }

        /// The operations, on any [`::lumen_orm::mongo::MongoRun`] (for example `&mut tx`).
        pub fn on<R: ::lumen_orm::mongo::MongoRun>(run: R) -> Actions<R> { Actions { run } }

        impl<R: ::lumen_orm::mongo::MongoRun> Actions<R> {
            /// Documents matching the filters you add.
            #[must_use]
            pub fn find_many(self) -> FindMany<R> { FindMany { run: self.run, find: find() } }

            /// The document with this id or unique key.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn find_unique(mut self, w: UniqueWhere) -> ::lumen_orm::Result<Option<#record>> {
                let mut f = find();
                f.filter = Some(w.into());
                f.limit = Some(1);
                Ok(::lumen_orm::mongo::MongoRun::find(&mut self.run, &f).await?.into_iter().next())
            }

            /// The first document matching `filter`.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn find_first(self, filter: ::lumen_orm::Filter) -> ::lumen_orm::Result<Option<#record>> {
                Ok(self.find_many().filter(filter).take(1).exec().await?.into_iter().next())
            }

            /// Store a new document and return it.
            ///
            /// # Errors
            /// P2002 when a unique index is hit; a coded error otherwise.
            pub async fn create(mut self, data: Create) -> ::lumen_orm::Result<#record> {
                let ids = ::lumen_orm::mongo::MongoRun::insert_many(&mut self.run, #collection, &[data.into_document()]).await?;
                let id = ids.first().map(::lumen_orm::mongo::value_of_bson).unwrap_or(::lumen_orm::Value::Null);
                let mut f = find();
                f.filter = Some(::lumen_orm::col("_id").equals(id));
                ::lumen_orm::mongo::MongoRun::find(&mut self.run, &f).await?.into_iter().next().ok_or_else(|| ::lumen_orm::Error::not_found(#missing))
            }

            /// Store many documents; the number stored.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn create_many(mut self, data: Vec<Create>) -> ::lumen_orm::Result<u64> {
                let docs: Vec<NewDocument> = data.into_iter().map(Create::into_document).collect();
                let ids = ::lumen_orm::mongo::MongoRun::insert_many(&mut self.run, #collection, &docs).await?;
                Ok(ids.len() as u64)
            }

            /// Change one document and return it (found first, so a changed unique key is fine).
            ///
            /// # Errors
            /// P2025 when there is no such document; a coded error otherwise.
            pub async fn update(mut self, w: UniqueWhere, data: Update) -> ::lumen_orm::Result<#record> {
                let mut f = find();
                f.filter = Some(w.into());
                f.limit = Some(1);
                let Some(current) = ::lumen_orm::mongo::MongoRun::find::<#record>(&mut self.run, &f).await?.into_iter().next() else {
                    return Err(::lumen_orm::Error::not_found(#missing));
                };
                let set = data.set();
                if !set.is_empty() {
                    ::lumen_orm::mongo::MongoRun::update_many(&mut self.run, #collection, Some(&by_id(&current)), set).await?;
                }
                let mut again = find();
                again.filter = Some(by_id(&current));
                ::lumen_orm::mongo::MongoRun::find(&mut self.run, &again).await?.into_iter().next().ok_or_else(|| ::lumen_orm::Error::not_found(#missing))
            }

            /// Change every document matching `filter`; how many matched.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn update_many(mut self, filter: ::lumen_orm::Filter, data: Update) -> ::lumen_orm::Result<u64> {
                let set = data.set();
                if set.is_empty() {
                    return Ok(0);
                }
                ::lumen_orm::mongo::MongoRun::update_many(&mut self.run, #collection, Some(&filter), set).await
            }

            /// Change the document, or create it when it does not exist. Run it in a transaction
            /// when two writers may race.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn upsert(mut self, w: UniqueWhere, create: Create, update: Update) -> ::lumen_orm::Result<#record> {
                let mut f = find();
                f.filter = Some(w.clone().into());
                f.limit = Some(1);
                let exists = !::lumen_orm::mongo::MongoRun::find::<#record>(&mut self.run, &f).await?.is_empty();
                if exists { self.update(w, update).await } else { self.create(create).await }
            }

            /// Remove one document and return it.
            ///
            /// # Errors
            /// P2025 when there is no such document; a coded error otherwise.
            pub async fn delete(mut self, w: UniqueWhere) -> ::lumen_orm::Result<#record> {
                let mut f = find();
                f.filter = Some(w.into());
                f.limit = Some(1);
                let Some(current) = ::lumen_orm::mongo::MongoRun::find::<#record>(&mut self.run, &f).await?.into_iter().next() else {
                    return Err(::lumen_orm::Error::not_found(#missing));
                };
                ::lumen_orm::mongo::MongoRun::delete_many(&mut self.run, #collection, Some(&by_id(&current))).await?;
                Ok(current)
            }

            /// Remove every document matching `filter` (all of them for `None`); how many.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn delete_many(mut self, filter: Option<::lumen_orm::Filter>) -> ::lumen_orm::Result<u64> {
                ::lumen_orm::mongo::MongoRun::delete_many(&mut self.run, #collection, filter.as_ref()).await
            }

            /// How many documents match `filter` (all of them for `None`).
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn count(mut self, filter: Option<::lumen_orm::Filter>) -> ::lumen_orm::Result<u64> {
                ::lumen_orm::mongo::MongoRun::count(&mut self.run, #collection, filter.as_ref()).await
            }
        }

        /// Documents matching the filters added, in the order asked for.
        pub struct FindMany<R> { run: R, find: ::lumen_orm::mongo::Find }

        impl<R: ::lumen_orm::mongo::MongoRun> FindMany<R> {
            /// Only documents matching `f` (combined with earlier filters by AND).
            #[must_use]
            pub fn filter(mut self, f: ::lumen_orm::Filter) -> Self {
                self.find.filter = Some(match self.find.filter.take() { Some(e) => e.and(f), None => f });
                self
            }
            /// Order by a field, for example `account::created_at::desc()`.
            #[must_use]
            pub fn order_by(mut self, order: (String, ::lumen_orm::Order)) -> Self {
                self.find.order.push(order);
                self
            }
            /// At most `n` documents.
            #[must_use]
            pub fn take(mut self, n: i64) -> Self {
                self.find.limit = Some(n);
                self
            }
            /// Skip the first `n` documents.
            #[must_use]
            pub fn skip(mut self, n: u64) -> Self {
                self.find.skip = Some(n);
                self
            }
            /// Run the query.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn exec(mut self) -> ::lumen_orm::Result<Vec<#record>> {
                ::lumen_orm::mongo::MongoRun::find(&mut self.run, &self.find).await
            }
        }
    }
}

fn key(var: &TokenStream, (name, optional): &(String, bool)) -> TokenStream {
    let id = ident(name);
    if *optional {
        quote!(#var.#id.clone())
    } else {
        quote!(Some(#var.#id.clone()))
    }
}

fn include_code(schema: &Schema, model: &Model, inc: &Include) -> TokenStream {
    let record = ident(&model.name);
    let record_field = ident(&snake(&model.name));
    let target = &schema.models[inc.target.0];
    let target_mod = ident(&snake(&target.name));
    let target_ty = ident(&target.name);
    let with = format_ident!("With{}", pascal(&inc.field));
    let builder = format_ident!("Include{}", pascal(&inc.field));
    let method = format_ident!("include_{}", snake(&inc.field));
    let field = ident(&snake(&inc.field));
    let child_field = &inc.child_column;
    let parent_key = key(&quote!(p), &inc.parent_key);
    let child_key = key(&quote!(c), &inc.child_key);
    let (related_ty, load) = if inc.many {
        (
            quote!(Vec<super::#target_mod::#target_ty>),
            quote! {
                let related = ::lumen_orm::mongo::load_many(&mut run, &parents, |p| #parent_key, &self.related, #child_field, |c: &super::#target_mod::#target_ty| #child_key, self.take).await?;
                Ok(parents.into_iter().zip(related).map(|(#record_field, #field)| #with { #record_field, #field }).collect())
            },
        )
    } else if inc.required {
        let missing = format!("{} related to a {}", target.name, model.name);
        (
            quote!(super::#target_mod::#target_ty),
            quote! {
                let related = ::lumen_orm::mongo::load_one(&mut run, &parents, |p| #parent_key, &self.related, #child_field, |c: &super::#target_mod::#target_ty| #child_key).await?;
                parents.into_iter().zip(related).map(|(#record_field, #field)| {
                    let #field = #field.ok_or_else(|| ::lumen_orm::Error::not_found(#missing))?;
                    Ok(#with { #record_field, #field })
                }).collect()
            },
        )
    } else {
        (
            quote!(Option<super::#target_mod::#target_ty>),
            quote! {
                let related = ::lumen_orm::mongo::load_one(&mut run, &parents, |p| #parent_key, &self.related, #child_field, |c: &super::#target_mod::#target_ty| #child_key).await?;
                Ok(parents.into_iter().zip(related).map(|(#record_field, #field)| #with { #record_field, #field }).collect())
            },
        )
    };
    // paging a to-one relation means nothing, so its builder has no take or skip
    let paging = if inc.many {
        quote! {
            /// At most `n` related documents per document (done on the server).
            #[must_use]
            pub fn take(mut self, n: i64) -> Self {
                self.take = Some(n);
                self
            }
            /// Skip the first `n` related documents of each document (done on the server).
            #[must_use]
            pub fn skip(mut self, n: u64) -> Self {
                self.related.skip = Some(n);
                self
            }
        }
    } else {
        TokenStream::new()
    };
    let (take_field, take_init) = if inc.many {
        (quote!(, take: Option<i64>), quote!(, take: None))
    } else {
        (TokenStream::new(), TokenStream::new())
    };
    let with_doc = format!(" A `{}` with its `{}`.", model.name, inc.field);
    quote! {
        #[doc = #with_doc]
        #[derive(Debug, Clone, PartialEq, ::lumen_orm::serde::Serialize)]
        #[serde(crate = "::lumen_orm::serde")]
        pub struct #with {
            /// The document.
            #[serde(flatten)]
            pub #record_field: #record,
            /// The related documents.
            pub #field: #related_ty,
        }

        /// A find-many that also loads one relation (one more query for all the documents).
        pub struct #builder<R> { find: FindMany<R>, related: ::lumen_orm::mongo::Find #take_field }

        impl<R: ::lumen_orm::mongo::MongoRun> #builder<R> {
            /// Only related documents matching `f`.
            #[must_use]
            pub fn filter(mut self, f: ::lumen_orm::Filter) -> Self {
                self.related.filter = Some(match self.related.filter.take() { Some(e) => e.and(f), None => f });
                self
            }
            /// Order the related documents.
            #[must_use]
            pub fn order_by(mut self, order: (String, ::lumen_orm::Order)) -> Self {
                self.related.order.push(order);
                self
            }
            #paging
            /// Run both queries.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn exec(self) -> ::lumen_orm::Result<Vec<#with>> {
                let FindMany { mut run, find } = self.find;
                let parents: Vec<#record> = ::lumen_orm::mongo::MongoRun::find(&mut run, &find).await?;
                #load
            }
        }

        impl<R: ::lumen_orm::mongo::MongoRun> FindMany<R> {
            /// Also load `#field`.
            #[must_use]
            pub fn #method(self) -> #builder<R> {
                #builder { find: self, related: super::#target_mod::find() #take_init }
            }
        }
    }
}

/// The module for one MongoDB model.
pub fn module(schema: &Schema, model_id: ModelId) -> TokenStream {
    let model = &schema.models[model_id.0];
    let module = ident(&snake(&model.name));
    let collection = model.database_name();
    let record = record(schema, model);
    let fields = model
        .fields
        .iter()
        .filter(|f| !f.ignored && f.is_scalar_like() && base_type(schema, f).is_some())
        .map(|f| field_module(schema, f, &quote!()));
    let relations = crate::relation_filter::modules(schema, model_id, true);
    let create = create_input(schema, model);
    let update = update_input(schema, model);
    let unique = unique_where(schema, model);
    let actions = actions(model);
    let includes = includes(schema, model_id)
        .iter()
        .map(|i| include_code(schema, model, i))
        .collect::<Vec<_>>();
    let nested = crate::nested::code(schema, model_id, &create_fields(schema, model), true);
    let module_doc = format!(" The `{}` model (collection `{collection}`).", model.name);
    quote! {
        #[doc = #module_doc]
        pub mod #module {
            /// The collection.
            pub const COLLECTION: &str = #collection;
            /// A find over this model's documents, for dynamic queries.
            pub fn find() -> ::lumen_orm::mongo::Find {
                ::lumen_orm::mongo::Find { collection: COLLECTION.to_owned(), ..::lumen_orm::mongo::Find::default() }
            }
            #record
            #(#fields)*
            #(#relations)*
            #create
            #update
            #unique
            #actions
            #(#includes)*
            #nested
        }
    }
}
