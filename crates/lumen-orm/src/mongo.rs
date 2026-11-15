//! MongoDB: the same filters, ordering and paging as on Postgres, rendered as BSON queries.
//! `not` keeps documents where the field is null or missing, as in Prisma.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use mongodb::ClientSession;
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
    if message.contains("Transaction numbers are only allowed") || message.contains("replica set") {
        return Error::new(
            "P2010",
            format!("MongoDB transactions need a replica set or a sharded cluster ({message})"),
        );
    }
    Error::new("P2010", message)
}

/// The query and options of a find.
fn find_parts(find: &Find) -> Result<(Document, FindOptions)> {
    let filter = find
        .filter
        .as_ref()
        .map(query_of)
        .transpose()?
        .unwrap_or_default();
    let mut options = FindOptions::default();
    if !find.order.is_empty() {
        options.sort = Some(sort_doc(&find.order));
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
    Ok((filter, options))
}

fn sort_doc(order: &[(String, Order)]) -> Document {
    let mut sort = Document::new();
    for (f, o) in order {
        sort.insert(f.clone(), if *o == Order::Desc { -1 } else { 1 });
    }
    sort
}

/// At most `n` documents per value of `key`, in the find's order, done on the server with
/// `$setWindowFields` (MongoDB 5+), like `ROW_NUMBER()` on Postgres.
fn per_key_pipeline(find: &Find, key: &str, n: i64) -> Result<Vec<Document>> {
    let filter = find
        .filter
        .as_ref()
        .map(query_of)
        .transpose()?
        .unwrap_or_default();
    let mut sort = sort_doc(&find.order);
    if sort.is_empty() {
        sort.insert("_id", 1);
    }
    let mut pipeline = vec![
        doc! { "$match": filter },
        doc! { "$setWindowFields": { "partitionBy": format!("${key}"), "sortBy": sort.clone(), "output": { "__lumen_rn": { "$documentNumber": {} } } } },
        doc! { "$match": { "__lumen_rn": { "$lte": n } } },
        doc! { "$unset": "__lumen_rn" },
        doc! { "$sort": sort },
    ];
    if !find.fields.is_empty() {
        let mut projection = Document::new();
        for f in &find.fields {
            projection.insert(f.clone(), 1);
        }
        pipeline.push(doc! { "$project": projection });
    }
    Ok(pipeline)
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
        let (filter, options) = find_parts(find)?;
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

    /// At most `n` matching documents per value of `key`.
    ///
    /// # Errors
    /// A coded [`Error`].
    pub async fn find_per_key<T: DeserializeOwned + Send + Sync>(
        &self,
        find: &Find,
        key: &str,
        n: i64,
    ) -> Result<Vec<T>> {
        self.tick();
        let mut cursor = self
            .db
            .collection::<Document>(&find.collection)
            .aggregate(per_key_pipeline(find, key, n)?)
            .await
            .map_err(|e| mongo_err(&e))?;
        let mut out = Vec::new();
        while cursor.advance().await.map_err(|e| mongo_err(&e))? {
            let d = cursor
                .deserialize_current()
                .map_err(|e| Error::new("P2023", e.to_string()))?;
            out.push(bson::from_document(d).map_err(|e| Error::new("P2023", e.to_string()))?);
        }
        Ok(out)
    }

    /// Run `f` in a transaction (needs a replica set): committed when it returns `Ok`, aborted when
    /// it returns `Err`.
    ///
    /// # Errors
    /// The error `f` returned, or a coded [`Error`] (P2010 on a standalone server).
    pub async fn transaction<T, F>(&self, f: F) -> Result<T>
    where
        F: AsyncFnOnce(&mut MongoTx) -> Result<T>,
    {
        let mut session = self
            .db
            .client()
            .start_session()
            .await
            .map_err(|e| mongo_err(&e))?;
        session
            .start_transaction()
            .await
            .map_err(|e| mongo_err(&e))?;
        let mut tx = MongoTx {
            db: self.db.clone(),
            session,
            statements: Arc::clone(&self.statements),
        };
        match f(&mut tx).await {
            Ok(v) => {
                tx.session
                    .commit_transaction()
                    .await
                    .map_err(|e| mongo_err(&e))?;
                Ok(v)
            }
            Err(e) => {
                let _ = tx.session.abort_transaction().await;
                Err(e)
            }
        }
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

/// An open MongoDB transaction; see [`MongoDb::transaction`].
#[derive(Debug)]
pub struct MongoTx {
    db: mongodb::Database,
    session: ClientSession,
    statements: Arc<AtomicU64>,
}

/// Something MongoDB operations run on: the database (`&MongoDb`) or a transaction (`MongoTx`).
#[allow(
    async_fn_in_trait,
    reason = "lumen's own code and the generated client are the callers"
)]
pub trait MongoRun {
    /// The matching documents.
    ///
    /// # Errors
    /// A coded [`Error`].
    async fn find<T: DeserializeOwned + Send + Sync>(&mut self, find: &Find) -> Result<Vec<T>>;
    /// At most `n` matching documents per value of `key`.
    ///
    /// # Errors
    /// A coded [`Error`].
    async fn find_per_key<T: DeserializeOwned + Send + Sync>(
        &mut self,
        find: &Find,
        key: &str,
        n: i64,
    ) -> Result<Vec<T>>;
    /// How many documents match.
    ///
    /// # Errors
    /// A coded [`Error`].
    async fn count(&mut self, collection: &str, filter: Option<&Filter>) -> Result<u64>;
    /// Insert documents; their ids.
    ///
    /// # Errors
    /// A coded [`Error`].
    async fn insert_many<T: Serialize + Send + Sync>(
        &mut self,
        collection: &str,
        docs: &[T],
    ) -> Result<Vec<Bson>>;
    /// Set fields on the matching documents; how many matched.
    ///
    /// # Errors
    /// A coded [`Error`].
    async fn update_many(
        &mut self,
        collection: &str,
        filter: Option<&Filter>,
        set: Vec<(String, Value)>,
    ) -> Result<u64>;
    /// Remove the matching documents; how many.
    ///
    /// # Errors
    /// A coded [`Error`].
    async fn delete_many(&mut self, collection: &str, filter: Option<&Filter>) -> Result<u64>;
}

impl MongoRun for &MongoDb {
    async fn find<T: DeserializeOwned + Send + Sync>(&mut self, find: &Find) -> Result<Vec<T>> {
        MongoDb::find(self, find).await
    }
    async fn find_per_key<T: DeserializeOwned + Send + Sync>(
        &mut self,
        find: &Find,
        key: &str,
        n: i64,
    ) -> Result<Vec<T>> {
        MongoDb::find_per_key(self, find, key, n).await
    }
    async fn count(&mut self, collection: &str, filter: Option<&Filter>) -> Result<u64> {
        MongoDb::count(self, collection, filter).await
    }
    async fn insert_many<T: Serialize + Send + Sync>(
        &mut self,
        collection: &str,
        docs: &[T],
    ) -> Result<Vec<Bson>> {
        MongoDb::insert_many(self, collection, docs).await
    }
    async fn update_many(
        &mut self,
        collection: &str,
        filter: Option<&Filter>,
        set: Vec<(String, Value)>,
    ) -> Result<u64> {
        MongoDb::update_many(self, collection, filter, set).await
    }
    async fn delete_many(&mut self, collection: &str, filter: Option<&Filter>) -> Result<u64> {
        MongoDb::delete_many(self, collection, filter).await
    }
}

impl<T: MongoRun> MongoRun for &mut T {
    async fn find<U: DeserializeOwned + Send + Sync>(&mut self, find: &Find) -> Result<Vec<U>> {
        (**self).find(find).await
    }
    async fn find_per_key<U: DeserializeOwned + Send + Sync>(
        &mut self,
        find: &Find,
        key: &str,
        n: i64,
    ) -> Result<Vec<U>> {
        (**self).find_per_key(find, key, n).await
    }
    async fn count(&mut self, collection: &str, filter: Option<&Filter>) -> Result<u64> {
        (**self).count(collection, filter).await
    }
    async fn insert_many<U: Serialize + Send + Sync>(
        &mut self,
        collection: &str,
        docs: &[U],
    ) -> Result<Vec<Bson>> {
        (**self).insert_many(collection, docs).await
    }
    async fn update_many(
        &mut self,
        collection: &str,
        filter: Option<&Filter>,
        set: Vec<(String, Value)>,
    ) -> Result<u64> {
        (**self).update_many(collection, filter, set).await
    }
    async fn delete_many(&mut self, collection: &str, filter: Option<&Filter>) -> Result<u64> {
        (**self).delete_many(collection, filter).await
    }
}

impl MongoRun for MongoTx {
    async fn find<T: DeserializeOwned + Send + Sync>(&mut self, find: &Find) -> Result<Vec<T>> {
        self.statements.fetch_add(1, Ordering::Relaxed);
        let (filter, options) = find_parts(find)?;
        let mut cursor = self
            .db
            .collection::<T>(&find.collection)
            .find(filter)
            .with_options(options)
            .session(&mut self.session)
            .await
            .map_err(|e| mongo_err(&e))?;
        let mut out = Vec::new();
        while cursor
            .advance(&mut self.session)
            .await
            .map_err(|e| mongo_err(&e))?
        {
            out.push(
                cursor
                    .deserialize_current()
                    .map_err(|e| Error::new("P2023", e.to_string()))?,
            );
        }
        Ok(out)
    }

    async fn find_per_key<T: DeserializeOwned + Send + Sync>(
        &mut self,
        find: &Find,
        key: &str,
        n: i64,
    ) -> Result<Vec<T>> {
        self.statements.fetch_add(1, Ordering::Relaxed);
        let mut cursor = self
            .db
            .collection::<Document>(&find.collection)
            .aggregate(per_key_pipeline(find, key, n)?)
            .session(&mut self.session)
            .await
            .map_err(|e| mongo_err(&e))?;
        let mut out = Vec::new();
        while cursor
            .advance(&mut self.session)
            .await
            .map_err(|e| mongo_err(&e))?
        {
            let d = cursor
                .deserialize_current()
                .map_err(|e| Error::new("P2023", e.to_string()))?;
            out.push(bson::from_document(d).map_err(|e| Error::new("P2023", e.to_string()))?);
        }
        Ok(out)
    }

    async fn count(&mut self, collection: &str, filter: Option<&Filter>) -> Result<u64> {
        self.statements.fetch_add(1, Ordering::Relaxed);
        let filter = filter.map(query_of).transpose()?.unwrap_or_default();
        self.db
            .collection::<Document>(collection)
            .count_documents(filter)
            .session(&mut self.session)
            .await
            .map_err(|e| mongo_err(&e))
    }

    async fn insert_many<T: Serialize + Send + Sync>(
        &mut self,
        collection: &str,
        docs: &[T],
    ) -> Result<Vec<Bson>> {
        self.statements.fetch_add(1, Ordering::Relaxed);
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let result = self
            .db
            .collection::<T>(collection)
            .insert_many(docs)
            .session(&mut self.session)
            .await
            .map_err(|e| mongo_err(&e))?;
        let mut ids: Vec<(usize, Bson)> = result.inserted_ids.into_iter().collect();
        ids.sort_by_key(|(i, _)| *i);
        Ok(ids.into_iter().map(|(_, id)| id).collect())
    }

    async fn update_many(
        &mut self,
        collection: &str,
        filter: Option<&Filter>,
        set: Vec<(String, Value)>,
    ) -> Result<u64> {
        self.statements.fetch_add(1, Ordering::Relaxed);
        let filter = filter.map(query_of).transpose()?.unwrap_or_default();
        let mut fields = Document::new();
        for (k, v) in &set {
            fields.insert(k.clone(), bson_of(v)?);
        }
        self.db
            .collection::<Document>(collection)
            .update_many(filter, doc! { "$set": fields })
            .session(&mut self.session)
            .await
            .map(|r| r.matched_count)
            .map_err(|e| mongo_err(&e))
    }

    async fn delete_many(&mut self, collection: &str, filter: Option<&Filter>) -> Result<u64> {
        self.statements.fetch_add(1, Ordering::Relaxed);
        let filter = filter.map(query_of).transpose()?.unwrap_or_default();
        self.db
            .collection::<Document>(collection)
            .delete_many(filter)
            .session(&mut self.session)
            .await
            .map(|r| r.deleted_count)
            .map_err(|e| mongo_err(&e))
    }
}

fn with_filter(find: &Find, extra: Filter) -> Find {
    let mut f = find.clone();
    f.filter = Some(match f.filter.take() {
        Some(existing) => existing.and(extra),
        None => extra,
    });
    f
}

/// The children of each parent, in the parents' order, in one query: `child_field` holds the
/// parent's key (`$in`). `take` keeps at most that many children per parent, on the server.
///
/// # Errors
/// A coded [`Error`].
pub async fn load_many<R, P, C, K>(
    run: &mut R,
    parents: &[P],
    parent_key: impl Fn(&P) -> Option<K>,
    child: &Find,
    child_field: &str,
    child_key: impl Fn(&C) -> Option<K>,
    take: Option<i64>,
) -> Result<Vec<Vec<C>>>
where
    R: MongoRun,
    C: DeserializeOwned + Send + Sync,
    K: Eq + std::hash::Hash + Clone + Into<Value>,
{
    let mut seen = std::collections::HashSet::new();
    let keys: Vec<K> = parents
        .iter()
        .filter_map(&parent_key)
        .filter(|k| seen.insert(k.clone()))
        .collect();
    let mut grouped: std::collections::HashMap<K, Vec<C>> = std::collections::HashMap::new();
    if !keys.is_empty() {
        let find = with_filter(
            child,
            Filter::Column(
                child_field.to_owned(),
                Op::Any(keys.into_iter().map(Into::into).collect()),
            ),
        );
        let rows: Vec<C> = match take {
            Some(n) => run.find_per_key(&find, child_field, n).await?,
            None => run.find(&find).await?,
        };
        for c in rows {
            if let Some(k) = child_key(&c) {
                grouped.entry(k).or_default().push(c);
            }
        }
    }
    Ok(parents
        .iter()
        .map(|p| {
            parent_key(p)
                .and_then(|k| grouped.get_mut(&k).map(std::mem::take))
                .unwrap_or_default()
        })
        .collect())
}

/// The one related document of each parent (or `None`), in the parents' order, in one query.
///
/// # Errors
/// A coded [`Error`].
pub async fn load_one<R, P, C, K>(
    run: &mut R,
    parents: &[P],
    parent_key: impl Fn(&P) -> Option<K>,
    child: &Find,
    child_field: &str,
    child_key: impl Fn(&C) -> Option<K>,
) -> Result<Vec<Option<C>>>
where
    R: MongoRun,
    C: DeserializeOwned + Send + Sync + Clone,
    K: Eq + std::hash::Hash + Clone + Into<Value>,
{
    let mut seen = std::collections::HashSet::new();
    let keys: Vec<K> = parents
        .iter()
        .filter_map(&parent_key)
        .filter(|k| seen.insert(k.clone()))
        .collect();
    let mut found: std::collections::HashMap<K, C> = std::collections::HashMap::new();
    if !keys.is_empty() {
        let find = with_filter(
            child,
            Filter::Column(
                child_field.to_owned(),
                Op::Any(keys.into_iter().map(Into::into).collect()),
            ),
        );
        for c in run.find::<C>(&find).await? {
            if let Some(k) = child_key(&c) {
                found.entry(k).or_insert(c);
            }
        }
    }
    // several parents may share one related document, so it is cloned, not moved
    Ok(parents
        .iter()
        .map(|p| parent_key(p).and_then(|k| found.get(&k).cloned()))
        .collect())
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
