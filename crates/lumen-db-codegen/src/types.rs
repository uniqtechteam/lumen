//! Rust names and types for schema items.

use lumen_schema::ast::Arity;
use lumen_schema::checked::{Field, FieldKind, Provider, ScalarType, Schema, StringOrEnv};
use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;

const KEYWORDS: [&str; 39] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while", "gen", "try",
];

/// `createdAt` → `created_at`.
#[must_use]
pub fn snake(name: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_uppercase() {
            if prev_lower {
                out.push('_');
            }
            out.extend(c.to_lowercase());
            prev_lower = false;
        } else {
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
            out.push(if c == '-' { '_' } else { c });
        }
    }
    out
}

/// `ADMIN` / `writer` → `Admin` / `Writer`.
#[must_use]
pub fn pascal(name: &str) -> String {
    name.split(['_', '-'])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let lower = if p.chars().all(|c| c.is_uppercase() || c.is_ascii_digit()) {
                p.to_lowercase()
            } else {
                p.to_owned()
            };
            let mut c = lower.chars();
            c.next()
                .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        })
        .collect()
}

/// An identifier, raw (`r#type`) when it is a keyword.
#[must_use]
pub fn ident(name: &str) -> Ident {
    if KEYWORDS.contains(&name) {
        Ident::new_raw(name, Span::call_site())
    } else {
        Ident::new(name, Span::call_site())
    }
}

/// The crate a Postgres client's dates come from: the lumen generator's `dateTimeType` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dates {
    /// `time` (the default).
    Time,
    /// `chrono`, which needs lumen-orm's `chrono` feature.
    Chrono,
}

/// The `dateTimeType` the schema's lumen generator asks for.
///
/// # Errors
/// When the setting names something other than `time` or `chrono`.
pub fn dates(schema: &Schema) -> Result<Dates, String> {
    let setting = schema
        .generators
        .iter()
        .filter(|g| matches!(&g.provider, Some(StringOrEnv::Literal(p)) if p.starts_with("lumen")))
        .flat_map(|g| &g.settings)
        .find(|(name, _)| name == "dateTimeType");
    match setting.map(|(_, v)| v.as_str()) {
        None | Some("time") => Ok(Dates::Time),
        Some("chrono") => Ok(Dates::Chrono),
        Some(other) => Err(format!(
            "dateTimeType = \"{other}\" is not supported; use \"time\" (the default) or \"chrono\""
        )),
    }
}

/// A Postgres `DateTime` field's type, by native type: `@db.Timestamptz` keeps the offset,
/// `@db.Date` and `@db.Time` hold a date or a time of day, anything else is a UTC timestamp.
fn date_type(schema: &Schema, native: Option<&str>) -> TokenStream {
    match (dates(schema).unwrap_or(Dates::Time), native) {
        (Dates::Time, Some("Timestamptz")) => quote!(::lumen_orm::time::OffsetDateTime),
        (Dates::Time, Some("Date")) => quote!(::lumen_orm::time::Date),
        (Dates::Time, Some("Time")) => quote!(::lumen_orm::time::Time),
        (Dates::Time, _) => quote!(::lumen_orm::time::PrimitiveDateTime),
        (Dates::Chrono, Some("Timestamptz")) => {
            quote!(::lumen_orm::chrono::DateTime<::lumen_orm::chrono::Utc>)
        }
        (Dates::Chrono, Some("Date")) => quote!(::lumen_orm::chrono::NaiveDate),
        (Dates::Chrono, Some("Time")) => quote!(::lumen_orm::chrono::NaiveTime),
        (Dates::Chrono, _) => quote!(::lumen_orm::chrono::NaiveDateTime),
    }
}

/// `#[serde(with = ..)]` for a Postgres record field, so `time` dates are ISO 8601 text in JSON
/// (`chrono` does that by itself; `time::Date` and `time::Time` already are).
pub fn serde_with(schema: &Schema, field: &Field) -> Option<TokenStream> {
    if field.kind != FieldKind::Scalar(ScalarType::DateTime)
        || schema.provider() == Provider::MongoDb
        || dates(schema).unwrap_or(Dates::Time) != Dates::Time
    {
        return None;
    }
    let module = match field.native_type.as_ref().map(|n| n.name.as_str()) {
        Some("Date" | "Time") => return None,
        Some("Timestamptz") => "::lumen_orm::time_serde::rfc3339",
        _ => "::lumen_orm::time_serde::timestamp",
    };
    let path = match field.arity {
        Arity::Required => module.to_owned(),
        Arity::Optional => format!("{module}::option"),
        Arity::List => return None,
    };
    Some(quote!(#[serde(with = #path)]))
}

/// The Rust type of a field's value without `Option` or `Vec`, as seen from a model module.
pub fn base_type(schema: &Schema, field: &Field) -> Option<TokenStream> {
    base_type_at(schema, field, 1)
}

/// Like [`base_type`], from `depth` modules below the file's top (enums live at the top).
pub fn base_type_at(schema: &Schema, field: &Field, depth: usize) -> Option<TokenStream> {
    let native = field.native_type.as_ref().map(|n| n.name.as_str());
    if schema.provider() == Provider::MongoDb {
        return mongo_type(schema, field, depth);
    }
    Some(match &field.kind {
        FieldKind::Scalar(s) => match (s, native) {
            (ScalarType::String, Some("Uuid")) => quote!(::lumen_orm::uuid::Uuid),
            (ScalarType::String, _) => quote!(String),
            (ScalarType::Int, _) => quote!(i32),
            (ScalarType::BigInt, _) => quote!(i64),
            (ScalarType::Float, _) => quote!(f64),
            (ScalarType::Decimal, _) => quote!(::lumen_orm::rust_decimal::Decimal),
            (ScalarType::Boolean, _) => quote!(bool),
            (ScalarType::DateTime, _) => date_type(schema, native),
            (ScalarType::Json, _) => quote!(::lumen_orm::serde_json::Value),
            (ScalarType::Bytes, _) => quote!(Vec<u8>),
        },
        FieldKind::Enum(id) => {
            let name = ident(&schema.enums[id.0].name);
            let up = std::iter::repeat_n(quote!(super::), depth);
            quote!(#(#up)* #name)
        }
        FieldKind::Model(_)
        | FieldKind::Composite(_)
        | FieldKind::Unsupported(_)
        | FieldKind::Unknown => {
            return None;
        }
    })
}

/// MongoDB types: ObjectIds, BSON dates and decimals, and structs for composite types.
fn mongo_type(schema: &Schema, field: &Field, depth: usize) -> Option<TokenStream> {
    let native = field.native_type.as_ref().map(|n| n.name.as_str());
    let up: Vec<TokenStream> = std::iter::repeat_n(quote!(super::), depth).collect();
    Some(match &field.kind {
        FieldKind::Scalar(ScalarType::String) if native == Some("ObjectId") => {
            quote!(::lumen_orm::mongo::ObjectId)
        }
        FieldKind::Scalar(ScalarType::String) => quote!(String),
        FieldKind::Scalar(ScalarType::Int) => quote!(i32),
        FieldKind::Scalar(ScalarType::BigInt) => quote!(i64),
        FieldKind::Scalar(ScalarType::Float) => quote!(f64),
        FieldKind::Scalar(ScalarType::Decimal) => quote!(::lumen_orm::mongo::Decimal128),
        FieldKind::Scalar(ScalarType::Boolean) => quote!(bool),
        FieldKind::Scalar(ScalarType::DateTime) => quote!(::lumen_orm::mongo::DateTime),
        FieldKind::Scalar(ScalarType::Json) => quote!(::lumen_orm::serde_json::Value),
        FieldKind::Scalar(ScalarType::Bytes) => quote!(::lumen_orm::mongo::Binary),
        FieldKind::Enum(id) => {
            let name = ident(&schema.enums[id.0].name);
            quote!(#(#up)* #name)
        }
        FieldKind::Composite(id) => {
            let name = ident(&schema.composite_types[id.0].name);
            quote!(#(#up)* #name)
        }
        FieldKind::Model(_) | FieldKind::Unsupported(_) | FieldKind::Unknown => return None,
    })
}

/// The Rust type of a field, with `Option` / `Vec`.
pub fn field_type(schema: &Schema, field: &Field) -> Option<TokenStream> {
    let base = base_type(schema, field)?;
    Some(match field.arity {
        Arity::Required => base,
        Arity::Optional => quote!(Option<#base>),
        Arity::List => quote!(Vec<#base>),
    })
}

/// Whether values of this field can be a relation key (`Eq + Hash`).
#[must_use]
pub fn hashable(field: &Field) -> bool {
    matches!(
        field.kind,
        FieldKind::Scalar(
            ScalarType::String | ScalarType::Int | ScalarType::BigInt | ScalarType::Boolean
        )
    )
}

#[cfg(test)]
mod tests {
    use super::{pascal, snake};

    #[test]
    fn names() {
        assert_eq!(snake("createdAt"), "created_at");
        assert_eq!(snake("userId"), "user_id");
        assert_eq!(snake("id"), "id");
        assert_eq!(pascal("ADMIN"), "Admin");
        assert_eq!(pascal("writer"), "Writer");
        assert_eq!(pascal("IN_PROGRESS"), "InProgress");
        assert_eq!(pascal("OrderLine"), "OrderLine");
    }
}
