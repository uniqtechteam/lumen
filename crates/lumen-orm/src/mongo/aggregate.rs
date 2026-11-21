//! `aggregate()` and `group_by()` on MongoDB: the same [`Aggregate`] statement as on Postgres,
//! run as one `$match` / `$group` / `$project` pipeline, with the same results: a count of 0 and
//! no other values over no documents, and no sum (rather than 0) when no document has a value.

use std::marker::PhantomData;

use mongodb::bson::{Bson, Document, doc};
use serde::de::DeserializeOwned;

use super::{MongoRun, query_of, sort_doc};
use crate::aggregate::{Agg, AggFn, Aggregate, Tag, aggregate_methods, group_by_methods};
use crate::error::{Error, Result};
use crate::query::Table;

fn helper(column: &str) -> String {
    format!("__lumen_n_{column}")
}

/// Rows where `column` holds a value (not null, not missing): 1, else 0.
fn present(column: &str) -> Bson {
    Bson::Document(doc! { "$cond": [{ "$gt": [format!("${column}"), Bson::Null] }, 1, 0] })
}

fn accumulator(agg: &Agg) -> Bson {
    let Some(column) = agg.column.as_deref() else {
        // COUNT(*); a value function always has a column
        return Bson::Document(doc! { "$sum": 1 });
    };
    let field = format!("${column}");
    Bson::Document(match agg.func {
        AggFn::Count => doc! { "$sum": present(column) },
        AggFn::Sum => doc! { "$sum": field },
        AggFn::Avg => doc! { "$avg": field },
        AggFn::Min => doc! { "$min": field },
        AggFn::Max => doc! { "$max": field },
    })
}

/// The pipeline for `agg`, its filter already free of relation filters.
///
/// # Errors
/// When a filter value has no BSON form.
pub fn pipeline(agg: &Aggregate) -> Result<Vec<Document>> {
    let mut stages = Vec::new();
    if let Some(f) = &agg.filter {
        stages.push(doc! { "$match": query_of(f)? });
    }
    let grouped = !agg.group_by.is_empty();
    let page = |stages: &mut Vec<Document>| {
        if !agg.order.is_empty() {
            stages.push(doc! { "$sort": sort_doc(&agg.order) });
        }
        if let Some(n) = agg.offset {
            stages.push(doc! { "$skip": n });
        }
        if let Some(n) = agg.limit {
            stages.push(doc! { "$limit": n });
        }
    };
    if !grouped && agg.has_tail() {
        page(&mut stages);
    }
    let values = values(agg);
    let mut group = Document::new();
    let mut project = doc! { "_id": 0 };
    if grouped {
        let mut key = Document::new();
        for c in &agg.group_by {
            key.insert(c.clone(), format!("${c}"));
            project.insert(c.clone(), format!("$_id.{c}"));
        }
        group.insert("_id", key);
    } else {
        group.insert("_id", Bson::Null);
    }
    for v in &values {
        let alias = v.alias();
        group.insert(alias.clone(), accumulator(v));
        if let (AggFn::Sum, Some(c)) = (v.func, v.column.as_deref()) {
            // $sum gives 0 when no document has a value; SQL gives NULL
            group.insert(helper(c), doc! { "$sum": present(c) });
            project.insert(
                alias.clone(),
                doc! { "$cond": [{ "$gt": [format!("${}", helper(c)), 0] }, format!("${alias}"), Bson::Null] },
            );
        } else {
            project.insert(alias, 1);
        }
    }
    stages.push(doc! { "$group": group });
    stages.push(doc! { "$project": project });
    if let Some(h) = &agg.having {
        stages.push(doc! { "$match": query_of(h)? });
    }
    if grouped {
        page(&mut stages);
    }
    Ok(stages)
}

fn values(agg: &Aggregate) -> Vec<Agg> {
    if agg.values.is_empty() && agg.group_by.is_empty() {
        vec![Agg::count_all()]
    } else {
        agg.values.clone()
    }
}

/// The one result of an ungrouped aggregate over no documents, where `$group` gives nothing:
/// counts are 0, every other value is missing.
pub(super) fn empty_result(agg: &Aggregate) -> Document {
    let mut d = Document::new();
    for v in values(agg) {
        if v.func == AggFn::Count {
            d.insert(v.alias(), 0_i64);
        }
    }
    d
}

/// Read an aggregate field from a result document: `None` when it is null or missing.
///
/// # Errors
/// P2023 when the field holds a different type.
pub fn read<T: DeserializeOwned>(d: &Document, field: &str) -> Result<Option<T>> {
    match d.get(field) {
        None | Some(Bson::Null) => Ok(None),
        Some(b) => mongodb::bson::from_bson(b.clone())
            .map(Some)
            .map_err(|e| Error::new("P2023", format!("{field}: {e}"))),
    }
}

/// A MongoDB model that `aggregate()` and `group_by()` run on, implemented by the generated
/// client: its result types and how to read them.
pub trait MongoAggModel {
    /// One aggregate result.
    type Aggregate;
    /// One group of a `group_by()`.
    type Group;

    /// Read an aggregate result.
    ///
    /// # Errors
    /// P2023 when a value has a different type.
    fn aggregate(d: &Document) -> Result<Self::Aggregate>;

    /// Read a group.
    ///
    /// # Errors
    /// P2023 when a value has a different type.
    fn group(d: &Document) -> Result<Self::Group>;
}

/// `aggregate()` on MongoDB: values over the documents of `M` that match, in one pipeline.
pub struct MongoAggregateQuery<R, M> {
    run: R,
    query: Aggregate,
    tag: Tag<M>,
}

impl<R: MongoRun, M: MongoAggModel> MongoAggregateQuery<R, M> {
    /// Over every document of `collection`, run on `run`.
    #[must_use]
    pub fn new(run: R, collection: &str) -> Self {
        Self {
            run,
            query: Aggregate::from(Table::new(collection)),
            tag: PhantomData,
        }
    }

    aggregate_methods!();

    /// Run it. Without any value asked for, it counts.
    ///
    /// # Errors
    /// A coded error from the database.
    pub async fn exec(mut self) -> Result<M::Aggregate> {
        let docs = self.run.aggregate(&self.query).await?;
        let d = docs
            .into_iter()
            .next()
            .unwrap_or_else(|| empty_result(&self.query));
        M::aggregate(&d)
    }
}

/// `group_by()` on MongoDB: values per group of documents of `M` sharing the `group_by` fields.
pub struct MongoGroupByQuery<R, M> {
    run: R,
    query: Aggregate,
    tag: Tag<M>,
}

impl<R: MongoRun, M: MongoAggModel> MongoGroupByQuery<R, M> {
    /// Groups of `collection` by `fields`, run on `run`.
    #[must_use]
    pub fn new<'a>(run: R, collection: &str, fields: impl IntoIterator<Item = &'a str>) -> Self {
        let mut query = Aggregate::from(Table::new(collection));
        query.group_by = fields.into_iter().map(str::to_owned).collect();
        Self {
            run,
            query,
            tag: PhantomData,
        }
    }

    group_by_methods!();

    /// Run it: one pipeline, one group per result document.
    ///
    /// # Errors
    /// A coded error from the database.
    pub async fn exec(mut self) -> Result<Vec<M::Group>> {
        let docs = self.run.aggregate(&self.query).await?;
        docs.iter().map(M::group).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{empty_result, pipeline};
    use crate::aggregate::{Agg, Aggregate};
    use crate::filter::col;
    use crate::query::{Order, Table};
    use mongodb::bson::doc;

    #[test]
    fn groups_project_keys_and_fix_empty_sums() -> crate::Result<()> {
        let mut q = Aggregate::from(Table::new("orders"));
        q.filter = Some(col("paid").equals(true));
        q.group_by = vec!["status".into()];
        q.values = vec![Agg::count_all(), Agg::sum("total")];
        q.having = Some(col("_count").gt(1_i64));
        q.order = vec![("_sum_total".into(), Order::Desc)];
        q.limit = Some(3);
        let p = pipeline(&q)?;
        assert_eq!(p[0], doc! { "$match": { "paid": true } });
        assert_eq!(
            p[1],
            doc! { "$group": {
                "_id": { "status": "$status" },
                "_count": { "$sum": 1 },
                "_sum_total": { "$sum": "$total" },
                "__lumen_n_total": { "$sum": { "$cond": [{ "$gt": ["$total", null] }, 1, 0] } },
            } }
        );
        assert_eq!(
            p[2],
            doc! { "$project": {
                "_id": 0,
                "status": "$_id.status",
                "_count": 1,
                "_sum_total": { "$cond": [{ "$gt": ["$__lumen_n_total", 0] }, "$_sum_total", null] },
            } }
        );
        assert_eq!(p[3], doc! { "$match": { "_count": { "$gt": 1_i64 } } });
        assert_eq!(p[4], doc! { "$sort": { "_sum_total": -1 } });
        assert_eq!(p[5], doc! { "$limit": 3_i64 });
        Ok(())
    }

    #[test]
    fn paging_without_groups_comes_before_the_group() -> crate::Result<()> {
        let mut q = Aggregate::from(Table::new("orders"));
        q.values = vec![Agg::max("total")];
        q.order = vec![("total".into(), Order::Desc)];
        q.limit = Some(2);
        let p = pipeline(&q)?;
        assert_eq!(p[0], doc! { "$sort": { "total": -1 } });
        assert_eq!(p[1], doc! { "$limit": 2_i64 });
        assert_eq!(p[2].keys().next().map(String::as_str), Some("$group"));
        Ok(())
    }

    #[test]
    fn no_documents_count_zero() {
        let mut q = Aggregate::from(Table::new("orders"));
        q.values = vec![Agg::count_all(), Agg::count("note"), Agg::sum("total")];
        assert_eq!(
            empty_result(&q),
            doc! { "_count": 0_i64, "_count_note": 0_i64 }
        );
    }
}
