//! `aggregate()` and `group_by()` for a Postgres model: typed aggregate values per field
//! (`order::total::sum()`), the result types and the two builders. Everything lives in the model's
//! `aggregates` module, so no generated name can meet a model's own name.

use lumen_schema::ast::Arity;
use lumen_schema::checked::{Field, FieldKind, Model, ScalarType, Schema};
use proc_macro2::TokenStream;
use quote::quote;

use crate::model::columns;
use crate::types::{base_type_at, ident, snake};

/// The module name: `aggregates`, or `aggregates_` when a field already uses that name.
pub(crate) fn module_name(model: &Model) -> String {
    if model.fields.iter().any(|f| snake(&f.name) == "aggregates") {
        "aggregates_".to_owned()
    } else {
        "aggregates".to_owned()
    }
}

fn scalar(f: &Field) -> Option<ScalarType> {
    match f.kind {
        FieldKind::Scalar(s) if f.arity != Arity::List => Some(s),
        _ => None,
    }
}

fn is_uuid(f: &Field) -> bool {
    f.native_type.as_ref().is_some_and(|n| n.name == "Uuid")
}

/// `SUM` of the field: its Rust type and the cast that gives it that type.
fn sum_type(f: &Field) -> Option<(TokenStream, Option<&'static str>)> {
    match scalar(f)? {
        ScalarType::Int | ScalarType::BigInt => Some((quote!(i64), Some("bigint"))),
        ScalarType::Float => Some((quote!(f64), Some("double precision"))),
        ScalarType::Decimal => Some((quote!(::lumen_orm::rust_decimal::Decimal), None)),
        _ => None,
    }
}

/// `AVG` of the field: its Rust type and cast.
fn avg_type(f: &Field) -> Option<(TokenStream, Option<&'static str>)> {
    match scalar(f)? {
        ScalarType::Int | ScalarType::BigInt | ScalarType::Float => {
            Some((quote!(f64), Some("double precision")))
        }
        ScalarType::Decimal => Some((quote!(::lumen_orm::rust_decimal::Decimal), None)),
        _ => None,
    }
}

/// Whether `MIN` and `MAX` work on the field (and give back its own type).
fn has_min_max(f: &Field) -> bool {
    matches!(
        scalar(f),
        Some(
            ScalarType::String
                | ScalarType::Int
                | ScalarType::BigInt
                | ScalarType::Float
                | ScalarType::Decimal
                | ScalarType::DateTime
        )
    ) && !is_uuid(f)
}

fn countable(f: &Field) -> bool {
    f.arity != Arity::List
}

fn cast_tokens(cast: Option<&str>) -> TokenStream {
    match cast {
        Some(c) => quote!(.cast(#c)),
        None => quote!(),
    }
}

/// The functions added to a field's module: `count()`, and `sum()`, `avg()`, `min()`, `max()`
/// where the field's type allows them.
pub(crate) fn field_fns(model: &Model, f: &Field) -> TokenStream {
    let m = ident(&module_name(model));
    let mut fns = Vec::new();
    if countable(f) {
        fns.push(quote! {
            /// How many records have a value here (not NULL), for `aggregate()` and `group_by()`.
            pub fn count() -> super::#m::Value { super::#m::Value::new(::lumen_orm::Agg::count(COLUMN)) }
        });
    }
    if let Some((_, cast)) = sum_type(f) {
        let cast = cast_tokens(cast);
        fns.push(quote! {
            /// The sum, for `aggregate()` and `group_by()`.
            pub fn sum() -> super::#m::Value { super::#m::Value::new(::lumen_orm::Agg::sum(COLUMN) #cast) }
        });
    }
    if let Some((_, cast)) = avg_type(f) {
        let cast = cast_tokens(cast);
        fns.push(quote! {
            /// The average, for `aggregate()` and `group_by()`.
            pub fn avg() -> super::#m::Value { super::#m::Value::new(::lumen_orm::Agg::avg(COLUMN) #cast) }
        });
    }
    if has_min_max(f) {
        fns.push(quote! {
            /// The smallest value, for `aggregate()` and `group_by()`.
            pub fn min() -> super::#m::Value { super::#m::Value::new(::lumen_orm::Agg::min(COLUMN)) }
            /// The largest value, for `aggregate()` and `group_by()`.
            pub fn max() -> super::#m::Value { super::#m::Value::new(::lumen_orm::Agg::max(COLUMN)) }
        });
    }
    quote!(#(#fns)*)
}

/// One result struct (`Counts`, `Sums`, ..): a field per model field that has the aggregate.
fn values_struct(
    name: &str,
    doc: &str,
    fields: &[(&Field, TokenStream, String)],
    reader: &TokenStream,
) -> (TokenStream, TokenStream) {
    let name = ident(name);
    let decl = fields.iter().map(|(f, ty, _)| {
        let id = ident(&snake(&f.name));
        let d = format!(" `{}`.", f.name);
        quote!(#[doc = #d] pub #id: Option<#ty>,)
    });
    let reads = fields.iter().map(|(f, _, alias)| {
        let id = ident(&snake(&f.name));
        quote!(#id: #reader(row, #alias)?,)
    });
    let decl = quote! {
        #[doc = #doc]
        #[derive(Debug, Clone, PartialEq, Default)]
        pub struct #name { #(#decl)* }
    };
    let read = quote!(#name { #(#reads)* });
    (decl, read)
}

fn alias(func: &str, f: &Field) -> String {
    format!("_{func}_{}", f.database_name())
}

/// The model's `aggregates` module and the `aggregate()` / `group_by()` actions, for Postgres or
/// MongoDB.
pub(crate) fn model_code(schema: &Schema, model: &Model, mongo: bool) -> TokenStream {
    let m = ident(&module_name(model));
    let cols: Vec<&Field> = columns(schema, model).into_iter().map(|(f, _)| f).collect();
    let pick = |func: &str, ty: &dyn Fn(&Field) -> Option<TokenStream>| {
        cols.iter()
            .filter_map(|f| ty(f).map(|t| (*f, t, alias(func, f))))
            .collect::<Vec<_>>()
    };
    let own = |f: &Field| base_type_at(schema, f, 2);
    // MongoDB keeps decimals as Decimal128; Postgres reads numeric as rust_decimal
    let decimal = |t: TokenStream| {
        if mongo && t.to_string().contains("Decimal") {
            quote!(::lumen_orm::mongo::Decimal128)
        } else {
            t
        }
    };
    let reader = if mongo {
        quote!(::lumen_orm::mongo::read_aggregate)
    } else {
        quote!(::lumen_orm::read_aggregate)
    };
    let counts = pick("count", &|f| countable(f).then(|| quote!(i64)));
    let sums = pick("sum", &|f| sum_type(f).map(|(t, _)| decimal(t)));
    let avgs = pick("avg", &|f| avg_type(f).map(|(t, _)| decimal(t)));
    let mins = pick("min", &|f| if has_min_max(f) { own(f) } else { None });
    let maxes = pick("max", &|f| if has_min_max(f) { own(f) } else { None });
    let (counts_decl, counts_read) =
        values_struct("Counts", " Non-NULL counts per field.", &counts, &reader);
    let (sums_decl, sums_read) = values_struct("Sums", " Sums per numeric field.", &sums, &reader);
    let (avgs_decl, avgs_read) =
        values_struct("Avgs", " Averages per numeric field.", &avgs, &reader);
    let (mins_decl, mins_read) =
        values_struct("Mins", " Smallest values per field.", &mins, &reader);
    let (maxes_decl, maxes_read) =
        values_struct("Maxes", " Largest values per field.", &maxes, &reader);
    let keys: Vec<(&Field, TokenStream, String)> = cols
        .iter()
        .filter(|f| f.arity != Arity::List)
        .filter_map(|f| own(f).map(|t| (*f, t, f.database_name().to_owned())))
        .collect();
    let (key_decl, key_read) = values_struct(
        "Key",
        " The group's values of the `group_by` fields; the others are `None`.",
        &keys,
        &reader,
    );
    let read_body = quote! {
        Ok(Self {
            count: #reader(row, "_count")?,
            count_of: #counts_read,
            sum: #sums_read,
            avg: #avgs_read,
            min: #mins_read,
            max: #maxes_read,
        })
    };
    let backend = backend_code(mongo, &key_read, &read_body);
    let actions = actions_code(&m, mongo);
    let model_name = &model.name;
    let module_doc = format!(" `aggregate()` and `group_by()` on `{model_name}`.");
    quote! {
        #[doc = #module_doc]
        pub mod #m {
            /// Tags this model's aggregate values and conditions, and names its result types.
            #[derive(Debug, Clone, Copy)]
            pub struct Model;

            /// An aggregate value: `count()`, or `count()`, `sum()`, `avg()`, `min()`, `max()` of a
            /// field, for example `order::total::sum()`.
            pub type Value = ::lumen_orm::AggValue<Model>;
            /// A condition on groups.
            pub type Having = ::lumen_orm::Having<Model>;

            /// `COUNT(*)`: the number of records (in each group).
            pub fn count() -> Value { Value::new(::lumen_orm::Agg::count_all()) }

            #counts_decl
            #sums_decl
            #avgs_decl
            #mins_decl
            #maxes_decl
            #key_decl

            /// The aggregate values asked for; the rest are `None`, and so are sums, averages and
            /// extremes over no records.
            #[derive(Debug, Clone, PartialEq, Default)]
            pub struct Aggregate {
                /// `count()`: the number of records.
                pub count: Option<i64>,
                /// `<field>::count()`.
                pub count_of: Counts,
                /// `<field>::sum()`.
                pub sum: Sums,
                /// `<field>::avg()`.
                pub avg: Avgs,
                /// `<field>::min()`.
                pub min: Mins,
                /// `<field>::max()`.
                pub max: Maxes,
            }

            /// One group of `group_by()`: its key and its aggregate values.
            #[derive(Debug, Clone, PartialEq, Default)]
            pub struct Group {
                /// The values the group's records share.
                pub key: Key,
                /// The aggregate values over the group.
                pub values: Aggregate,
            }

            #backend
        }

        #actions
    }
}

/// The parts of the `aggregates` module that differ between Postgres and MongoDB: the model
/// trait, the builders and how a result is read.
fn backend_code(mongo: bool, key_read: &TokenStream, read_body: &TokenStream) -> TokenStream {
    if mongo {
        quote! {
            impl ::lumen_orm::mongo::MongoAggModel for Model {
                type Aggregate = Aggregate;
                type Group = Group;
                fn aggregate(row: &::lumen_orm::mongo::Document) -> ::lumen_orm::Result<Aggregate> { Aggregate::read(row) }
                fn group(row: &::lumen_orm::mongo::Document) -> ::lumen_orm::Result<Group> {
                    Ok(Group { key: #key_read, values: Aggregate::read(row)? })
                }
            }
            /// The builder `aggregate()` returns.
            pub type Query<R> = ::lumen_orm::mongo::MongoAggregateQuery<R, Model>;
            /// The builder `group_by()` returns.
            pub type GroupBy<R> = ::lumen_orm::mongo::MongoGroupByQuery<R, Model>;
            impl Aggregate {
                fn read(row: &::lumen_orm::mongo::Document) -> ::lumen_orm::Result<Self> { #read_body }
            }
        }
    } else {
        quote! {
            impl ::lumen_orm::AggModel for Model {
                type Aggregate = Aggregate;
                type Group = Group;
            }
            /// The builder `aggregate()` returns.
            pub type Query<R> = ::lumen_orm::AggregateQuery<R, Model>;
            /// The builder `group_by()` returns.
            pub type GroupBy<R> = ::lumen_orm::GroupByQuery<R, Model>;
            impl Aggregate {
                fn read(row: &::lumen_orm::sqlx::postgres::PgRow) -> ::core::result::Result<Self, ::lumen_orm::sqlx::Error> { #read_body }
            }
            impl<'r> ::lumen_orm::sqlx::FromRow<'r, ::lumen_orm::sqlx::postgres::PgRow> for Aggregate {
                fn from_row(row: &'r ::lumen_orm::sqlx::postgres::PgRow) -> ::core::result::Result<Self, ::lumen_orm::sqlx::Error> {
                    Self::read(row)
                }
            }
            impl<'r> ::lumen_orm::sqlx::FromRow<'r, ::lumen_orm::sqlx::postgres::PgRow> for Group {
                fn from_row(row: &'r ::lumen_orm::sqlx::postgres::PgRow) -> ::core::result::Result<Self, ::lumen_orm::sqlx::Error> {
                    Ok(Self { key: #key_read, values: Aggregate::read(row)? })
                }
            }
        }
    }
}

/// `aggregate()` and `group_by()` on the model's `Actions`.
fn actions_code(m: &proc_macro2::Ident, mongo: bool) -> TokenStream {
    if mongo {
        quote! {
            impl<R: ::lumen_orm::mongo::MongoRun> Actions<R> {
                /// Count, sum, average, min and max over the documents that match, in one pipeline:
                /// `db.order().aggregate().count().value(order::total::sum()).exec()`.
                pub fn aggregate(self) -> #m::Query<R> {
                    ::lumen_orm::mongo::MongoAggregateQuery::new(self.run, COLLECTION)
                }

                /// Aggregate values per group of documents sharing `fields`:
                /// `db.order().group_by([order::status::COLUMN]).count().exec()`.
                pub fn group_by<'a>(self, fields: impl IntoIterator<Item = &'a str>) -> #m::GroupBy<R> {
                    ::lumen_orm::mongo::MongoGroupByQuery::new(self.run, COLLECTION, fields)
                }
            }
        }
    } else {
        quote! {
            impl<R: ::lumen_orm::Run> Actions<R> {
                /// Count, sum, average, min and max over the records that match, in one statement:
                /// `db.order().aggregate().count().value(order::total::sum()).exec()`.
                pub fn aggregate(self) -> #m::Query<R> {
                    ::lumen_orm::AggregateQuery::new(self.run, table())
                }

                /// Aggregate values per group of records sharing `columns`:
                /// `db.order().group_by([order::status::COLUMN]).count().exec()`.
                pub fn group_by<'a>(self, columns: impl IntoIterator<Item = &'a str>) -> #m::GroupBy<R> {
                    ::lumen_orm::GroupByQuery::new(self.run, table(), columns)
                }
            }
        }
    }
}
