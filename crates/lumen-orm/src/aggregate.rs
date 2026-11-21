//! Count, sum, average, min and max over the rows a filter matches, for the whole set or per
//! group, in one statement.
//!
//! ```
//! use lumen_orm::{Agg, Aggregate, Order, Table, col};
//!
//! let mut q = Aggregate::from(Table::new("Order"));
//! q.filter = Some(col("paid").equals(true));
//! q.group_by = vec!["status".into()];
//! q.values = vec![Agg::count_all(), Agg::sum("total").cast("bigint")];
//! q.having = Some(col(&Agg::count_all().alias()).gt(10_i64));
//! q.order = vec![(Agg::sum("total").alias(), Order::Desc)];
//! let built = q.build();
//! assert!(built.sql.contains("GROUP BY \"status\""));
//! ```
//!
//! Each value comes back in a column named by [`Agg::alias`] (`_count`, `_sum_total`, ..), and each
//! group column under its own name. `having` and `order` refer to those names, so a condition on
//! an aggregate needs that aggregate in `values`.

use std::fmt::Write as _;

use crate::filter::Filter;
use crate::query::{Built, Order, Table, Writer, ident};
use crate::value::Value;

/// Which aggregate function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFn {
    /// `COUNT(*)`, or `COUNT(column)`: rows where the column is not NULL.
    Count,
    /// `SUM(column)`.
    Sum,
    /// `AVG(column)`.
    Avg,
    /// `MIN(column)`.
    Min,
    /// `MAX(column)`.
    Max,
}

impl AggFn {
    fn name(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Sum => "sum",
            Self::Avg => "avg",
            Self::Min => "min",
            Self::Max => "max",
        }
    }
}

/// One aggregate value: a function, the column it reads (`None` for `COUNT(*)`), and an optional
/// SQL type the result is cast to, so its Rust type is known (`SUM` of a `bigint` is `numeric` in
/// Postgres; cast it back with `.cast("bigint")`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agg {
    /// The function.
    pub func: AggFn,
    /// The column, `None` for `COUNT(*)`.
    pub column: Option<String>,
    /// `::type` after the function call.
    pub cast: Option<String>,
}

impl Agg {
    fn on(func: AggFn, column: &str) -> Self {
        Self {
            func,
            column: Some(column.to_owned()),
            cast: None,
        }
    }

    /// `COUNT(*)`: every row (in a group).
    #[must_use]
    pub fn count_all() -> Self {
        Self {
            func: AggFn::Count,
            column: None,
            cast: None,
        }
    }

    /// `COUNT(column)`: rows where `column` is not NULL.
    #[must_use]
    pub fn count(column: &str) -> Self {
        Self::on(AggFn::Count, column)
    }

    /// `SUM(column)`; NULL when no row has a value.
    #[must_use]
    pub fn sum(column: &str) -> Self {
        Self::on(AggFn::Sum, column)
    }

    /// `AVG(column)`; NULL when no row has a value.
    #[must_use]
    pub fn avg(column: &str) -> Self {
        Self::on(AggFn::Avg, column)
    }

    /// `MIN(column)`.
    #[must_use]
    pub fn min(column: &str) -> Self {
        Self::on(AggFn::Min, column)
    }

    /// `MAX(column)`.
    #[must_use]
    pub fn max(column: &str) -> Self {
        Self::on(AggFn::Max, column)
    }

    /// Cast the result to `sql_type` (`bigint`, `double precision`, ..).
    #[must_use]
    pub fn cast(mut self, sql_type: &str) -> Self {
        self.cast = Some(sql_type.to_owned());
        self
    }

    /// The name of the result column: `_count`, `_count_email`, `_sum_total`, ..
    #[must_use]
    pub fn alias(&self) -> String {
        match &self.column {
            Some(c) => format!("_{}_{c}", self.func.name()),
            None => format!("_{}", self.func.name()),
        }
    }

    fn sql(&self) -> String {
        let arg = self.column.as_deref().map_or_else(|| "*".to_owned(), ident);
        let mut s = format!("{}({arg})", self.func.name().to_uppercase());
        if let Some(t) = &self.cast {
            let _ = write!(s, "::{t}");
        }
        let _ = write!(s, " AS {}", ident(&self.alias()));
        s
    }
}

/// Aggregates over a table, optionally grouped.
#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    /// From.
    pub table: Table,
    /// `WHERE`, on the rows before they are aggregated.
    pub filter: Option<Filter>,
    /// The values to compute. Empty without `group_by` means `COUNT(*)`.
    pub values: Vec<Agg>,
    /// `GROUP BY` these columns; empty aggregates all matching rows into one result.
    pub group_by: Vec<String>,
    /// A condition on the results (`HAVING`), naming group columns and value aliases.
    pub having: Option<Filter>,
    /// With `group_by`: the order of the groups, by group column or value alias. Without it: the
    /// order of the rows that `limit` and `offset` pick before aggregating.
    pub order: Vec<(String, Order)>,
    /// With `group_by`: at most this many groups. Without it: aggregate only this many rows.
    pub limit: Option<i64>,
    /// With `group_by`: skip this many groups. Without it: skip this many rows first.
    pub offset: Option<i64>,
}

impl Aggregate {
    /// Every row of `table`.
    #[must_use]
    pub fn from(table: Table) -> Self {
        Self {
            table,
            filter: None,
            values: Vec::new(),
            group_by: Vec::new(),
            having: None,
            order: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    /// The SQL: one statement, one row per group (one row without `group_by`).
    #[must_use]
    pub fn build(&self) -> Built {
        let mut w = Writer::on(&self.table);
        let values = if self.values.is_empty() && self.group_by.is_empty() {
            vec![Agg::count_all()]
        } else {
            self.values.clone()
        };
        let mut select: Vec<String> = self.group_by.iter().map(|c| ident(c)).collect();
        select.extend(values.iter().map(Agg::sql));
        let grouped = !self.group_by.is_empty();
        let outer = self.having.is_some() || (grouped && self.has_tail());
        if outer {
            w.push("SELECT * FROM (");
        }
        let _ = write!(w.sql, "SELECT {} FROM ", select.join(", "));
        if !grouped && self.has_tail() {
            // page the rows first, then aggregate the page
            w.push("(SELECT * FROM ");
            w.push(&self.table.sql());
            self.write_where(&mut w);
            self.write_tail(&mut w);
            w.push(") AS \"__lumen_rows\"");
        } else {
            w.push(&self.table.sql());
            self.write_where(&mut w);
        }
        if grouped {
            let _ = write!(
                w.sql,
                " GROUP BY {}",
                self.group_by
                    .iter()
                    .map(|c| ident(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if outer {
            w.push(") AS \"__lumen_groups\"");
            if let Some(h) = &self.having {
                w.push(" WHERE ");
                w.filter(h);
            }
            if grouped {
                self.write_tail(&mut w);
            }
        }
        w.done()
    }

    fn has_tail(&self) -> bool {
        !self.order.is_empty() || self.limit.is_some() || self.offset.is_some()
    }

    fn write_where(&self, w: &mut Writer) {
        if let Some(f) = &self.filter {
            w.push(" WHERE ");
            w.filter(f);
        }
    }

    fn write_tail(&self, w: &mut Writer) {
        if !self.order.is_empty() {
            let parts: Vec<String> = self
                .order
                .iter()
                .map(|(c, o)| {
                    format!(
                        "{} {}",
                        ident(c),
                        if *o == Order::Desc { "DESC" } else { "ASC" }
                    )
                })
                .collect();
            let _ = write!(w.sql, " ORDER BY {}", parts.join(", "));
        }
        if let Some(n) = self.limit {
            w.push(" LIMIT ");
            w.param(Value::BigInt(n));
        }
        if let Some(n) = self.offset {
            w.push(" OFFSET ");
            w.param(Value::BigInt(n));
        }
    }
}

/// Read an aggregate column from a result row: `None` when the value is NULL or the column was
/// not asked for. The generated client decodes its aggregate results with this.
///
/// # Errors
/// When the column holds a different type.
pub fn read<'r, T>(row: &'r sqlx::postgres::PgRow, column: &str) -> Result<Option<T>, sqlx::Error>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    match sqlx::Row::try_get::<Option<T>, _>(row, column) {
        Ok(v) => Ok(v),
        Err(sqlx::Error::ColumnNotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::{Agg, Aggregate};
    use crate::filter::col;
    use crate::query::{Order, Table};
    use crate::value::Value;

    #[test]
    fn whole_table() {
        let q = Aggregate::from(Table::new("Order"));
        assert_eq!(
            q.build().sql,
            "SELECT COUNT(*) AS \"_count\" FROM \"Order\""
        );
    }

    #[test]
    fn values_with_filter_and_casts() {
        let mut q = Aggregate::from(Table::new("Order"));
        q.filter = Some(col("paid").equals(true));
        q.values = vec![
            Agg::count_all(),
            Agg::count("note"),
            Agg::sum("total").cast("bigint"),
            Agg::avg("total").cast("double precision"),
            Agg::min("createdAt"),
            Agg::max("createdAt"),
        ];
        let b = q.build();
        assert_eq!(
            b.sql,
            "SELECT COUNT(*) AS \"_count\", COUNT(\"note\") AS \"_count_note\", \
             SUM(\"total\")::bigint AS \"_sum_total\", AVG(\"total\")::double precision AS \"_avg_total\", \
             MIN(\"createdAt\") AS \"_min_createdAt\", MAX(\"createdAt\") AS \"_max_createdAt\" \
             FROM \"Order\" WHERE \"paid\" = $1"
        );
        assert_eq!(b.params, vec![Value::Bool(true)]);
    }

    #[test]
    fn paging_without_groups_applies_to_rows() {
        let mut q = Aggregate::from(Table::new("Order"));
        q.values = vec![Agg::sum("total")];
        q.order = vec![("id".into(), Order::Desc)];
        q.limit = Some(10);
        assert_eq!(
            q.build().sql,
            "SELECT SUM(\"total\") AS \"_sum_total\" FROM (SELECT * FROM \"Order\" \
             ORDER BY \"id\" DESC LIMIT $1) AS \"__lumen_rows\""
        );
    }

    #[test]
    fn groups_with_having_order_and_paging() {
        let mut q = Aggregate::from(Table::new("Order"));
        q.filter = Some(col("total").gt(0));
        q.group_by = vec!["status".into()];
        q.values = vec![Agg::count_all(), Agg::sum("total").cast("bigint")];
        q.having = Some(col("_count").gte(2_i64).or(col("status").equals("NEW")));
        q.order = vec![("_sum_total".into(), Order::Desc), ("status".into(), Order::Asc)];
        q.limit = Some(5);
        q.offset = Some(5);
        let b = q.build();
        assert_eq!(
            b.sql,
            "SELECT * FROM (SELECT \"status\", COUNT(*) AS \"_count\", SUM(\"total\")::bigint AS \"_sum_total\" \
             FROM \"Order\" WHERE \"total\" > $1 GROUP BY \"status\") AS \"__lumen_groups\" \
             WHERE (\"_count\" >= $2 OR \"status\" = $3) ORDER BY \"_sum_total\" DESC, \"status\" ASC \
             LIMIT $4 OFFSET $5"
        );
        assert_eq!(b.params.len(), 5);
    }

    #[test]
    fn groups_alone_need_no_subquery() {
        let mut q = Aggregate::from(Table::new("Order"));
        q.group_by = vec!["status".into(), "paid".into()];
        assert_eq!(
            q.build().sql,
            "SELECT \"status\", \"paid\" FROM \"Order\" GROUP BY \"status\", \"paid\""
        );
    }
}
