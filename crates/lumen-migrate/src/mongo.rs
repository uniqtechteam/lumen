//! MongoDB: `db push` instead of migration files, as in Prisma. Only collections and indexes are
//! managed. Collections are created and never dropped. Indexes the schema has are created, and
//! dropped and created again when their definition in the schema changes (the schema names them,
//! so they are lumen's). An index the schema does not have is never dropped: it may be one a
//! person made by hand, and MongoDB keeps nothing that tells lumen's apart from it, so `db push`
//! reports it ([`MongoPush::not_in_schema`]) and leaves it for a person to drop.

use lumen_schema::checked::{IndexKind, ModelKind, Schema, SortOrder};
use mongodb::IndexModel;
use mongodb::bson::Document;
use mongodb::options::IndexOptions;

use crate::MigrateError;

/// One index, as both sides see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MongoIndex {
    /// The name.
    pub name: String,
    /// Field paths and directions (1 or -1), in order.
    pub keys: Vec<(String, i32)>,
    /// Whether it is unique.
    pub unique: bool,
}

/// One collection and its indexes (without the built-in `_id_` index).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MongoCollection {
    /// The name.
    pub name: String,
    /// The indexes, by name.
    pub indexes: Vec<MongoIndex>,
}

/// What `db push` does.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum MongoStep {
    /// Create a collection.
    CreateCollection(String),
    /// Drop an index (collection, index) the schema has with a different definition, to create it
    /// again with the schema's.
    DropIndex(String, String),
    /// Create an index.
    CreateIndex(String, String),
}

/// An index in the database that the schema does not have, on a collection the schema has.
/// `db push` leaves it as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraIndex {
    /// The collection.
    pub collection: String,
    /// The index.
    pub index: MongoIndex,
}

/// What `db push` did, and what it left alone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MongoPush {
    /// The steps it ran, in order.
    pub steps: Vec<MongoStep>,
    /// Indexes the schema does not have; reported, never dropped.
    pub not_in_schema: Vec<ExtraIndex>,
}

fn cut(name: String) -> String {
    if name.len() <= 127 {
        return name;
    }
    let mut end = 127;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

/// The collections and indexes `schema` asks for, with Prisma's index names.
#[must_use]
pub fn calculate(schema: &Schema) -> Vec<MongoCollection> {
    let mut out: Vec<MongoCollection> = schema
        .models
        .iter()
        .filter(|m| m.kind == ModelKind::Model && !m.ignored)
        .map(|m| {
            let collection = m.database_name().to_owned();
            let mut indexes: Vec<MongoIndex> = m
                .indexes
                .iter()
                .filter(|i| i.kind != IndexKind::PrimaryKey)
                .filter_map(|i| {
                    let keys: Vec<(String, i32)> = i
                        .fields
                        .iter()
                        .map(|f| {
                            let dir = if f.sort == Some(SortOrder::Desc) {
                                -1
                            } else {
                                1
                            };
                            (m.fields[f.field].database_name().to_owned(), dir)
                        })
                        .collect();
                    if keys.iter().any(|(k, _)| k == "_id") && keys.len() == 1 {
                        return None;
                    }
                    let unique = i.kind == IndexKind::Unique;
                    let joined: Vec<&str> = keys.iter().map(|(k, _)| k.as_str()).collect();
                    let suffix = if unique { "key" } else { "idx" };
                    Some(MongoIndex {
                        name: i.mapped_name.clone().unwrap_or_else(|| {
                            cut(format!("{collection}_{}_{suffix}", joined.join("_")))
                        }),
                        keys,
                        unique,
                    })
                })
                .collect();
            indexes.sort_by(|a, b| a.name.cmp(&b.name));
            MongoCollection {
                name: collection,
                indexes,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The collections and indexes in `db`.
///
/// # Errors
/// When the server cannot be read.
pub async fn describe(db: &mongodb::Database) -> Result<Vec<MongoCollection>, MigrateError> {
    let err = |e: mongodb::error::Error| MigrateError::new(format!("cannot read MongoDB: {e}"));
    let mut names = db.list_collection_names().await.map_err(err)?;
    names.retain(|n| !n.starts_with("system."));
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let mut cursor = db
            .collection::<Document>(&name)
            .list_indexes()
            .await
            .map_err(err)?;
        let mut indexes = Vec::new();
        while cursor.advance().await.map_err(err)? {
            let model = cursor.deserialize_current().map_err(err)?;
            let options = model.options.unwrap_or_default();
            let index_name = options.name.unwrap_or_default();
            if index_name == "_id_" {
                continue;
            }
            let keys = model
                .keys
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_i32()
                            .or_else(|| v.as_i64().map(|n| if n < 0 { -1 } else { 1 }))
                            .or_else(|| v.as_f64().map(|f| if f < 0.0 { -1 } else { 1 }))
                            .unwrap_or(1),
                    )
                })
                .collect();
            indexes.push(MongoIndex {
                name: index_name,
                keys,
                unique: options.unique.unwrap_or(false),
            });
        }
        indexes.sort_by(|a, b| a.name.cmp(&b.name));
        out.push(MongoCollection { name, indexes });
    }
    Ok(out)
}

/// The steps from `live` to `target`. Indexes `target` does not have get no step (see
/// [`not_in_schema`]).
#[must_use]
pub fn diff(live: &[MongoCollection], target: &[MongoCollection]) -> Vec<MongoStep> {
    let mut steps = Vec::new();
    for t in target {
        let existing = live.iter().find(|l| l.name == t.name);
        if existing.is_none() {
            steps.push(MongoStep::CreateCollection(t.name.clone()));
        }
        let have = existing.map_or(&[][..], |l| l.indexes.as_slice());
        for i in &t.indexes {
            match have.iter().find(|h| h.name == i.name) {
                Some(h) if h == i => {}
                Some(_) => {
                    steps.push(MongoStep::DropIndex(t.name.clone(), i.name.clone()));
                    steps.push(MongoStep::CreateIndex(t.name.clone(), i.name.clone()));
                }
                None => steps.push(MongoStep::CreateIndex(t.name.clone(), i.name.clone())),
            }
        }
    }
    steps.sort();
    steps
}

/// The indexes in `live` that `target` does not have, on the collections `target` has.
#[must_use]
pub fn not_in_schema(live: &[MongoCollection], target: &[MongoCollection]) -> Vec<ExtraIndex> {
    let mut out = Vec::new();
    for t in target {
        let Some(l) = live.iter().find(|l| l.name == t.name) else {
            continue;
        };
        for h in &l.indexes {
            if !t.indexes.iter().any(|i| i.name == h.name) {
                out.push(ExtraIndex {
                    collection: t.name.clone(),
                    index: h.clone(),
                });
            }
        }
    }
    out
}

/// MongoDB refuses a second index over the same keys, so an index the schema asks for that
/// a kept index already covers is a clear error before anything runs, not a refusal halfway.
fn check_clashes(
    steps: &[MongoStep],
    target: &[MongoCollection],
    extra: &[ExtraIndex],
) -> Result<(), MigrateError> {
    for step in steps {
        let MongoStep::CreateIndex(c, name) = step else {
            continue;
        };
        let Some(wanted) = target
            .iter()
            .find(|t| &t.name == c)
            .and_then(|t| t.indexes.iter().find(|i| &i.name == name))
        else {
            continue;
        };
        if let Some(e) = extra
            .iter()
            .find(|e| &e.collection == c && e.index.keys == wanted.keys)
        {
            let other = &e.index.name;
            return Err(MigrateError::new(format!(
                "cannot create index {name} on {c}: the index {other}, which the schema does not \
                 have, is on the same keys, and lumen does not drop indexes it was not asked for. \
                 Drop it by hand (in mongosh: db.{c}.dropIndex(\"{other}\")) or keep it by giving \
                 the schema's index map: \"{other}\"."
            )));
        }
    }
    Ok(())
}

/// `db push`: create the collections and indexes `schema` has in `db`, and rebuild its indexes
/// whose definition changed. Returns what was done and the indexes the schema does not have,
/// which are left as they are.
///
/// # Errors
/// When the server refuses a step (for example a unique index over duplicate values), or an index
/// to create has the same keys as one the schema does not have (nothing is run then).
pub async fn push(db: &mongodb::Database, schema: &Schema) -> Result<MongoPush, MigrateError> {
    let target = calculate(schema);
    let live = describe(db).await?;
    let steps = diff(&live, &target);
    let extra = not_in_schema(&live, &target);
    check_clashes(&steps, &target, &extra)?;
    let err = |e: mongodb::error::Error| MigrateError::new(format!("MongoDB refused a step: {e}"));
    for step in &steps {
        match step {
            MongoStep::CreateCollection(c) => db.create_collection(c).await.map_err(err)?,
            MongoStep::DropIndex(c, name) => db
                .collection::<Document>(c)
                .drop_index(name)
                .await
                .map_err(err)?,
            MongoStep::CreateIndex(c, name) => {
                let Some(index) = target
                    .iter()
                    .find(|t| &t.name == c)
                    .and_then(|t| t.indexes.iter().find(|i| &i.name == name))
                else {
                    continue;
                };
                let mut keys = Document::new();
                for (k, dir) in &index.keys {
                    keys.insert(k.clone(), *dir);
                }
                let options = IndexOptions::builder()
                    .name(index.name.clone())
                    .unique(index.unique)
                    .build();
                db.collection::<Document>(c)
                    .create_index(IndexModel::builder().keys(keys).options(options).build())
                    .await
                    .map_err(err)?;
            }
        }
    }
    Ok(MongoPush {
        steps,
        not_in_schema: extra,
    })
}
