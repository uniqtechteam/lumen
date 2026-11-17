//! `db pull` for MongoDB: sample up to 1000 documents per collection and suggest a schema. A field missing or null in any sampled document is optional; a field with
//! several types gets the most common one and a comment; nested documents become composite types.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use mongodb::bson::{Bson, Document, doc};

use crate::MigrateError;

/// Documents sampled per collection.
pub const SAMPLE_SIZE: i64 = 1000;

/// How deep nested documents become composite types; deeper ones become `Json`.
const MAX_DEPTH: usize = 2;

#[derive(Debug, Default)]
struct FieldStats {
    /// Prisma type → how many documents had it.
    types: BTreeMap<String, usize>,
    /// Documents where the field is missing or null.
    missing: usize,
    /// For nested documents: their fields.
    nested: Shape,
}

#[derive(Debug, Default)]
struct Shape {
    fields: BTreeMap<String, FieldStats>,
    order: Vec<String>,
    documents: usize,
}

fn prisma_type(value: &Bson, depth: usize) -> String {
    match value {
        Bson::String(_) => "String".into(),
        Bson::Int32(_) => "Int".into(),
        Bson::Int64(_) => "BigInt".into(),
        Bson::Double(_) => "Float".into(),
        Bson::Decimal128(_) => "Decimal".into(),
        Bson::Boolean(_) => "Boolean".into(),
        Bson::DateTime(_) | Bson::Timestamp(_) => "DateTime".into(),
        Bson::ObjectId(_) => "ObjectId".into(),
        Bson::Binary(_) => "Bytes".into(),
        Bson::Document(_) if depth < MAX_DEPTH => "Document".into(),
        Bson::Array(items) => match items.first() {
            Some(first) => {
                let t = prisma_type(first, depth);
                if items.iter().all(|i| prisma_type(i, depth) == t) && t != "Document" {
                    format!("{t}[]")
                } else {
                    "Json".into()
                }
            }
            None => "Json".into(),
        },
        _ => "Json".into(),
    }
}

impl Shape {
    fn add(&mut self, doc: &Document, depth: usize) {
        self.documents += 1;
        for (key, value) in doc {
            if !self.fields.contains_key(key) {
                self.order.push(key.clone());
            }
            let stats = self.fields.entry(key.clone()).or_default();
            if matches!(value, Bson::Null) {
                stats.missing += 1;
                continue;
            }
            let t = prisma_type(value, depth);
            if let Bson::Document(inner) = value
                && t == "Document"
            {
                stats.nested.add(inner, depth + 1);
            }
            *stats.types.entry(t).or_default() += 1;
        }
    }

    fn missing_in(&self, stats: &FieldStats) -> bool {
        let seen: usize = stats.types.values().sum();
        stats.missing > 0 || seen < self.documents
    }
}

/// A valid field name for `key`, and whether it needs `@map`.
fn field_name(key: &str) -> (String, bool) {
    let mut name: String = key
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.starts_with(|c: char| c.is_ascii_digit()) || name.is_empty() {
        name.insert(0, 'f');
    }
    let changed = name != key;
    (name, changed)
}

fn pascal(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        })
        .collect()
}

/// The field lines of a model or type, and the composite types it needs.
fn render_fields(
    owner: &str,
    shape: &Shape,
    types: &mut Vec<String>,
    is_model: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    for key in &shape.order {
        let stats = &shape.fields[key];
        let Some((chosen, _)) = stats.types.iter().max_by_key(|(_, n)| **n) else {
            // only ever null: its type is unknown
            let (name, mapped) = field_name(key);
            let map = if mapped {
                format!(" @map(\"{key}\")")
            } else {
                String::new()
            };
            lines.push(format!(
                "// lumen: `{key}` was always null in the sample\n  {name} Json?{map}"
            ));
            continue;
        };
        if is_model && key == "_id" {
            let line = if chosen == "ObjectId" {
                "id String @id @default(auto()) @map(\"_id\") @db.ObjectId".to_owned()
            } else {
                format!(
                    "id {} @id @map(\"_id\")",
                    if chosen == "Document" { "Json" } else { chosen }
                )
            };
            lines.push(line);
            continue;
        }
        let (name, mapped) = field_name(key);
        let mut attrs = Vec::new();
        let ty = match chosen.as_str() {
            "ObjectId" => {
                attrs.push("@db.ObjectId".to_owned());
                "String".to_owned()
            }
            "ObjectId[]" => {
                attrs.push("@db.ObjectId".to_owned());
                "String[]".to_owned()
            }
            "Document" => {
                let type_name = format!("{owner}{}", pascal(key));
                let inner = render_fields(&type_name, &stats.nested, types, false);
                types.push(format!(
                    "type {type_name} {{\n{}\n}}",
                    inner
                        .iter()
                        .map(|l| format!("  {l}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
                type_name
            }
            other => other.to_owned(),
        };
        if mapped {
            attrs.push(format!("@map(\"{key}\")"));
        }
        let optional = !ty.ends_with("[]") && shape.missing_in(stats);
        let mut line = String::new();
        if stats.types.len() > 1 {
            let total: usize = stats.types.values().sum();
            let split: Vec<String> = stats
                .types
                .iter()
                .map(|(t, n)| format!("{t} {}%", n * 100 / total.max(1)))
                .collect();
            let _ = write!(
                line,
                "// lumen: `{key}` holds several types ({}); chose {chosen}\n  ",
                split.join(", ")
            );
        }
        let _ = write!(line, "{name} {ty}{}", if optional { "?" } else { "" });
        for a in attrs {
            let _ = write!(line, " {a}");
        }
        lines.push(line);
    }
    lines
}

async fn indexes(
    collection: &mongodb::Collection<Document>,
    field_names: &BTreeMap<String, String>,
) -> Result<(Vec<String>, Vec<String>), MigrateError> {
    let err = |e: mongodb::error::Error| MigrateError::new(format!("cannot read MongoDB: {e}"));
    let mut cursor = collection.list_indexes().await.map_err(err)?;
    let mut uniques = Vec::new();
    let mut blocks = Vec::new();
    while cursor.advance().await.map_err(err)? {
        let model = cursor.deserialize_current().map_err(err)?;
        let options = model.options.unwrap_or_default();
        if options.name.as_deref() == Some("_id_") {
            continue;
        }
        let keys: Vec<String> = model
            .keys
            .iter()
            .map(|(k, v)| {
                let name = field_names
                    .get(k)
                    .cloned()
                    .unwrap_or_else(|| field_name(k).0);
                if v.as_i32() == Some(-1) {
                    format!("{name}(sort: Desc)")
                } else {
                    name
                }
            })
            .collect();
        let unique = options.unique.unwrap_or(false);
        match (unique, keys.as_slice()) {
            (true, [one]) if !one.contains('(') => uniques.push(one.clone()),
            (true, _) => blocks.push(format!("@@unique([{}])", keys.join(", "))),
            (false, _) => blocks.push(format!("@@index([{}])", keys.join(", "))),
        }
    }
    Ok((uniques, blocks))
}

/// A suggested schema file for the database, formatted.
///
/// # Errors
/// When the server cannot be read.
pub async fn pull(db: &mongodb::Database) -> Result<String, MigrateError> {
    let err = |e: mongodb::error::Error| MigrateError::new(format!("cannot read MongoDB: {e}"));
    let mut names = db.list_collection_names().await.map_err(err)?;
    names.retain(|n| !n.starts_with("system."));
    names.sort();
    let mut out = String::from(
        "datasource db {\n  provider = \"mongodb\"\n  url      = env(\"DATABASE_URL\")\n}\n",
    );
    let mut types = Vec::new();
    for name in names {
        let collection = db.collection::<Document>(&name);
        let mut cursor = collection
            .aggregate(vec![doc! { "$sample": { "size": SAMPLE_SIZE } }])
            .await
            .map_err(err)?;
        let mut shape = Shape::default();
        while cursor.advance().await.map_err(err)? {
            let d = cursor.deserialize_current().map_err(err)?;
            shape.add(&d, 0);
        }
        let model = pascal(&name);
        let mut lines = render_fields(&model, &shape, &mut types, true);
        let field_names: BTreeMap<String, String> = shape
            .order
            .iter()
            .map(|k| {
                (
                    k.clone(),
                    if k == "_id" {
                        "id".to_owned()
                    } else {
                        field_name(k).0
                    },
                )
            })
            .collect();
        let (uniques, blocks) = indexes(&collection, &field_names).await?;
        for line in &mut lines {
            let name = line
                .rsplit("\n  ")
                .next()
                .unwrap_or(line)
                .split(' ')
                .next()
                .unwrap_or("")
                .to_owned();
            if uniques.contains(&name) {
                line.push_str(" @unique");
            }
        }
        let _ = write!(out, "\nmodel {model} {{\n");
        for l in lines {
            let _ = writeln!(out, "  {l}");
        }
        if !blocks.is_empty() {
            out.push('\n');
            for b in blocks {
                let _ = writeln!(out, "  {b}");
            }
        }
        if model != name {
            let _ = writeln!(out, "\n  @@map(\"{name}\")");
        }
        out.push_str("}\n");
    }
    for t in types {
        let _ = write!(out, "\n{t}\n");
    }
    let (formatted, _) = lumen_schema::format(&lumen_schema::SourceFile::new("schema.prisma", out));
    Ok(formatted)
}
