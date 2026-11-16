//! One module per model: the record, its row decoding, typed filters per field, create and update
//! inputs, unique lookups, the actions and the find-many builder with includes.

use lumen_schema::ast::Arity;
use lumen_schema::checked::{
    DefaultValue, Field, FieldKind, IndexKind, Model, ModelId, RelationKind, ScalarType, Schema,
};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::types::{base_type, base_type_at, field_type, hashable, ident, pascal, snake};

pub(crate) fn doc(text: Option<&str>) -> TokenStream {
    match text {
        Some(t) => {
            let lines = t.lines().map(|l| format!(" {l}"));
            quote!(#(#[doc = #lines])*)
        }
        None => quote!(),
    }
}

/// The fields stored as columns, with their Rust types.
pub(crate) fn columns<'a>(schema: &'a Schema, model: &'a Model) -> Vec<(&'a Field, TokenStream)> {
    model
        .fields
        .iter()
        .filter(|f| !f.ignored)
        .filter_map(|f| field_type(schema, f).map(|t| (f, t)))
        .collect()
}

fn record(schema: &Schema, model: &Model) -> TokenStream {
    let name = ident(&model.name);
    let docs = doc(model.doc.as_deref());
    let cols = columns(schema, model);
    let fields = cols.iter().map(|(f, ty)| {
        let id = ident(&snake(&f.name));
        let schema_name = &f.name;
        let d = doc(f.doc.as_deref());
        let rename = (snake(&f.name) != f.name).then(|| quote!(#[serde(rename = #schema_name)]));
        quote!(#d #rename pub #id: #ty,)
    });
    let reads = cols.iter().map(|(f, _)| {
        let id = ident(&snake(&f.name));
        let column = f.database_name();
        quote!(#id: ::lumen_orm::sqlx::Row::try_get(row, #column)?,)
    });
    quote! {
        #docs
        #[derive(Debug, Clone, PartialEq, ::lumen_orm::serde::Serialize, ::lumen_orm::serde::Deserialize)]
        #[serde(crate = "::lumen_orm::serde")]
        pub struct #name { #(#fields)* }

        impl<'r> ::lumen_orm::sqlx::FromRow<'r, ::lumen_orm::sqlx::postgres::PgRow> for #name {
            fn from_row(row: &'r ::lumen_orm::sqlx::postgres::PgRow) -> Result<Self, ::lumen_orm::sqlx::Error> {
                Ok(Self { #(#reads)* })
            }
        }
    }
}

fn is_text(f: &Field) -> bool {
    f.kind == FieldKind::Scalar(ScalarType::String)
        && f.native_type
            .as_ref()
            .is_none_or(|n| n.name != "Uuid" && n.name != "ObjectId")
}

fn is_ordered(f: &Field) -> bool {
    matches!(
        f.kind,
        FieldKind::Scalar(
            ScalarType::String
                | ScalarType::Int
                | ScalarType::BigInt
                | ScalarType::Float
                | ScalarType::Decimal
                | ScalarType::DateTime
        )
    )
}

/// `user::email::equals(..)` and friends, typed by the field.
pub(crate) fn field_module(schema: &Schema, f: &Field) -> TokenStream {
    let module = ident(&snake(&f.name));
    let column = f.database_name();
    let Some(base) = base_type_at(schema, f, 2) else {
        return quote!();
    };
    let arg = if is_text(f) {
        quote!(impl Into<String>)
    } else {
        quote!(#base)
    };
    let conv = if is_text(f) {
        quote!(::lumen_orm::Value::from(v.into()))
    } else {
        quote!(::lumen_orm::Value::from(v))
    };
    let mut fns = Vec::new();
    if f.arity != Arity::List {
        fns.push(quote! {
            /// Equal to `v`.
            pub fn equals(v: #arg) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).equals(#conv) }
            /// Not equal to `v` (rows where it is NULL match too, as in Prisma).
            pub fn not(v: #arg) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).not_equals(#conv) }
            /// One of `values`.
            pub fn is_in(values: impl IntoIterator<Item = #base>) -> ::lumen_orm::Filter {
                ::lumen_orm::col(COLUMN).is_in(values.into_iter().map(::lumen_orm::Value::from))
            }
            /// None of `values`.
            pub fn not_in(values: impl IntoIterator<Item = #base>) -> ::lumen_orm::Filter {
                ::lumen_orm::col(COLUMN).not_in(values.into_iter().map(::lumen_orm::Value::from))
            }
        });
    }
    if is_ordered(f) && f.arity != Arity::List {
        fns.push(quote! {
            /// Less than `v`.
            pub fn lt(v: #arg) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).lt(#conv) }
            /// At most `v`.
            pub fn lte(v: #arg) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).lte(#conv) }
            /// More than `v`.
            pub fn gt(v: #arg) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).gt(#conv) }
            /// At least `v`.
            pub fn gte(v: #arg) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).gte(#conv) }
        });
    }
    if is_text(f) && f.arity != Arity::List {
        fns.push(quote! {
            /// Contains the text.
            pub fn contains(v: impl Into<String>) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).contains(v) }
            /// Contains the text, ignoring case.
            pub fn contains_insensitive(v: impl Into<String>) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).contains_insensitive(v) }
            /// Starts with the text.
            pub fn starts_with(v: impl Into<String>) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).starts_with(v) }
            /// Ends with the text.
            pub fn ends_with(v: impl Into<String>) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).ends_with(v) }
            /// Starts with the text, ignoring case.
            pub fn starts_with_insensitive(v: impl Into<String>) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).starts_with_insensitive(v) }
            /// Ends with the text, ignoring case.
            pub fn ends_with_insensitive(v: impl Into<String>) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).ends_with_insensitive(v) }
            /// Equal, ignoring case.
            pub fn equals_insensitive(v: impl Into<String>) -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).equals_insensitive(v) }
        });
    }
    if f.arity == Arity::Optional {
        fns.push(quote! {
            /// Has no value.
            pub fn is_null() -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).is_null() }
            /// Has a value.
            pub fn is_not_null() -> ::lumen_orm::Filter { ::lumen_orm::col(COLUMN).is_not_null() }
        });
    }
    let field_name = &f.name;
    let module_doc = format!(" Conditions and ordering on `{field_name}`.");
    quote! {
        #[doc = #module_doc]
        pub mod #module {
            /// The column.
            pub const COLUMN: &str = #column;
            #(#fns)*
            /// Smallest first.
            pub fn asc() -> (String, ::lumen_orm::Order) { (COLUMN.to_owned(), ::lumen_orm::Order::Asc) }
            /// Largest first.
            pub fn desc() -> (String, ::lumen_orm::Order) { (COLUMN.to_owned(), ::lumen_orm::Order::Desc) }
        }
    }
}

/// How a create fills a field the caller left out.
pub(crate) fn client_default(f: &Field) -> Option<TokenStream> {
    client_default_for(f, false)
}

/// Like [`client_default`]; `mongo` uses BSON dates.
pub(crate) fn client_default_for(f: &Field, mongo: bool) -> Option<TokenStream> {
    if mongo && f.updated_at {
        return Some(quote!(::lumen_orm::Value::from(
            ::lumen_orm::mongo::DateTime::now()
        )));
    }
    match (&f.default, f.updated_at) {
        (_, true) => Some(
            if f.native_type
                .as_ref()
                .is_some_and(|n| n.name == "Timestamptz")
            {
                quote!(::lumen_orm::Value::from(::lumen_orm::chrono::Utc::now()))
            } else {
                quote!(::lumen_orm::Value::from(
                    ::lumen_orm::chrono::Utc::now().naive_utc()
                ))
            },
        ),
        (Some(DefaultValue::Uuid(7)), _) => Some(uuid_value(f, &quote!(now_v7))),
        (Some(DefaultValue::Uuid(_)), _) => Some(uuid_value(f, &quote!(new_v4))),
        _ => None,
    }
}

fn uuid_value(f: &Field, make: &TokenStream) -> TokenStream {
    if f.native_type.as_ref().is_some_and(|n| n.name == "Uuid") {
        quote!(::lumen_orm::Value::from(::lumen_orm::uuid::Uuid::#make()))
    } else {
        quote!(::lumen_orm::Value::from(::lumen_orm::uuid::Uuid::#make().to_string()))
    }
}

/// Whether the database or the client fills the field when it is left out.
pub(crate) fn has_default(f: &Field) -> bool {
    f.updated_at
        || matches!(
            f.default,
            Some(
                DefaultValue::Autoincrement
                    | DefaultValue::Now
                    | DefaultValue::Literal(_)
                    | DefaultValue::DbGenerated(_)
                    | DefaultValue::Uuid(_)
            )
        )
}

pub(crate) fn create_input(schema: &Schema, model: &Model) -> TokenStream {
    let cols = columns(schema, model);
    let fields = cols.iter().map(|(f, ty)| {
        let id = ident(&snake(&f.name));
        let required = f.arity == Arity::Required && !has_default(f);
        if required || f.arity == Arity::Optional {
            quote!(pub #id: #ty,)
        } else {
            quote!(pub #id: Option<#ty>,)
        }
    });
    let pushes = cols.iter().map(|(f, _)| {
        let id = ident(&snake(&f.name));
        let column = f.database_name();
        let required = f.arity == Arity::Required && !has_default(f);
        if required {
            quote!(columns.push(#column.to_owned()); values.push(::lumen_orm::Value::from(self.#id));)
        } else if let Some(default) = client_default(f) {
            quote! {
                columns.push(#column.to_owned());
                values.push(match self.#id { Some(v) => ::lumen_orm::Value::from(v), None => #default });
            }
        } else {
            quote!(if let Some(v) = self.#id { columns.push(#column.to_owned()); values.push(::lumen_orm::Value::from(v)); })
        }
    });
    let defaults_doc =
        " The data for a new record. Fields the database fills (ids, defaults) are `Option`.";
    quote! {
        #[doc = #defaults_doc]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Create { #(#fields)* }

        impl Create {
            fn row(self) -> (Vec<String>, Vec<::lumen_orm::Value>) {
                let mut columns = Vec::new();
                let mut values = Vec::new();
                #(#pushes)*
                (columns, values)
            }
        }
    }
}

pub(crate) fn update_input(schema: &Schema, model: &Model) -> TokenStream {
    let cols = columns(schema, model);
    let fields = cols.iter().map(|(f, ty)| {
        let id = ident(&snake(&f.name));
        quote!(pub #id: ::lumen_orm::Set<#ty>,)
    });
    let pushes = cols.iter().map(|(f, _)| {
        let id = ident(&snake(&f.name));
        let column = f.database_name();
        let mongo = schema.provider() == lumen_schema::checked::Provider::MongoDb;
        let auto = if f.updated_at { client_default_for(f, mongo) } else { None };
        if let Some(now) = auto {
            quote! {
                set.push((#column.to_owned(), match self.#id { ::lumen_orm::Set::To(v) => ::lumen_orm::Value::from(v), ::lumen_orm::Set::NotSet => #now }));
            }
        } else {
            quote! {
                if let ::lumen_orm::Set::To(v) = self.#id { set.push((#column.to_owned(), ::lumen_orm::Value::from(v))); }
            }
        }
    });
    quote! {
        /// The fields to change; `Set::NotSet` (the default) leaves a field as it is.
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct Update { #(#fields)* }

        impl Update {
            fn set(self) -> Vec<(String, ::lumen_orm::Value)> {
                let mut set = Vec::new();
                #(#pushes)*
                set
            }
        }
    }
}

/// `UniqueWhere::Id(1)`, `UniqueWhere::AuthorIdTitle { .. }`: one variant per unique key.
pub(crate) fn unique_where(schema: &Schema, model: &Model) -> TokenStream {
    let mut variants = Vec::new();
    let mut arms = Vec::new();
    let mut columns_arms = Vec::new();
    for index in model
        .indexes
        .iter()
        .filter(|i| matches!(i.kind, IndexKind::PrimaryKey | IndexKind::Unique))
    {
        let fields: Vec<&Field> = index
            .fields
            .iter()
            .map(|f| &model.fields[f.field])
            .collect();
        let Some(types) = fields
            .iter()
            .map(|f| base_type(schema, f))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let variant_name = index.client_name.as_deref().map_or_else(
            || fields.iter().map(|f| pascal(&f.name)).collect::<String>(),
            pascal,
        );
        let variant = format_ident!("{}", variant_name);
        if variants
            .iter()
            .any(|(v, _): &(proc_macro2::Ident, TokenStream)| *v == variant)
        {
            continue;
        }
        let cols: Vec<&str> = fields.iter().map(|f| f.database_name()).collect();
        columns_arms.push(quote!(Self::#variant { .. } => vec![#(#cols.to_owned()),*],));
        if let ([f], [ty]) = (fields.as_slice(), types.as_slice()) {
            let column = f.database_name();
            variants.push((variant.clone(), quote!(#variant(#ty),)));
            arms.push(quote!(UniqueWhere::#variant(v) => ::lumen_orm::col(#column).equals(::lumen_orm::Value::from(v)),));
            columns_arms.pop();
            columns_arms.push(quote!(Self::#variant(_) => vec![#column.to_owned()],));
        } else {
            let names: Vec<proc_macro2::Ident> =
                fields.iter().map(|f| ident(&snake(&f.name))).collect();
            let decl = names.iter().zip(&types).map(|(n, t)| quote!(#n: #t));
            variants.push((variant.clone(), quote!(#variant { #(#decl),* },)));
            let conds = names
                .iter()
                .zip(&cols)
                .map(|(n, c)| quote!(::lumen_orm::col(#c).equals(::lumen_orm::Value::from(#n))));
            arms.push(quote!(UniqueWhere::#variant { #(#names),* } => ::lumen_orm::Filter::And(vec![#(#conds),*]),));
        }
    }
    let decls = variants.iter().map(|(_, d)| d);
    quote! {
        /// One record, by its id or a unique key.
        #[derive(Debug, Clone, PartialEq)]
        pub enum UniqueWhere { #(#decls)* }

        impl UniqueWhere {
            fn columns(&self) -> Vec<String> {
                match self { #(#columns_arms)* }
            }
        }

        impl From<UniqueWhere> for ::lumen_orm::Filter {
            fn from(w: UniqueWhere) -> Self {
                match w { #(#arms)* }
            }
        }
    }
}

/// An include of a one-to-one or one-to-many relation on a single-column key.
pub(crate) struct Include {
    pub(crate) field: String,
    pub(crate) target: ModelId,
    pub(crate) many: bool,
    pub(crate) required: bool,
    pub(crate) parent_key: (String, bool),
    pub(crate) child_column: String,
    pub(crate) child_key: (String, bool),
}

pub(crate) fn includes(schema: &Schema, model_id: ModelId) -> Vec<Include> {
    let model = &schema.models[model_id.0];
    let mut out = Vec::new();
    for f in &model.fields {
        let (FieldKind::Model(target), Some(rel)) = (&f.kind, f.relation) else {
            continue;
        };
        let r = &schema.relations[rel.0];
        if r.kind == RelationKind::ManyToMany || r.fields.len() != 1 {
            continue;
        }
        let a = &schema.models[r.a.model.0];
        let b = &schema.models[r.b.model.0];
        let fk = &a.fields[r.fields[0]];
        let referenced = &b.fields[r.references[0]];
        if !hashable(fk)
            || base_type(schema, fk).map(|t| t.to_string())
                != base_type(schema, referenced).map(|t| t.to_string())
        {
            continue;
        }
        let optional = |x: &Field| x.arity == Arity::Optional;
        let holds_key = r.a.model == model_id
            && r.a.field
                == model
                    .fields
                    .iter()
                    .position(|g| std::ptr::eq(g, f))
                    .unwrap_or(usize::MAX);
        out.push(if holds_key {
            Include {
                field: f.name.clone(),
                target: *target,
                many: false,
                required: f.arity == Arity::Required,
                parent_key: (snake(&fk.name), optional(fk)),
                child_column: referenced.database_name().to_owned(),
                child_key: (snake(&referenced.name), optional(referenced)),
            }
        } else {
            Include {
                field: f.name.clone(),
                target: *target,
                many: f.arity == Arity::List,
                required: false,
                parent_key: (snake(&referenced.name), optional(referenced)),
                child_column: fk.database_name().to_owned(),
                child_key: (snake(&fk.name), optional(fk)),
            }
        });
    }
    out
}

fn key_expr(var: &TokenStream, (name, optional): &(String, bool)) -> TokenStream {
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
    let child_column = &inc.child_column;
    let parent_key = key_expr(&quote!(p), &inc.parent_key);
    let child_key = key_expr(&quote!(c), &inc.child_key);
    let (related_ty, load) = if inc.many {
        (
            quote!(Vec<super::#target_mod::#target_ty>),
            quote! {
                let related = ::lumen_orm::load::load_many(&mut run, &parents, |p| #parent_key, &self.related, #child_column, |c: &super::#target_mod::#target_ty| #child_key, self.take).await?;
                Ok(parents.into_iter().zip(related).map(|(#record_field, #field)| #with { #record_field, #field }).collect())
            },
        )
    } else if inc.required {
        let missing = format!("{} related to a {}", target.name, model.name);
        (
            quote!(super::#target_mod::#target_ty),
            quote! {
                let related = ::lumen_orm::load::load_one(&mut run, &parents, |p| #parent_key, &self.related, #child_column, |c: &super::#target_mod::#target_ty| #child_key).await?;
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
                let related = ::lumen_orm::load::load_one(&mut run, &parents, |p| #parent_key, &self.related, #child_column, |c: &super::#target_mod::#target_ty| #child_key).await?;
                Ok(parents.into_iter().zip(related).map(|(#record_field, #field)| #with { #record_field, #field }).collect())
            },
        )
    };
    // paging a to-one relation means nothing, so its builder has no take or skip
    let paging = if inc.many {
        quote! {
            /// At most `n` related records per record (done in SQL).
            #[must_use]
            pub fn take(mut self, n: i64) -> Self {
                self.take = Some(n);
                self
            }
            /// Skip the first `n` related records of each record (done in SQL).
            #[must_use]
            pub fn skip(mut self, n: i64) -> Self {
                self.related.offset = Some(n);
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
            /// The record.
            #[serde(flatten)]
            pub #record_field: #record,
            /// The related records.
            pub #field: #related_ty,
        }

        /// A find-many that also loads one relation (one more query for all the records).
        pub struct #builder<R> { find: FindMany<R>, related: ::lumen_orm::Select #take_field }

        impl<R: ::lumen_orm::Run> #builder<R> {
            /// Only related records matching `f`.
            #[must_use]
            pub fn filter(mut self, f: ::lumen_orm::Filter) -> Self {
                self.related.filter = Some(match self.related.filter.take() { Some(e) => e.and(f), None => f });
                self
            }
            /// Order the related records.
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
                let FindMany { mut run, select } = self.find;
                let parents: Vec<#record> = ::lumen_orm::Run::fetch_all(&mut run, select.build()).await?;
                #load
            }
        }

        impl<R: ::lumen_orm::Run> FindMany<R> {
            /// Also load `#field`.
            #[must_use]
            pub fn #method(self) -> #builder<R> {
                #builder { find: self, related: super::#target_mod::select() #take_init }
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one quote! template: the generated actions, kept together"
)]
fn actions(model: &Model) -> TokenStream {
    let record = ident(&model.name);
    let missing = format!("{} record", model.name);
    quote! {
        /// The operations on this model, on the pool or in a transaction.
        pub struct Actions<R> { run: R }

        /// The operations, on any [`::lumen_orm::Run`] (for example `&mut tx` in a transaction).
        pub fn on<R: ::lumen_orm::Run>(run: R) -> Actions<R> { Actions { run } }

        impl<R: ::lumen_orm::Run> Actions<R> {
            /// Records matching the filters you add.
            #[must_use]
            pub fn find_many(self) -> FindMany<R> { FindMany { run: self.run, select: select() } }

            /// The record with this id or unique key.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn find_unique(mut self, w: UniqueWhere) -> ::lumen_orm::Result<Option<#record>> {
                let mut s = select();
                s.filter = Some(w.into());
                s.limit = Some(1);
                ::lumen_orm::Run::fetch_optional(&mut self.run, s.build()).await
            }

            /// The first record matching `filter`.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn find_first(self, filter: ::lumen_orm::Filter) -> ::lumen_orm::Result<Option<#record>> {
                Ok(self.find_many().filter(filter).take(1).exec().await?.into_iter().next())
            }

            /// Store a new record and return it.
            ///
            /// # Errors
            /// P2002 when a unique field is taken; a coded error otherwise.
            pub async fn create(mut self, data: Create) -> ::lumen_orm::Result<#record> {
                let (columns, values) = data.row();
                let insert = ::lumen_orm::Insert { table: table(), columns, rows: vec![values], on_conflict: None, returning: columns_list() };
                ::lumen_orm::Run::fetch_one(&mut self.run, insert.build()).await
            }

            /// Store many records; the number stored.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn create_many(mut self, data: Vec<Create>) -> ::lumen_orm::Result<u64> {
                // one statement per set of columns given
                let mut groups: Vec<(Vec<String>, Vec<Vec<::lumen_orm::Value>>)> = Vec::new();
                for d in data {
                    let (columns, values) = d.row();
                    match groups.iter_mut().find(|(c, _)| *c == columns) {
                        Some((_, rows)) => rows.push(values),
                        None => groups.push((columns, vec![values])),
                    }
                }
                let mut n = 0;
                for (columns, rows) in groups {
                    let insert = ::lumen_orm::Insert { table: table(), columns, rows, on_conflict: None, returning: Vec::new() };
                    n += ::lumen_orm::Run::execute(&mut self.run, insert.build()).await?;
                }
                Ok(n)
            }

            /// Change one record and return it.
            ///
            /// # Errors
            /// P2025 when there is no such record; a coded error otherwise.
            pub async fn update(mut self, w: UniqueWhere, data: Update) -> ::lumen_orm::Result<#record> {
                let set = data.set();
                if set.is_empty() {
                    return self.find_unique(w).await?.ok_or_else(|| ::lumen_orm::Error::not_found(#missing));
                }
                let update = ::lumen_orm::Update { table: table(), set, filter: Some(w.into()), returning: columns_list() };
                ::lumen_orm::Run::fetch_optional(&mut self.run, update.build()).await?.ok_or_else(|| ::lumen_orm::Error::not_found(#missing))
            }

            /// Change every record matching `filter`; the number changed.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn update_many(mut self, filter: ::lumen_orm::Filter, data: Update) -> ::lumen_orm::Result<u64> {
                let set = data.set();
                if set.is_empty() {
                    return Ok(0);
                }
                let update = ::lumen_orm::Update { table: table(), set, filter: Some(filter), returning: Vec::new() };
                ::lumen_orm::Run::execute(&mut self.run, update.build()).await
            }

            /// Create the record, or change it when the unique key exists (one statement).
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn upsert(mut self, w: UniqueWhere, create: Create, update: Update) -> ::lumen_orm::Result<#record> {
                let keys = w.columns();
                let (columns, values) = create.row();
                let mut set = update.set();
                if set.is_empty() {
                    // nothing to change: touch the key so the row is still returned
                    if let Some((c, v)) = columns.iter().zip(&values).find(|(c, _)| keys.contains(c)) {
                        set.push((c.clone(), v.clone()));
                    }
                }
                let insert = ::lumen_orm::Insert {
                    table: table(),
                    columns,
                    rows: vec![values],
                    on_conflict: Some(::lumen_orm::OnConflict { columns: keys, update: set }),
                    returning: columns_list(),
                };
                ::lumen_orm::Run::fetch_one(&mut self.run, insert.build()).await
            }

            /// Remove one record and return it.
            ///
            /// # Errors
            /// P2025 when there is no such record; a coded error otherwise.
            pub async fn delete(mut self, w: UniqueWhere) -> ::lumen_orm::Result<#record> {
                let delete = ::lumen_orm::Delete { table: table(), filter: Some(w.into()), returning: columns_list() };
                ::lumen_orm::Run::fetch_optional(&mut self.run, delete.build()).await?.ok_or_else(|| ::lumen_orm::Error::not_found(#missing))
            }

            /// Remove every record matching `filter` (all of them for `None`); the number removed.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn delete_many(mut self, filter: Option<::lumen_orm::Filter>) -> ::lumen_orm::Result<u64> {
                let delete = ::lumen_orm::Delete { table: table(), filter, returning: Vec::new() };
                ::lumen_orm::Run::execute(&mut self.run, delete.build()).await
            }

            /// How many records match `filter` (all of them for `None`).
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn count(mut self, filter: Option<::lumen_orm::Filter>) -> ::lumen_orm::Result<i64> {
                let mut s = ::lumen_orm::Select::from(table());
                s.filter = filter;
                ::lumen_orm::Run::count(&mut self.run, &s).await
            }
        }

        /// Records matching the filters added, in the order asked for.
        pub struct FindMany<R> { run: R, select: ::lumen_orm::Select }

        impl<R: ::lumen_orm::Run> FindMany<R> {
            /// Only records matching `f` (combined with earlier filters by AND).
            #[must_use]
            pub fn filter(mut self, f: ::lumen_orm::Filter) -> Self {
                self.select.filter = Some(match self.select.filter.take() { Some(e) => e.and(f), None => f });
                self
            }
            /// Order by a column, for example `user::id::desc()`.
            #[must_use]
            pub fn order_by(mut self, order: (String, ::lumen_orm::Order)) -> Self {
                self.select.order.push(order);
                self
            }
            /// At most `n` records.
            #[must_use]
            pub fn take(mut self, n: i64) -> Self {
                self.select.limit = Some(n);
                self
            }
            /// Skip the first `n` records.
            #[must_use]
            pub fn skip(mut self, n: i64) -> Self {
                self.select.offset = Some(n);
                self
            }
            /// Run the query.
            ///
            /// # Errors
            /// A coded error from the database.
            pub async fn exec(mut self) -> ::lumen_orm::Result<Vec<#record>> {
                ::lumen_orm::Run::fetch_all(&mut self.run, self.select.build()).await
            }
        }
    }
}

/// The module for one model.
pub fn module(schema: &Schema, model_id: ModelId) -> TokenStream {
    let model = &schema.models[model_id.0];
    let module = ident(&snake(&model.name));
    let table = model.database_name();
    let schema_name = model
        .schema
        .as_ref()
        .map_or_else(|| quote!(None), |s| quote!(Some(#s.to_owned())));
    let cols: Vec<&str> = columns(schema, model)
        .iter()
        .map(|(f, _)| f.database_name())
        .collect();
    let record = record(schema, model);
    let fields = model
        .fields
        .iter()
        .filter(|f| !f.ignored && f.is_scalar_like())
        .map(|f| field_module(schema, f));
    let relations = crate::relation_filter::modules(schema, model_id, false);
    let create = create_input(schema, model);
    let update = update_input(schema, model);
    let unique = unique_where(schema, model);
    let actions = actions(model);
    let includes = includes(schema, model_id)
        .iter()
        .map(|i| include_code(schema, model, i))
        .collect::<Vec<_>>();
    let module_doc = format!(" The `{}` model (table `{table}`).", model.name);
    quote! {
        #[doc = #module_doc]
        pub mod #module {
            /// The table.
            pub fn table() -> ::lumen_orm::Table { ::lumen_orm::Table { schema: #schema_name, name: #table.to_owned() } }
            /// The columns read for a record.
            pub const COLUMNS: &[&str] = &[#(#cols),*];
            fn columns_list() -> Vec<String> { COLUMNS.iter().map(|c| (*c).to_owned()).collect() }
            /// A select of this model's records, for dynamic queries.
            pub fn select() -> ::lumen_orm::Select {
                let mut s = ::lumen_orm::Select::from(table());
                s.columns = columns_list();
                s
            }
            #record
            #(#fields)*
            #(#relations)*
            #create
            #update
            #unique
            #actions
            #(#includes)*
        }
    }
}
