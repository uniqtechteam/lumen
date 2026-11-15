//! `@db.X(..)` native types: one table per provider with each type's arguments and the scalar
//! types it may be used on.

use super::{Provider, ScalarType};

/// A native database type, as written: `@db.VarChar(255)` is `VarChar` with `[255]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeType {
    /// The type name without the datasource prefix.
    pub name: String,
    /// Its numeric arguments.
    pub args: Vec<u32>,
}

/// One row of a provider's table.
struct Entry {
    name: &'static str,
    /// The smallest and largest number of arguments.
    args: (usize, usize),
    scalars: &'static [ScalarType],
}

macro_rules! table {
    ($($name:literal $min:literal..$max:literal => [$($scalar:ident),*]),* $(,)?) => {
        &[$(Entry { name: $name, args: ($min, $max), scalars: &[$(ScalarType::$scalar),*] }),*]
    };
}

const POSTGRES: &[Entry] = table![
    "Text" 0..0 => [String],
    "Char" 0..1 => [String],
    "VarChar" 0..1 => [String],
    "Bit" 0..1 => [String],
    "VarBit" 0..1 => [String],
    "Uuid" 0..0 => [String],
    "Xml" 0..0 => [String],
    "Inet" 0..0 => [String],
    "Citext" 0..0 => [String],
    "Boolean" 0..0 => [Boolean],
    "Integer" 0..0 => [Int],
    "SmallInt" 0..0 => [Int],
    "Oid" 0..0 => [Int],
    "BigInt" 0..0 => [BigInt],
    "Real" 0..0 => [Float],
    "DoublePrecision" 0..0 => [Float],
    "Decimal" 0..2 => [Decimal],
    "Money" 0..0 => [Decimal],
    "Timestamp" 0..1 => [DateTime],
    "Timestamptz" 0..1 => [DateTime],
    "Date" 0..0 => [DateTime],
    "Time" 0..1 => [DateTime],
    "Timetz" 0..1 => [DateTime],
    "Json" 0..0 => [Json],
    "JsonB" 0..0 => [Json],
    "ByteA" 0..0 => [Bytes],
];

const MONGODB: &[Entry] = table![
    "ObjectId" 0..0 => [String, Bytes],
    "String" 0..0 => [String],
    "Int" 0..0 => [Int],
    "Long" 0..0 => [Int, BigInt],
    "Double" 0..0 => [Float],
    "Bool" 0..0 => [Boolean],
    "Date" 0..0 => [DateTime],
    "Timestamp" 0..0 => [DateTime],
    "BinData" 0..0 => [Bytes],
    "Json" 0..0 => [Json],
];

fn table(provider: Provider) -> &'static [Entry] {
    match provider {
        Provider::Postgres => POSTGRES,
        Provider::MongoDb => MONGODB,
    }
}

/// Why a native type does not fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Misfit {
    /// No such type for this provider; the list of known names.
    Unknown(Vec<&'static str>),
    /// Wrong number of arguments: (min, max).
    Arguments(usize, usize),
    /// Not allowed on this scalar; the scalars it is allowed on.
    Scalar(Vec<&'static str>),
}

/// Check `@db.name(args)` on a field of type `scalar`.
pub(super) fn check(
    provider: Provider,
    name: &str,
    arg_count: usize,
    scalar: Option<ScalarType>,
) -> Result<(), Misfit> {
    let entries = table(provider);
    let Some(entry) = entries.iter().find(|e| e.name == name) else {
        return Err(Misfit::Unknown(entries.iter().map(|e| e.name).collect()));
    };
    if arg_count < entry.args.0 || arg_count > entry.args.1 {
        return Err(Misfit::Arguments(entry.args.0, entry.args.1));
    }
    if let Some(scalar) = scalar
        && !entry.scalars.contains(&scalar)
    {
        return Err(Misfit::Scalar(
            entry.scalars.iter().map(|s| s.name()).collect(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Misfit, check};
    use crate::checked::{Provider, ScalarType};

    #[test]
    fn postgres_types() {
        assert_eq!(
            check(Provider::Postgres, "VarChar", 1, Some(ScalarType::String)),
            Ok(())
        );
        assert_eq!(
            check(Provider::Postgres, "VarChar", 2, Some(ScalarType::String)),
            Err(Misfit::Arguments(0, 1))
        );
        assert_eq!(
            check(Provider::Postgres, "Uuid", 0, Some(ScalarType::Int)),
            Err(Misfit::Scalar(vec!["String"]))
        );
        assert!(matches!(
            check(Provider::Postgres, "Varchar", 0, None),
            Err(Misfit::Unknown(_))
        ));
        assert_eq!(
            check(Provider::MongoDb, "ObjectId", 0, Some(ScalarType::String)),
            Ok(())
        );
    }
}
