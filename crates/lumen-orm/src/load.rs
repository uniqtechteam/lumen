//! Loading related rows for many parents at once: one query per relation level, never one per
//! parent. Single-column keys use `= ANY($1)` (one array parameter, so no parameter limit);
//! composite keys use `(a, b) IN (..)` in chunks under Postgres's parameter limit. Children keep
//! the select's order. Paging inside an include is per parent and runs in SQL with
//! `ROW_NUMBER() OVER (PARTITION BY ..)`: the child select's `limit` and `offset` are the `take`
//! and `skip` of each parent's children, never of all children together. Paging that SQL cannot
//! run (a negative value, or paging a to-one relation) is a P2009 error, never ignored.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use sqlx::FromRow;
use sqlx::postgres::PgRow;

use crate::db::Run;
use crate::error::{Error, Result};
use crate::filter::{Filter, Op};
use crate::query::Select;
use crate::value::Value;

/// Postgres accepts at most 65,535 parameters in one statement.
const MAX_PARAMETERS: usize = 65_535;

fn with(select: &Select, filter: Filter) -> Select {
    let mut s = select.clone();
    s.filter = Some(match s.filter.take() {
        Some(existing) => existing.and(filter),
        None => filter,
    });
    s
}

/// Paging inside an include must be something the database can run per parent.
pub(crate) fn check_paging(take: Option<i64>, skip: Option<i64>) -> Result<()> {
    if take.is_some_and(|n| n < 0) || skip.is_some_and(|n| n < 0) {
        return Err(Error::invalid_query(
            "take and skip inside an include must not be negative (lumen pages each parent's \
             related records forwards only)",
        ));
    }
    Ok(())
}

/// A to-one relation has at most one related record, so paging it means nothing; it is refused
/// rather than applied to all parents' records together.
pub(crate) fn refuse_paging(take: Option<i64>, skip: Option<i64>) -> Result<()> {
    if take.is_some() || skip.is_some() {
        return Err(Error::invalid_query(
            "take and skip cannot be used inside a to-one include",
        ));
    }
    Ok(())
}

fn distinct<K: Eq + Hash + Clone>(keys: impl Iterator<Item = K>) -> Vec<K> {
    let mut seen: HashSet<K> = HashSet::new();
    keys.filter(|k| seen.insert(k.clone())).collect()
}

/// The children of each parent, in the parents' order: `child_column` holds the parent's key.
/// `take` (or else `child.limit`) keeps at most that many children per parent and `child.offset`
/// skips that many per parent, in the select's order, done in SQL.
///
/// # Errors
/// P2009 for a negative `take` or `skip`; a coded error from the database.
pub async fn load_many<R, P, C, K>(
    run: &mut R,
    parents: &[P],
    parent_key: impl Fn(&P) -> Option<K>,
    child: &Select,
    child_column: &str,
    child_key: impl Fn(&C) -> Option<K>,
    take: Option<i64>,
) -> Result<Vec<Vec<C>>>
where
    R: Run,
    C: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    K: Eq + Hash + Clone + Into<Value>,
{
    let take = take.or(child.limit);
    check_paging(take, child.offset)?;
    let keys = distinct(parents.iter().filter_map(&parent_key));
    let mut grouped: HashMap<K, Vec<C>> = HashMap::new();
    if !keys.is_empty() {
        let mut q = with(
            child,
            Filter::Column(
                child_column.to_owned(),
                Op::Any(keys.into_iter().map(Into::into).collect()),
            ),
        );
        q.limit = take;
        q.per_key = vec![child_column.to_owned()];
        for c in run.fetch_all::<C>(q.build()).await? {
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

/// The one related row of each parent (or `None`), in the parents' order: `child_column` holds the
/// key `parent_key` gives. Used for `include` of a to-one relation.
///
/// # Errors
/// P2009 when `child` has a `limit` or `offset`; a coded error from the database.
pub async fn load_one<R, P, C, K>(
    run: &mut R,
    parents: &[P],
    parent_key: impl Fn(&P) -> Option<K>,
    child: &Select,
    child_column: &str,
    child_key: impl Fn(&C) -> Option<K>,
) -> Result<Vec<Option<C>>>
where
    R: Run,
    C: for<'r> FromRow<'r, PgRow> + Send + Unpin + Clone,
    K: Eq + Hash + Clone + Into<Value>,
{
    refuse_paging(child.limit, child.offset)?;
    let keys = distinct(parents.iter().filter_map(&parent_key));
    let mut found: HashMap<K, C> = HashMap::new();
    if !keys.is_empty() {
        let q = with(
            child,
            Filter::Column(
                child_column.to_owned(),
                Op::Any(keys.into_iter().map(Into::into).collect()),
            ),
        );
        for c in run.fetch_all::<C>(q.build()).await? {
            if let Some(k) = child_key(&c) {
                found.entry(k).or_insert(c);
            }
        }
    }
    Ok(parents
        .iter()
        .map(|p| parent_key(p).and_then(|k| found.get(&k).cloned()))
        .collect())
}

/// Like [`load_many`] for a composite key: `child_columns` hold the parent's key parts, which
/// `parts` turns into values. Keys are sent in chunks under the parameter limit. `child.limit` and
/// `child.offset` page each parent's children, in SQL.
///
/// # Errors
/// P2009 for a negative `limit` or `offset`; a coded error from the database.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors load_many with the composite key's parts"
)]
pub async fn load_many_composite<R, P, C, K>(
    run: &mut R,
    parents: &[P],
    parent_key: impl Fn(&P) -> Option<K>,
    child: &Select,
    child_columns: &[String],
    child_key: impl Fn(&C) -> K,
    parts: impl Fn(&K) -> Vec<Value>,
) -> Result<Vec<Vec<C>>>
where
    R: Run,
    C: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    K: Eq + Hash + Clone,
{
    check_paging(child.limit, child.offset)?;
    let keys = distinct(parents.iter().filter_map(&parent_key));
    let per_chunk = (MAX_PARAMETERS / child_columns.len().max(1)).max(1) - 1;
    let mut grouped: HashMap<K, Vec<C>> = HashMap::new();
    for chunk in keys.chunks(per_chunk) {
        let rows = chunk.iter().map(&parts).collect();
        let mut q = with(child, Filter::Tuples(child_columns.to_vec(), rows));
        q.per_key = child_columns.to_vec();
        for c in run.fetch_all::<C>(q.build()).await? {
            grouped.entry(child_key(&c)).or_default().push(c);
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
