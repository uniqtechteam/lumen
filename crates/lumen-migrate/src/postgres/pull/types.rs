//! Column types and defaults as schema text. A scalar type (with `@db.X` when the column is not
//! the scalar's usual type) is used only when it gives back exactly the column's type; anything
//! else is `Unsupported("..")`. Defaults the schema cannot say become `dbgenerated("..")`.

use lumen_schema::checked::{NativeType, ScalarType};

use crate::postgres::types::column_type;
use crate::shape::Default;

/// What a column holds, as the schema says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A built-in scalar, with its native type when it is not the usual one.
    Scalar(ScalarType, Option<NativeType>),
    /// An enum: the index into the pulled enums.
    Enum(usize),
    /// `Unsupported("..")`, with the type as Postgres prints it.
    Unsupported(String),
}

/// A column type: what it holds and whether it is a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnKind {
    /// The element type.
    pub kind: Kind,
    /// `[]`.
    pub list: bool,
}

fn args(ty: &str) -> Vec<u32> {
    ty.split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(inside, _)| {
            inside
                .split(',')
                .filter_map(|a| a.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "reads as the `Option` the scalar's native type is"
)]
fn native(name: &str, args: Vec<u32>) -> Option<NativeType> {
    Some(NativeType {
        name: name.to_owned(),
        args,
    })
}

/// The scalar and native type that might print as `ty`; checked against [`column_type`].
fn scalar_guess(ty: &str) -> Option<(ScalarType, Option<NativeType>)> {
    use ScalarType as S;
    let head = ty.split('(').next().unwrap_or(ty).trim();
    let a = args(ty);
    let with_zone = ty.ends_with("with time zone");
    Some(match head {
        "text" => (S::String, None),
        "character varying" => (S::String, native("VarChar", a)),
        "character" => (S::String, native("Char", a)),
        "bit" => (S::String, native("Bit", a)),
        "bit varying" => (S::String, native("VarBit", a)),
        "uuid" => (S::String, native("Uuid", a)),
        "xml" => (S::String, native("Xml", a)),
        "inet" => (S::String, native("Inet", a)),
        "citext" => (S::String, native("Citext", a)),
        "integer" => (S::Int, None),
        "smallint" => (S::Int, native("SmallInt", a)),
        "oid" => (S::Int, native("Oid", a)),
        "bigint" => (S::BigInt, None),
        "double precision" => (S::Float, None),
        "real" => (S::Float, native("Real", a)),
        "numeric" if a == [65, 30] => (S::Decimal, None),
        "numeric" => (S::Decimal, native("Decimal", a)),
        "money" => (S::Decimal, native("Money", a)),
        "boolean" => (S::Boolean, None),
        "timestamp" if a == [3] && !with_zone => (S::DateTime, None),
        "timestamp" if with_zone => (S::DateTime, native("Timestamptz", a)),
        "timestamp" | "timestamp without time zone" => (S::DateTime, native("Timestamp", a)),
        "timestamp with time zone" => (S::DateTime, native("Timestamptz", a)),
        "date" => (S::DateTime, native("Date", a)),
        "time" if with_zone => (S::DateTime, native("Timetz", a)),
        "time" | "time without time zone" => (S::DateTime, native("Time", a)),
        "time with time zone" => (S::DateTime, native("Timetz", a)),
        "jsonb" => (S::Json, None),
        "json" => (S::Json, native("Json", a)),
        "bytea" => (S::Bytes, None),
        _ => return None,
    })
}

/// How the column type `ty` is written. `enums` are the pulled enums' type names as
/// `format_type` prints them, with and without their schema.
#[must_use]
pub fn column_kind(ty: &str, enums: &[Vec<String>]) -> ColumnKind {
    let (element, list) = match ty.strip_suffix("[]") {
        Some(e) if !e.ends_with("[]") => (e, true),
        _ => (ty, false),
    };
    if let Some(i) = enums
        .iter()
        .position(|names| names.iter().any(|n| n == element))
    {
        return ColumnKind {
            kind: Kind::Enum(i),
            list,
        };
    }
    match scalar_guess(element) {
        Some((s, n)) if column_type(s, n.as_ref()) == element => ColumnKind {
            kind: Kind::Scalar(s, n),
            list,
        },
        // `Unsupported("x[]")` keeps NOT NULL, which a list field cannot
        _ => ColumnKind {
            kind: Kind::Unsupported(ty.to_owned()),
            list: false,
        },
    }
}

/// `@db.VarChar(320)`, with the datasource's name.
#[must_use]
pub fn native_attribute(datasource: &str, n: &NativeType) -> String {
    if n.args.is_empty() {
        format!("@{datasource}.{}", n.name)
    } else {
        let a: Vec<String> = n.args.iter().map(u32::to_string).collect();
        format!("@{datasource}.{}({})", n.name, a.join(", "))
    }
}

/// A schema string literal: `"it's \"x\""`.
#[must_use]
pub fn string(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `'it''s'` → `it's`.
fn unquote(sql: &str) -> Option<String> {
    let inner = sql.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(inner.replace("''", "'"))
}

/// The items of an array literal `{a,"b c",NULL}`, without quotes; `None` when one is NULL.
fn array_items(text: &str) -> Option<Vec<String>> {
    let body = text.strip_prefix('{')?.strip_suffix('}')?;
    let mut items = Vec::new();
    if body.is_empty() {
        return Some(items);
    }
    let mut chars = body.chars().peekable();
    loop {
        let mut item = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next()? {
                    '\\' => item.push(chars.next()?),
                    '"' => break,
                    c => item.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == ',' {
                    break;
                }
                item.push(c);
                chars.next();
            }
            if item.eq_ignore_ascii_case("null") || item.starts_with('{') {
                return None;
            }
        }
        items.push(item);
        match chars.next() {
            Some(',') => {}
            None => return Some(items),
            Some(_) => return None,
        }
    }
}

/// One literal value for an element of type `kind`; `values` are an enum's (database name,
/// schema name) pairs.
fn element(kind: &Kind, value: &str, values: &[(String, String)]) -> Option<String> {
    use ScalarType as S;
    match kind {
        Kind::Scalar(S::String | S::Json | S::DateTime | S::Bytes, _) => Some(string(value)),
        Kind::Scalar(S::Int | S::BigInt, _) => {
            value.parse::<i64>().is_ok().then(|| value.to_owned())
        }
        Kind::Scalar(S::Float | S::Decimal, _) => value
            .parse::<f64>()
            .is_ok_and(f64::is_finite)
            .then(|| value.to_owned()),
        Kind::Scalar(S::Boolean, _) => match value {
            "t" | "true" => Some("true".to_owned()),
            "f" | "false" => Some("false".to_owned()),
            _ => None,
        },
        Kind::Enum(_) => values
            .iter()
            .find(|(db, _)| db == value)
            .map(|(_, name)| name.clone()),
        Kind::Unsupported(_) => None,
    }
}

/// The `@default(..)` for a column; `None` when it has none. `values` are the enum's values
/// when the column is an enum.
#[must_use]
pub fn default(
    column: &ColumnKind,
    default: &Default,
    values: &[(String, String)],
) -> Option<String> {
    use ScalarType as S;
    let inner = match default {
        Default::Sequence => match column.kind {
            Kind::Scalar(S::Int | S::BigInt, _) if !column.list => "autoincrement()".to_owned(),
            _ => return None,
        },
        Default::Now => match column.kind {
            Kind::Scalar(S::DateTime, _) if !column.list => "now()".to_owned(),
            _ => "dbgenerated(\"CURRENT_TIMESTAMP\")".to_owned(),
        },
        Default::Literal(sql) => {
            literal(column, sql, values).unwrap_or_else(|| format!("dbgenerated({})", string(sql)))
        }
        Default::Expression(sql) => format!("dbgenerated({})", string(sql)),
    };
    Some(format!("@default({inner})"))
}

fn literal(column: &ColumnKind, sql: &str, values: &[(String, String)]) -> Option<String> {
    if column.list {
        let items = array_items(&unquote(sql)?)?;
        let out: Option<Vec<String>> = items
            .iter()
            .map(|i| element(&column.kind, i, values))
            .collect();
        return Some(format!("[{}]", out?.join(", ")));
    }
    match unquote(sql) {
        // a quoted number on a number column was unquoted by the describer already
        Some(text) => match &column.kind {
            Kind::Scalar(
                ScalarType::String | ScalarType::Json | ScalarType::DateTime | ScalarType::Bytes,
                _,
            )
            | Kind::Enum(_) => element(&column.kind, &text, values),
            _ => None,
        },
        None => match &column.kind {
            Kind::Scalar(
                ScalarType::Int
                | ScalarType::BigInt
                | ScalarType::Float
                | ScalarType::Decimal
                | ScalarType::Boolean,
                _,
            ) => element(&column.kind, sql, values),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{ColumnKind, Kind, column_kind, default};
    use crate::shape::Default;
    use lumen_schema::checked::ScalarType;

    fn kind(ty: &str) -> String {
        format!("{:?}", column_kind(ty, &[vec!["\"Role\"".into()]]))
    }

    #[test]
    fn types_come_back_exactly_or_not_at_all() {
        assert!(kind("text").contains("Scalar(String, None)"));
        assert!(kind("character varying(320)").contains("VarChar"));
        assert!(kind("numeric(65,30)").contains("Scalar(Decimal, None)"));
        assert!(kind("numeric(10,2)").contains("args: [10, 2]"));
        assert!(kind("timestamp(3) without time zone").contains("Scalar(DateTime, None)"));
        assert!(kind("timestamp(6) with time zone").contains("Timestamptz"));
        assert!(kind("timestamp without time zone").contains("Timestamp\""));
        assert!(kind("time(6) without time zone").contains("\"Time\""));
        assert!(kind("text[]").contains("list: true"));
        assert!(kind("\"Role\"[]").contains("Enum(0), list: true"));
        assert!(kind("tsvector").contains("Unsupported(\"tsvector\")"));
        assert!(kind("interval[]").contains("Unsupported(\"interval[]\"), list: false"));
        assert!(kind("integer[][]").contains("Unsupported"));
    }

    #[test]
    fn defaults_as_schema_text() {
        let col = |kind, list| ColumnKind { kind, list };
        let s = col(Kind::Scalar(ScalarType::String, None), false);
        let lit = |l: &str| Default::Literal(l.to_owned());
        assert_eq!(
            default(&s, &lit("'it''s'"), &[]).as_deref(),
            Some("@default(\"it's\")")
        );
        let n = col(Kind::Scalar(ScalarType::Int, None), false);
        assert_eq!(
            default(&n, &Default::Sequence, &[]).as_deref(),
            Some("@default(autoincrement())")
        );
        assert_eq!(
            default(&n, &lit("-2"), &[]).as_deref(),
            Some("@default(-2)")
        );
        let tags = col(Kind::Scalar(ScalarType::String, None), true);
        assert_eq!(
            default(&tags, &lit("'{a,\"b c\"}'"), &[]).as_deref(),
            Some("@default([\"a\", \"b c\"])")
        );
        let roles = col(Kind::Enum(0), true);
        let values = [("writer".to_owned(), "WRITER".to_owned())];
        assert_eq!(
            default(&roles, &lit("'{writer}'"), &values).as_deref(),
            Some("@default([WRITER])")
        );
        let u = col(Kind::Unsupported("point".into()), false);
        assert_eq!(
            default(&u, &lit("'(0,0)'"), &[]).as_deref(),
            Some("@default(dbgenerated(\"'(0,0)'\"))")
        );
        assert_eq!(
            default(&s, &Default::Expression("gen_random_uuid()".into()), &[]).as_deref(),
            Some("@default(dbgenerated(\"gen_random_uuid()\"))")
        );
    }
}
