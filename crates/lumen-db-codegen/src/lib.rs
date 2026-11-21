//! Generates the type-safe Rust client from the lumen schema file: one plain, readable Rust file,
//! no macros to learn. Wrong field names and wrong value types fail at compile time.
//!
//! ```
//! use lumen_schema::{SourceFile, check};
//!
//! let (schema, _) = check(&SourceFile::new("schema.prisma", "model User {\n  id Int @id\n}\n"));
//! let code = lumen_db_codegen::generate(&schema, "schema.prisma").unwrap();
//! assert!(code.contains("pub mod user"));
//! ```

mod aggregate;
mod model;
mod mongo_model;
mod nested;
mod relation_filter;
mod types;

use std::collections::BTreeMap;

use lumen_schema::checked::{ModelId, ModelKind, Schema};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use types::{ident, pascal, snake};

/// The version of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn enums(schema: &Schema) -> TokenStream {
    let items = schema.enums.iter().map(|e| {
        let name = ident(&e.name);
        let db_name = e.mapped_name.clone().unwrap_or_else(|| e.name.clone());
        let sql_type = if db_name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            db_name.clone()
        } else {
            format!("\"{db_name}\"")
        };
        let variants: Vec<_> = e.values.iter().map(|v| format_ident!("{}", pascal(&v.name))).collect();
        let schema_names: Vec<&str> = e.values.iter().map(|v| v.name.as_str()).collect();
        let db_values: Vec<String> = e.values.iter().map(|v| v.mapped_name.clone().unwrap_or_else(|| v.name.clone())).collect();
        let doc = e.doc.as_deref().map_or_else(|| format!(" The `{}` enum.", e.name), |d| format!(" {d}"));
        let unknown = format!("`{{}}` is not a value of the enum {}", e.name);
        quote! {
            #[doc = #doc]
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ::lumen_orm::serde::Serialize, ::lumen_orm::serde::Deserialize)]
            #[serde(crate = "::lumen_orm::serde")]
            pub enum #name {
                #(#[serde(rename = #schema_names)] #variants,)*
            }

            impl #name {
                /// The enum's type in SQL.
                pub const SQL_TYPE: &'static str = #sql_type;

                /// The value as stored in the database.
                #[must_use]
                pub fn as_db(&self) -> &'static str {
                    match self { #(Self::#variants => #db_values,)* }
                }
            }

            impl From<#name> for ::lumen_orm::Value {
                fn from(v: #name) -> Self {
                    ::lumen_orm::Value::Enum(v.as_db().to_owned(), #name::SQL_TYPE.to_owned())
                }
            }

            impl ::lumen_orm::sqlx::Type<::lumen_orm::sqlx::Postgres> for #name {
                fn type_info() -> ::lumen_orm::sqlx::postgres::PgTypeInfo {
                    ::lumen_orm::sqlx::postgres::PgTypeInfo::with_name(#db_name)
                }
            }

            impl ::lumen_orm::sqlx::postgres::PgHasArrayType for #name {
                fn array_type_info() -> ::lumen_orm::sqlx::postgres::PgTypeInfo {
                    ::lumen_orm::sqlx::postgres::PgTypeInfo::array_of(#db_name)
                }
            }

            impl<'r> ::lumen_orm::sqlx::Decode<'r, ::lumen_orm::sqlx::Postgres> for #name {
                fn decode(value: ::lumen_orm::sqlx::postgres::PgValueRef<'r>) -> Result<Self, ::lumen_orm::sqlx::error::BoxDynError> {
                    let text = <&str as ::lumen_orm::sqlx::Decode<::lumen_orm::sqlx::Postgres>>::decode(value)?;
                    match text {
                        #(#db_values => Ok(Self::#variants),)*
                        other => Err(format!(#unknown, other).into()),
                    }
                }
            }
        }
    });
    quote!(#(#items)*)
}

/// Constraint name → schema field names, from the same naming rules migrations use, so errors
/// name fields (`email`) instead of constraints (`User_email_key`).
fn constraints(schema: &Schema) -> TokenStream {
    let shape = lumen_migrate::calculate(schema);
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for model in &schema.models {
        let Some(table) = shape
            .tables
            .iter()
            .find(|t| t.name == model.database_name())
        else {
            continue;
        };
        let field_of = |column: &str| {
            model
                .fields
                .iter()
                .find(|f| f.database_name() == column)
                .map_or_else(|| column.to_owned(), |f| f.name.clone())
        };
        if let Some(pk) = &table.primary_key {
            map.insert(
                pk.name.clone(),
                pk.columns.iter().map(|c| field_of(c)).collect(),
            );
        }
        for i in table.indexes.iter().filter(|i| i.unique) {
            map.insert(
                i.name.clone(),
                i.columns.iter().map(|c| field_of(&c.name)).collect(),
            );
        }
        for fk in &table.foreign_keys {
            map.insert(
                fk.name.clone(),
                fk.columns.iter().map(|c| field_of(c)).collect(),
            );
        }
    }
    let entries = map
        .iter()
        .map(|(name, fields)| quote!((#name.to_owned(), vec![#(#fields.to_owned()),*])));
    quote! {
        fn constraints() -> ::std::collections::HashMap<String, Vec<String>> {
            ::std::collections::HashMap::from([#(#entries),*])
        }
    }
}

fn client(schema: &Schema, models: &[ModelId]) -> TokenStream {
    let accessors = models.iter().map(|id| {
        let m = &schema.models[id.0];
        let module = ident(&snake(&m.name));
        let method = ident(&snake(&m.name));
        let doc = format!(" Operations on `{}`.", m.name);
        quote! {
            #[doc = #doc]
            #[must_use]
            pub fn #method(&self) -> #module::Actions<&::lumen_orm::Db> { #module::on(&self.db) }
        }
    });
    let constraints = constraints(schema);
    quote! {
        /// The client: one accessor per model.
        #[derive(Debug, Clone)]
        pub struct Client { db: ::lumen_orm::Db }

        impl Client {
            /// Connect to `url`.
            ///
            /// # Errors
            /// P1001 when the database cannot be reached.
            pub async fn connect(url: &str) -> ::lumen_orm::Result<Self> {
                Ok(Self::new(::lumen_orm::Db::connect(url).await?))
            }

            /// Use an existing connection.
            #[must_use]
            pub fn new(db: ::lumen_orm::Db) -> Self {
                Self { db: db.with_constraints(constraints()) }
            }

            /// The connection, for transactions (`client.db().transaction(..)`) and dynamic queries.
            #[must_use]
            pub fn db(&self) -> &::lumen_orm::Db { &self.db }

            #(#accessors)*
        }

        #constraints
    }
}

/// `"Collection.index"` → schema field names for the unique indexes `db push` creates (and the
/// built-in `_id_`), so a duplicate names fields (`email`) instead of an index.
fn mongo_constraints(schema: &Schema) -> TokenStream {
    let collections = lumen_migrate::mongo::calculate(schema);
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for model in schema
        .models
        .iter()
        .filter(|m| m.kind == ModelKind::Model && !m.ignored)
    {
        let collection = model.database_name();
        let field_of = |name: &str| {
            model
                .fields
                .iter()
                .find(|f| f.database_name() == name)
                .map_or_else(|| name.to_owned(), |f| f.name.clone())
        };
        map.insert(format!("{collection}._id_"), vec![field_of("_id")]);
        let Some(c) = collections.iter().find(|c| c.name == collection) else {
            continue;
        };
        for i in c.indexes.iter().filter(|i| i.unique) {
            map.insert(
                format!("{collection}.{}", i.name),
                i.keys.iter().map(|(k, _)| field_of(k)).collect(),
            );
        }
    }
    let entries = map
        .iter()
        .map(|(name, fields)| quote!((#name.to_owned(), vec![#(#fields.to_owned()),*])));
    quote! {
        fn constraints() -> ::std::collections::HashMap<String, Vec<String>> {
            ::std::collections::HashMap::from([#(#entries),*])
        }
    }
}

fn mongo_client(schema: &Schema, models: &[ModelId]) -> TokenStream {
    let accessors = models.iter().map(|id| {
        let m = &schema.models[id.0];
        let module = ident(&snake(&m.name));
        let doc = format!(" Operations on `{}`.", m.name);
        quote! {
            #[doc = #doc]
            #[must_use]
            pub fn #module(&self) -> #module::Actions<&::lumen_orm::mongo::MongoDb> { #module::on(&self.db) }
        }
    });
    let constraints = mongo_constraints(schema);
    quote! {
        /// The client: one accessor per model.
        #[derive(Debug, Clone)]
        pub struct Client { db: ::lumen_orm::mongo::MongoDb }

        impl Client {
            /// Connect to `url` and use `database`.
            ///
            /// # Errors
            /// P1001 when the server cannot be reached.
            pub async fn connect(url: &str, database: &str) -> ::lumen_orm::Result<Self> {
                Ok(Self::new(::lumen_orm::mongo::MongoDb::connect(url, database).await?))
            }

            /// Use an existing connection.
            #[must_use]
            pub fn new(db: ::lumen_orm::mongo::MongoDb) -> Self {
                Self { db: db.with_constraints(constraints()) }
            }

            /// The connection, for transactions (`client.db().transaction(..)`) and dynamic queries.
            #[must_use]
            pub fn db(&self) -> &::lumen_orm::mongo::MongoDb { &self.db }

            #(#accessors)*
        }

        #constraints
    }
}

/// The client for `schema`, as formatted Rust source. `source` is the schema's path, written in
/// the file's header.
///
/// # Errors
/// When the generator's `dateTimeType` is not `time` or `chrono`, or the generated code does not
/// parse (a bug in lumen; please report it).
pub fn generate(schema: &Schema, source: &str) -> Result<String, String> {
    types::dates(schema)?;
    let models: Vec<ModelId> = schema
        .models
        .iter()
        .enumerate()
        .filter(|(_, m)| m.kind == ModelKind::Model && !m.ignored)
        .map(|(i, _)| ModelId(i))
        .collect();
    let header = format!(
        " Generated by lumen {VERSION} from `{source}`. Do not edit: run `lumen generate`.\n Include it with `#[rustfmt::skip] mod db;` so `cargo fmt` leaves it as generated."
    );
    let enums = enums(schema);
    let mongo = schema.provider() == lumen_schema::checked::Provider::MongoDb;
    let composites = if mongo {
        mongo_model::composites(schema)
    } else {
        quote!()
    };
    let modules: Vec<TokenStream> = models
        .iter()
        .map(|id| {
            if mongo {
                mongo_model::module(schema, *id)
            } else {
                model::module(schema, *id)
            }
        })
        .collect();
    let client = if mongo {
        mongo_client(schema, &models)
    } else {
        client(schema, &models)
    };
    let file = quote! {
        #![doc = #header]
        #![allow(clippy::all, clippy::pedantic, dead_code, unused_imports, unused_mut, missing_docs)]
        #enums
        #composites
        #(#modules)*
        #client
    };
    let parsed: syn::File =
        syn::parse2(file).map_err(|e| format!("lumen generated code that does not parse: {e}"))?;
    Ok(prettyplease::unparse(&parsed))
}
