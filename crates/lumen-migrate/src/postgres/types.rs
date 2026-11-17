//! Postgres type names and default expressions, in the canonical form both sides of a diff use.

use lumen_schema::checked::{NativeType, ScalarType};

use crate::shape::Default;

fn precision(args: &[u32]) -> String {
    match args.first() {
        Some(p) => format!("({p})"),
        None => String::new(),
    }
}

/// The type `format_type()` prints for a scalar with an optional `@db.X`. The defaults:
/// `text`, `integer`, `bigint`, `double precision`, `numeric(65,30)`, `boolean`,
/// `timestamp(3) without time zone`, `jsonb`, `bytea`.
#[must_use]
pub fn column_type(scalar: ScalarType, native: Option<&NativeType>) -> String {
    let Some(n) = native else {
        return match scalar {
            ScalarType::String => "text",
            ScalarType::Int => "integer",
            ScalarType::BigInt => "bigint",
            ScalarType::Float => "double precision",
            ScalarType::Decimal => "numeric(65,30)",
            ScalarType::Boolean => "boolean",
            ScalarType::DateTime => "timestamp(3) without time zone",
            ScalarType::Json => "jsonb",
            ScalarType::Bytes => "bytea",
        }
        .to_owned();
    };
    let a = &n.args;
    match n.name.as_str() {
        "Text" => "text".to_owned(),
        "Char" => format!("character{}", precision(a)),
        "VarChar" => format!("character varying{}", precision(a)),
        "Bit" => format!("bit{}", precision(a)),
        "VarBit" => format!("bit varying{}", precision(a)),
        "Uuid" => "uuid".to_owned(),
        "Xml" => "xml".to_owned(),
        "Inet" => "inet".to_owned(),
        "Citext" => "citext".to_owned(),
        "Boolean" => "boolean".to_owned(),
        "Integer" => "integer".to_owned(),
        "SmallInt" => "smallint".to_owned(),
        "Oid" => "oid".to_owned(),
        "BigInt" => "bigint".to_owned(),
        "Real" => "real".to_owned(),
        "DoublePrecision" => "double precision".to_owned(),
        "Decimal" => match a.as_slice() {
            [p, s] => format!("numeric({p},{s})"),
            [p] => format!("numeric({p},0)"),
            _ => "numeric".to_owned(),
        },
        "Money" => "money".to_owned(),
        "Timestamp" => format!("timestamp{} without time zone", precision(a)),
        "Timestamptz" => format!("timestamp{} with time zone", precision(a)),
        "Date" => "date".to_owned(),
        "Time" => format!("time{} without time zone", precision(a)),
        "Timetz" => format!("time{} with time zone", precision(a)),
        "Json" => "json".to_owned(),
        "JsonB" => "jsonb".to_owned(),
        "ByteA" => "bytea".to_owned(),
        other => other.to_lowercase(),
    }
}

/// How an identifier looks in `format_type()` output: bare when it is a plain lower-case name,
/// quoted otherwise (`Role` → `"Role"`).
#[must_use]
pub fn type_name(name: &str) -> String {
    let plain = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name.chars().next().is_some_and(|c| !c.is_ascii_digit());
    if plain {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// `'text'` with quotes doubled.
#[must_use]
pub fn string_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A list default as a Postgres array literal, the way `array_out` prints it: `'{}'`,
/// `'{a,"b c"}'`, `'{1,-2}'`. The literal needs no cast, so it fits `text[]`, `integer[]` and enum
/// arrays alike, and Postgres keeps it as written, so the database reads back the same text.
#[must_use]
pub fn array_literal(items: &[String]) -> String {
    let elements: Vec<String> = items
        .iter()
        .map(|item| {
            let plain = !item.is_empty()
                && !item.eq_ignore_ascii_case("null")
                && !item.chars().any(|c| {
                    matches!(c, '{' | '}' | ',' | '"' | '\\')
                        || c.is_ascii_whitespace()
                        || c == '\x0b'
                });
            if plain {
                item.clone()
            } else {
                format!("\"{}\"", item.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect();
    string_literal(&format!("{{{}}}", elements.join(",")))
}

/// `ARRAY['a'::text, '-2'::integer, (1)::bigint, true]` or `ARRAY[]::text[]`, as `pg_get_expr`
/// prints a list default written as `ARRAY[..]` (Prisma writes them so), in the canonical
/// [`array_literal`] form. `None` for anything else.
fn array_constructor(expr: &str) -> Option<String> {
    let body = expr.strip_prefix("ARRAY[")?;
    let mut in_quote = false;
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    let mut end = None;
    for (i, c) in body.char_indices() {
        match c {
            '\'' => in_quote = !in_quote,
            _ if in_quote => {}
            '(' | '[' => depth += 1,
            ')' | ']' if depth > 0 => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&body[start..i]);
                start = i + 1;
            }
            ']' => {
                parts.push(&body[start..i]);
                end = Some(i);
                break;
            }
            _ => {}
        }
    }
    let rest = body[end? + 1..].trim();
    if !(rest.is_empty() || rest.starts_with("::")) {
        return None;
    }
    let mut items = Vec::new();
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let bare = strip_casts(part);
        let bare = bare
            .strip_prefix('(')
            .and_then(|b| b.strip_suffix(')'))
            .map_or(bare, strip_casts);
        let item = if let Some(inner) = bare.strip_prefix('\'').and_then(|b| b.strip_suffix('\'')) {
            inner.replace("''", "'")
        } else {
            match bare {
                "true" => "t".to_owned(),
                "false" => "f".to_owned(),
                n if n.parse::<f64>().is_ok() => n.to_owned(),
                _ => return None,
            }
        };
        items.push(item);
    }
    Some(array_literal(&items))
}

/// Remove trailing `::type` casts that are outside quotes: `'a'::text` → `'a'`.
fn strip_casts(expr: &str) -> &str {
    let mut out = expr.trim();
    loop {
        let mut in_quote = false;
        let mut cut = None;
        let bytes = out.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\'' => in_quote = !in_quote,
                b':' if !in_quote && bytes.get(i + 1) == Some(&b':') => {
                    cut = Some(i);
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        match cut {
            Some(i) => out = out[..i].trim(),
            None => return out,
        }
    }
}

/// `'-2'` in `'-2'::integer`: a quoted number cast to a numeric type, without its quotes.
fn quoted_number<'a>(expr: &str, bare: &'a str) -> Option<&'a str> {
    let inner = bare.strip_prefix('\'')?.strip_suffix('\'')?;
    inner.parse::<f64>().ok()?;
    let cast = expr
        .trim()
        .strip_prefix(bare)?
        .trim_start()
        .strip_prefix("::")?;
    [
        "integer",
        "bigint",
        "smallint",
        "numeric",
        "double precision",
        "real",
    ]
    .iter()
    .any(|t| cast.starts_with(t))
    .then_some(inner)
}

/// A default expression as `pg_get_expr` prints it, normalised.
#[must_use]
pub fn normalise_default(expr: &str) -> Default {
    let e = expr.trim();
    if e.starts_with("nextval(") {
        return Default::Sequence;
    }
    let lower = e.to_ascii_lowercase();
    if lower == "current_timestamp" || lower == "now()" || lower.starts_with("current_timestamp(") {
        return Default::Now;
    }
    if let Some(literal) = array_constructor(e) {
        return Default::Literal(literal);
    }
    let bare = strip_casts(e);
    let bare = bare
        .strip_prefix('(')
        .and_then(|b| b.strip_suffix(')'))
        .map_or(bare, strip_casts);
    // Postgres prints a negative number as a quoted string with a numeric cast: `'-2'::integer`
    if let Some(number) = quoted_number(e, bare) {
        return Default::Literal(number.to_owned());
    }
    let is_literal =
        bare.starts_with('\'') || bare == "true" || bare == "false" || bare.parse::<f64>().is_ok();
    if is_literal {
        Default::Literal(bare.to_owned())
    } else {
        Default::Expression(e.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::{array_literal, column_type, normalise_default, type_name};
    use crate::shape::Default;
    use lumen_schema::checked::{NativeType, ScalarType};

    #[test]
    fn types_match_format_type() {
        assert_eq!(
            column_type(ScalarType::DateTime, None),
            "timestamp(3) without time zone"
        );
        let varchar = NativeType {
            name: "VarChar".into(),
            args: vec![320],
        };
        assert_eq!(
            column_type(ScalarType::String, Some(&varchar)),
            "character varying(320)"
        );
        let dec = NativeType {
            name: "Decimal".into(),
            args: vec![10, 2],
        };
        assert_eq!(
            column_type(ScalarType::Decimal, Some(&dec)),
            "numeric(10,2)"
        );
        assert_eq!(type_name("Role"), "\"Role\"");
        assert_eq!(type_name("user_role"), "user_role");
    }

    #[test]
    fn defaults_are_normalised() {
        assert_eq!(
            normalise_default("nextval('\"User_id_seq\"'::regclass)"),
            Default::Sequence
        );
        assert_eq!(normalise_default("CURRENT_TIMESTAMP"), Default::Now);
        assert_eq!(
            normalise_default("'READER'::\"Role\""),
            Default::Literal("'READER'".into())
        );
        assert_eq!(
            normalise_default("'{}'::jsonb"),
            Default::Literal("'{}'".into())
        );
        assert_eq!(normalise_default("false"), Default::Literal("false".into()));
        assert_eq!(
            normalise_default("'0'::bigint"),
            Default::Literal("0".into())
        );
        assert_eq!(
            normalise_default("(0)::bigint"),
            Default::Literal("0".into())
        );
        assert_eq!(
            normalise_default("gen_random_uuid()"),
            Default::Expression("gen_random_uuid()".into())
        );
    }

    #[test]
    fn list_defaults_read_back_as_array_literals() {
        let lit = |s: &str| Default::Literal(s.to_owned());
        // as Postgres 13 to 17 print defaults written as `ARRAY[..]` (Prisma's form)
        for (printed, canonical) in [
            ("ARRAY[]::text[]", "'{}'"),
            ("ARRAY[]::\"Role\"[]", "'{}'"),
            ("ARRAY['x'::text, 'y it''s'::text]", "'{x,\"y it''s\"}'"),
            ("ARRAY[1, '-2'::integer]", "'{1,-2}'"),
            ("ARRAY['A'::\"Role\", 'B'::\"Role\"]", "'{A,B}'"),
            ("ARRAY[true]", "'{t}'"),
            ("ARRAY[(1)::bigint, (2)::bigint]", "'{1,2}'"),
            // and written as an array literal (lumen's form)
            ("'{}'::text[]", "'{}'"),
            ("'{x,\"y it''s\"}'::text[]", "'{x,\"y it''s\"}'"),
        ] {
            assert_eq!(normalise_default(printed), lit(canonical), "{printed}");
        }
        assert_eq!(
            normalise_default("ARRAY[lower('A'::text)]"),
            Default::Expression("ARRAY[lower('A'::text)]".into())
        );
    }

    #[test]
    fn array_literals_quote_like_postgres() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert_eq!(array_literal(&[]), "'{}'");
        assert_eq!(
            array_literal(&s(&["openid", "profile"])),
            "'{openid,profile}'"
        );
        assert_eq!(
            array_literal(&s(&[
                "a b",
                "",
                "NULL",
                "x,y",
                "q\"",
                "back\\slash",
                "it's"
            ])),
            "'{\"a b\",\"\",\"NULL\",\"x,y\",\"q\\\"\",\"back\\\\slash\",it''s}'"
        );
    }
}
