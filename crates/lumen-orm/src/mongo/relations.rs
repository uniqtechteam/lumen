//! Relation filters on MongoDB (`some`, `every`, `none`, `is`, `isNot`), without `$lookup`. Before a
//! query runs, each relation filter becomes a filter on the document's own key: one aggregation
//! collects the link values of the related documents that match (for `every`, that do not match),
//! and the outer query keeps the documents whose key is `$in` (or not in) that list. Nested
//! relation filters are resolved from the inside out, so a filter costs one extra statement per
//! relation in it, never one per document.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

use mongodb::ClientSession;
use mongodb::bson::{Bson, Document, doc};

use super::{mongo_err, query_of};
use crate::error::{Error, Result};
use crate::filter::{Filter, Link, Op, Quantifier, Related};
use crate::value::Value;

/// Where the extra statements run: on the database, or inside a transaction's session.
pub(super) struct Exec<'a> {
    pub(super) db: &'a mongodb::Database,
    pub(super) session: Option<&'a mut ClientSession>,
    pub(super) statements: &'a AtomicU64,
}

fn has_relation(filter: &Filter) -> bool {
    match filter {
        Filter::And(items) | Filter::Or(items) => items.iter().any(has_relation),
        Filter::Not(inner) => has_relation(inner),
        Filter::Relation(..) => true,
        Filter::Column(..) | Filter::Tuples(..) => false,
    }
}

/// `filter` with every relation filter replaced by a key filter (unchanged when it has none).
pub(super) async fn resolve_opt(
    ex: &mut Exec<'_>,
    filter: Option<&Filter>,
) -> Result<Option<Filter>> {
    match filter {
        Some(f) if has_relation(f) => Ok(Some(resolve(ex, f).await?)),
        other => Ok(other.cloned()),
    }
}

type Resolving<'a> = Pin<Box<dyn Future<Output = Result<Filter>> + Send + 'a>>;

fn resolve<'a, 'b: 'a>(ex: &'a mut Exec<'b>, filter: &'a Filter) -> Resolving<'a> {
    Box::pin(async move {
        Ok(match filter {
            Filter::And(items) | Filter::Or(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(resolve(&mut *ex, item).await?);
                }
                if matches!(filter, Filter::And(_)) {
                    Filter::And(out)
                } else {
                    Filter::Or(out)
                }
            }
            Filter::Not(inner) => !resolve(ex, inner).await?,
            Filter::Relation(quantifier, related) => relation(ex, *quantifier, related).await?,
            other => other.clone(),
        })
    })
}

async fn relation(ex: &mut Exec<'_>, quantifier: Quantifier, related: &Related) -> Result<Filter> {
    let Link::Direct(on) = &related.link else {
        return Err(Error::new(
            "P2026",
            "many-to-many relation filters are not supported on MongoDB",
        ));
    };
    let inner = resolve(&mut *ex, &related.filter).await?;
    // `every` keeps the documents that have no related document failing the condition
    let inner = if quantifier == Quantifier::Every {
        !inner
    } else {
        inner
    };
    let mut key = Document::new();
    for (i, (column, _)) in on.iter().enumerate() {
        key.insert(format!("k{i}"), format!("${column}"));
    }
    let pipeline = vec![
        doc! { "$match": query_of(&inner)? },
        doc! { "$group": { "_id": key } },
    ];
    let mut rows = Vec::new();
    for d in aggregate(ex, &related.table.name, pipeline).await? {
        let Ok(key) = d.get_document("_id") else {
            continue;
        };
        // a related document without a link value is linked to nothing
        let row: Option<Vec<Value>> = (0..on.len())
            .map(|i| key.get(format!("k{i}")).filter(|b| **b != Bson::Null))
            .map(|b| b.map(key_value).transpose())
            .collect::<Result<Option<Vec<Value>>>>()?;
        rows.extend(row);
    }
    let columns: Vec<String> = on.iter().map(|(_, this)| this.clone()).collect();
    let linked = if let [column] = columns.as_slice() {
        let values = rows.into_iter().filter_map(|r| r.into_iter().next());
        Filter::Column(column.clone(), Op::In(values.collect()))
    } else {
        Filter::Tuples(columns, rows)
    };
    Ok(match quantifier {
        Quantifier::Some | Quantifier::Is => linked,
        Quantifier::Every | Quantifier::None | Quantifier::IsNot => !linked,
    })
}

/// A link value read back from MongoDB, to match the outer documents with.
fn key_value(b: &Bson) -> Result<Value> {
    Ok(match b {
        Bson::ObjectId(id) => Value::ObjectId(id.to_hex()),
        Bson::String(s) => Value::Text(s.clone()),
        Bson::Int32(i) => Value::Int(*i),
        Bson::Int64(i) => Value::BigInt(*i),
        Bson::Double(f) => Value::Float(*f),
        Bson::Boolean(v) => Value::Bool(*v),
        Bson::Decimal128(d) => Value::Decimal(d.to_string()),
        Bson::DateTime(t) => Value::from(*t),
        Bson::Binary(b) => Value::Bytes(b.bytes.clone()),
        other => {
            return Err(Error::new(
                "P2026",
                format!(
                    "a relation filter cannot link on a {:?} value",
                    other.element_type()
                ),
            ));
        }
    })
}

async fn aggregate(
    ex: &mut Exec<'_>,
    collection: &str,
    pipeline: Vec<Document>,
) -> Result<Vec<Document>> {
    ex.statements.fetch_add(1, Ordering::Relaxed);
    let coll = ex.db.collection::<Document>(collection);
    let decode = |e: mongodb::error::Error| Error::new("P2023", e.to_string());
    let mut out = Vec::new();
    if let Some(session) = ex.session.as_deref_mut() {
        let mut cursor = coll
            .aggregate(pipeline)
            .session(&mut *session)
            .await
            .map_err(|e| mongo_err(&e))?;
        while cursor
            .advance(&mut *session)
            .await
            .map_err(|e| mongo_err(&e))?
        {
            out.push(cursor.deserialize_current().map_err(decode)?);
        }
    } else {
        let mut cursor = coll.aggregate(pipeline).await.map_err(|e| mongo_err(&e))?;
        while cursor.advance().await.map_err(|e| mongo_err(&e))? {
            out.push(cursor.deserialize_current().map_err(decode)?);
        }
    }
    Ok(out)
}
