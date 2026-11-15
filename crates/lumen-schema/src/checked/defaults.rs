//! `@default(..)`: read the value and check that it fits the field's type and the database.

use super::{DefaultValue, Enum, FieldKind, Literal, Provider, ScalarType};
use crate::ast::{Arity, Expr};

/// Why a default does not fit.
pub(super) type Misfit = String;

fn scalar(kind: &FieldKind) -> Option<ScalarType> {
    match kind {
        FieldKind::Scalar(s) => Some(*s),
        _ => None,
    }
}

fn literal(expr: &Expr, kind: &FieldKind, enums: &[Enum]) -> Result<Literal, Misfit> {
    let fits = |ok: bool, what: &str| {
        if ok {
            Ok(())
        } else {
            Err(format!("{what} does not fit this field's type"))
        }
    };
    match expr {
        Expr::String(s, _) => {
            fits(
                matches!(
                    scalar(kind),
                    Some(
                        ScalarType::String
                            | ScalarType::Json
                            | ScalarType::DateTime
                            | ScalarType::Bytes
                            | ScalarType::Decimal
                    )
                ),
                "a string",
            )?;
            Ok(Literal::String(s.clone()))
        }
        Expr::Number(n, _) => {
            let whole = !n.contains('.');
            fits(
                match scalar(kind) {
                    Some(ScalarType::Int | ScalarType::BigInt) => whole,
                    Some(ScalarType::Float | ScalarType::Decimal) => true,
                    _ => false,
                },
                "this number",
            )?;
            Ok(Literal::Number(n.clone()))
        }
        Expr::Constant(c, _) if c == "true" || c == "false" => {
            fits(scalar(kind) == Some(ScalarType::Boolean), "a boolean")?;
            Ok(Literal::Boolean(c == "true"))
        }
        Expr::Constant(c, _) => match kind {
            FieldKind::Enum(id) => {
                let e = &enums[id.0];
                if e.values.iter().any(|v| &v.name == c) {
                    Ok(Literal::EnumValue(c.clone()))
                } else {
                    let names: Vec<&str> = e.values.iter().map(|v| v.name.as_str()).collect();
                    Err(format!(
                        "`{c}` is not a value of the enum `{}` ({})",
                        e.name,
                        names.join(", ")
                    ))
                }
            }
            _ => Err(format!("`{c}` is not a value this field can hold")),
        },
        Expr::Array(items, _) => {
            let mut out = Vec::new();
            for item in items {
                out.push(literal(item, kind, enums)?);
            }
            Ok(Literal::List(out))
        }
        Expr::Function(..) | Expr::Object(..) => {
            Err("functions cannot be used inside a list default".to_owned())
        }
    }
}

fn version(args: &[crate::ast::Argument], allowed: &[u8], default: u8) -> Result<u8, Misfit> {
    match args.first().map(|a| &a.value) {
        None => Ok(default),
        Some(Expr::Number(n, _)) => n
            .parse::<u8>()
            .ok()
            .filter(|v| allowed.contains(v))
            .ok_or_else(|| format!("version {n} is not supported")),
        Some(_) => Err("the version is a number".to_owned()),
    }
}

/// Read `@default(expr)` for a field of `kind` and `arity`.
pub(super) fn read(
    expr: &Expr,
    kind: &FieldKind,
    arity: Arity,
    provider: Provider,
    enums: &[Enum],
) -> Result<DefaultValue, Misfit> {
    let Expr::Function(name, args, _) = expr else {
        if arity == Arity::List && !matches!(expr, Expr::Array(..)) {
            return Err("a list field's default is a list, for example `[]`".to_owned());
        }
        return literal(expr, kind, enums).map(DefaultValue::Literal);
    };
    let s = scalar(kind);
    let need = |ok: bool, msg: &str| if ok { Ok(()) } else { Err(msg.to_owned()) };
    Ok(match name.name.as_str() {
        "autoincrement" => {
            need(
                matches!(s, Some(ScalarType::Int | ScalarType::BigInt)),
                "`autoincrement()` needs an `Int` or `BigInt` field",
            )?;
            need(
                provider == Provider::Postgres,
                "`autoincrement()` is not available on MongoDB; use `@default(auto())` on an ObjectId",
            )?;
            DefaultValue::Autoincrement
        }
        "now" => {
            need(
                s == Some(ScalarType::DateTime),
                "`now()` needs a `DateTime` field",
            )?;
            DefaultValue::Now
        }
        "uuid" => {
            need(
                s == Some(ScalarType::String),
                "`uuid()` needs a `String` field",
            )?;
            DefaultValue::Uuid(version(args, &[4, 7], 4)?)
        }
        "cuid" => {
            need(
                s == Some(ScalarType::String),
                "`cuid()` needs a `String` field",
            )?;
            DefaultValue::Cuid(version(args, &[1, 2], 1)?)
        }
        "nanoid" => {
            need(
                s == Some(ScalarType::String),
                "`nanoid()` needs a `String` field",
            )?;
            let length = match args.first().map(|a| &a.value) {
                Some(Expr::Number(n, _)) => Some(
                    n.parse::<u32>()
                        .map_err(|_| format!("`{n}` is not a length"))?,
                ),
                _ => None,
            };
            DefaultValue::Nanoid(length)
        }
        "ulid" => {
            need(
                s == Some(ScalarType::String),
                "`ulid()` needs a `String` field",
            )?;
            DefaultValue::Ulid
        }
        "auto" => {
            need(
                provider == Provider::MongoDb,
                "`auto()` is for MongoDB ObjectIds; on Postgres use `autoincrement()` or `dbgenerated(\"..\")`",
            )?;
            DefaultValue::Auto
        }
        "dbgenerated" => {
            need(
                provider == Provider::Postgres,
                "`dbgenerated()` is not available on MongoDB",
            )?;
            let sql = match args.first().map(|a| &a.value) {
                Some(Expr::String(sql, _)) => Some(sql.clone()),
                None => None,
                Some(_) => return Err("`dbgenerated` takes the SQL as a string".to_owned()),
            };
            DefaultValue::DbGenerated(sql)
        }
        other => return Err(format!("`{other}()` is not a known default function")),
    })
}
