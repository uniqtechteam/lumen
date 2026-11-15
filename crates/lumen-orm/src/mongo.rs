//! MongoDB: the same filters, ordering and paging as on Postgres, rendered as BSON queries.
//! `not` keeps documents where the field is null or missing, as in Prisma.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use mongodb::bson::{self, Bson, Document, doc};
use mongodb::options::FindOptions;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};
use crate::filter::{Filter, Mode, Op};
use crate::query::Order;
use crate::value::Value;

/// A `Value` as BSON. Text in an `ObjectId` field is sent as an ObjectId.
///
/// # Errors
/// P2023 when a value cannot be sent (for example an invalid ObjectId).
pub fn bson_of(value: &Value) -> Result<Bson> {
    Ok(match value {
        Value::Null => Bson::Null,
        Value::Bool(b) => Bson::Boolean(*b),
        Value::Int(i) => Bson::Int32(*i),
        Value::BigInt(i) => Bson::Int64(*i),
        Value::Float(f) => Bson::Double(*f),
        Value::Text(s) | Value::Uuid(s) | Value::Enum(s, _) => Bson::String(s.clone()),
        Value::Decimal(s) => Bson::Decimal128(
            s.parse()
                .map_err(|_| Error::new("P2023", format!("`{s}` is not a decimal")))?,
        ),
        Value::Bytes(b) => Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Generic,
            bytes: b.clone(),
        }),
        Value::Json(j) => bson::to_bson(j).map_err(|e| Error::new("P2023", e.to_string()))?,
        Value::DateTime(t) => Bson::DateTime(bson::DateTime::from_millis(t.timestamp_millis())),
        Value::Timestamp(t) => {
            Bson::DateTime(bson::DateTime::from_millis(t.and_utc().timestamp_millis()))
        }
        Value::ObjectId(s) => Bson::ObjectId(
            bson::oid::ObjectId::parse_str(s)
                .map_err(|_| Error::new("P2023", format!("`{s}` is not an ObjectId")))?,
        ),
        Value::List(items) => Bson::Array(items.iter().map(bson_of).collect::<Result<_>>()?),
    })
}

fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.^$|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn regex(pattern: String, mode: Mode) -> Bson {
    let options = if mode == Mode::Insensitive { "i" } else { "" };
    Bson::RegularExpression(bson::Regex {
        pattern,
        options: options.to_owned(),
    })
}

fn op_doc(op: &Op) -> Result<Bson> {
    Ok(match op {
        Op::Equals(v, Mode::Insensitive) => match v {
            Value::Text(s) => regex(format!("^{}$", regex_escape(s)), Mode::Insensitive),
            other => bson_of(other)?,
        },
        Op::Equals(v, Mode::Default) => bson_of(v)?,
        Op::NotEquals(v, _) => Bson::Document(doc! { "$ne": bson_of(v)? }),
        Op::In(vs) | Op::Any(vs) => {
            Bson::Document(doc! { "$in": vs.iter().map(bson_of).collect::<Result<Vec<_>>>()? })
        }
        Op::NotIn(vs) => {
            Bson::Document(doc! { "$nin": vs.iter().map(bson_of).collect::<Result<Vec<_>>>()? })
        }
        Op::Lt(v) => Bson::Document(doc! { "$lt": bson_of(v)? }),
        Op::Lte(v) => Bson::Document(doc! { "$lte": bson_of(v)? }),
        Op::Gt(v) => Bson::Document(doc! { "$gt": bson_of(v)? }),
        Op::Gte(v) => Bson::Document(doc! { "$gte": bson_of(v)? }),
        Op::Contains(t, m) => regex(regex_escape(t), *m),
        Op::StartsWith(t, m) => regex(format!("^{}", regex_escape(t)), *m),
        Op::EndsWith(t, m) => regex(format!("{}$", regex_escape(t)), *m),
        Op::IsNull(true) => Bson::Null,
        Op::IsNull(false) => Bson::Document(doc! { "$ne": Bson::Null }),
    })
}

/// A filter as a MongoDB query document.
///
/// # Errors
/// P2023 when a value cannot be sent.
pub fn query_of(filter: &Filter) -> Result<Document> {
    Ok(match filter {
        Filter::And(items) if items.is_empty() => doc! {},
        // matches nothing
        Filter::Or(items) if items.is_empty() => {
            doc! { "_id": { "$exists": false, "$type": "null" } }
        }
        Filter::And(items) => {
            doc! { "$and": items.iter().map(query_of).collect::<Result<Vec<_>>>()? }
        }
        Filter::Or(items) => {
            doc! { "$or": items.iter().map(query_of).collect::<Result<Vec<_>>>()? }
        }
        Filter::Not(inner) => doc! { "$nor": [query_of(inner)?] },
        Filter::Column(field, op) => {
            let mut d = Document::new();
            d.insert(field.clone(), op_doc(op)?);
            d
        }
        Filter::Tuples(fields, rows) => {
            let alternatives = rows
                .iter()
                .map(|row| {
                    let mut d = Document::new();
                    for (f, v) in fields.iter().zip(row) {
                        d.insert(f.clone(), bson_of(v)?);
                    }
                    Ok(Bson::Document(d))
                })
                .collect::<Result<Vec<_>>>()?;
            if alternatives.is_empty() {
                doc! { "_id": { "$exists": false, "$type": "null" } }
            } else {
                doc! { "$or": alternatives }
            }
        }
    })
}

/// A find: filter, ordering, paging and the fields to return.
#[derive(Debug, Clone, Default)]
pub struct Find {
    /// The collection.
    pub collection: String,
    /// Which documents.
    pub filter: Option<Filter>,
    /// Sort order.
    pub order: Vec<(String, Order)>,
    /// At most this many.
    pub limit: Option<i64>,
    /// Skip this many.
    pub skip: Option<u64>,
    /// Only these fields (empty: all).
    pub fields: Vec<String>,
}

fn mongo_err(e: &mongodb::error::Error) -> Error {
    let message = e.to_string();
    if let mongodb::error::ErrorKind::Write(mongodb::error::WriteFailure::WriteError(w)) =
        e.kind.as_ref()
        && w.code == 11000
    {
        return Error::new(
            "P2002",
            format!(
                "a record with the same unique value already exists ({})",
                w.message
            ),
        );
    }
    if let mongodb::error::ErrorKind::InsertMany(m) = e.kind.as_ref()
        && m.write_errors
            .as_ref()
            .is_some_and(|ws| ws.iter().any(|w| w.code == 11000))
    {
        return Error::new(
            "P2002",
            "a record with the same unique value already exists",
        );
    }
    Error::new("P2010", message)
}

/// A MongoDB database, with a statement counter like [`crate::Db`].
#[derive(Debug, Clone)]
pub struct MongoDb {
    db: mongodb::Database,
    statements: Arc<AtomicU64>,
}

impl MongoDb {
    /// Connect to `url` and use `database`.
    ///
    /// # Errors
    /// P1001 when the server cannot be reached.
    pub async fn connect(url: &str, database: &str) -> Result<Self> {
        let client = mongodb::Client::with_uri_str(url)
            .await
            .map_err(|e| Error::new("P1001", format!("cannot reach MongoDB: {e}")))?;
        Ok(Self::new(client.database(database)))
    }

    /// Use a database handle.
    #[must_use]
    pub fn new(db: mongodb::Database) -> Self {
        Self {
            db,
            statements: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The driver's database, for anything lumen does not cover.
    #[must_use]
    pub fn database(&self) -> &mongodb::Database {
        &self.db
    }

    /// How many operations ran.
    #[must_use]
    pub fn statements(&self) -> u64 {
        self.statements.load(Ordering::Relaxed)
    }

    fn tick(&self) {
        self.statements.fetch_add(1, Ordering::Relaxed);
    }

    /// The matching documents, decoded as `T`.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn find<T: DeserializeOwned + Send + Sync>(&self, find: &Find) -> Result<Vec<T>> {
        self.tick();
        let filter = find
            .filter
            .as_ref()
            .map(query_of)
            .transpose()?
            .unwrap_or_default();
        let mut options = FindOptions::default();
        if !find.order.is_empty() {
            let mut sort = Document::new();
            for (f, o) in &find.order {
                sort.insert(f.clone(), if *o == Order::Desc { -1 } else { 1 });
            }
            options.sort = Some(sort);
        }
        options.limit = find.limit;
        options.skip = find.skip;
        if !find.fields.is_empty() {
            let mut projection = Document::new();
            for f in &find.fields {
                projection.insert(f.clone(), 1);
            }
            options.projection = Some(projection);
        }
        let mut cursor = self
            .db
            .collection::<T>(&find.collection)
            .find(filter)
            .with_options(options)
            .await
            .map_err(|e| mongo_err(&e))?;
        let mut out = Vec::new();
        while cursor.advance().await.map_err(|e| mongo_err(&e))? {
            out.push(
                cursor
                    .deserialize_current()
                    .map_err(|e| Error::new("P2023", e.to_string()))?,
            );
        }
        Ok(out)
    }

    /// How many documents match.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn count(&self, collection: &str, filter: Option<&Filter>) -> Result<u64> {
        self.tick();
        let filter = filter.map(query_of).transpose()?.unwrap_or_default();
        self.db
            .collection::<Document>(collection)
            .count_documents(filter)
            .await
            .map_err(|e| mongo_err(&e))
    }

    /// Insert documents; their ids.
    ///
    /// # Errors
    /// P2002 on a unique index; a coded [`Error`] otherwise.
    pub async fn insert_many<T: Serialize + Send + Sync>(
        &self,
        collection: &str,
        docs: &[T],
    ) -> Result<Vec<Bson>> {
        self.tick();
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let result = self
            .db
            .collection::<T>(collection)
            .insert_many(docs)
            .await
            .map_err(|e| mongo_err(&e))?;
        let mut ids: Vec<(usize, Bson)> = result.inserted_ids.into_iter().collect();
        ids.sort_by_key(|(i, _)| *i);
        Ok(ids.into_iter().map(|(_, id)| id).collect())
    }

    /// Set fields on the matching documents; how many matched.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn update_many(
        &self,
        collection: &str,
        filter: Option<&Filter>,
        set: Vec<(String, Value)>,
    ) -> Result<u64> {
        self.tick();
        let filter = filter.map(query_of).transpose()?.unwrap_or_default();
        let mut fields = Document::new();
        for (k, v) in &set {
            fields.insert(k.clone(), bson_of(v)?);
        }
        self.db
            .collection::<Document>(collection)
            .update_many(filter, doc! { "$set": fields })
            .await
            .map(|r| r.matched_count)
            .map_err(|e| mongo_err(&e))
    }

    /// Remove the matching documents; how many.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn delete_many(&self, collection: &str, filter: Option<&Filter>) -> Result<u64> {
        self.tick();
        let filter = filter.map(query_of).transpose()?.unwrap_or_default();
        self.db
            .collection::<Document>(collection)
            .delete_many(filter)
            .await
            .map(|r| r.deleted_count)
            .map_err(|e| mongo_err(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::query_of;
    use crate::filter::col;
    use crate::value::Value;
    use mongodb::bson::doc;

    #[test]
    fn filters_become_queries() {
        let q = query_of(
            &col("name")
                .contains_insensitive("a.b")
                .and(!col("age").lt(3)),
        )
        .unwrap_or_default();
        assert_eq!(
            q,
            doc! { "$and": [
                { "name": mongodb::bson::Regex { pattern: "a\\.b".into(), options: "i".into() } },
                { "$nor": [ { "age": { "$lt": 3 } } ] },
            ] }
        );
        let q = query_of(&col("_id").equals(Value::ObjectId("65f1c2a9e4b0a1b2c3d4e5f6".into())))
            .unwrap_or_default();
        assert!(q.get_object_id("_id").is_ok());
        assert!(
            query_of(&col("_id").equals(Value::ObjectId("nope".into())))
                .is_err_and(|e| e.code == "P2023")
        );
    }
}
