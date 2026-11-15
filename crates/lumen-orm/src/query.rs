//! Statements as data, and their Postgres SQL. Rendering collects the parameters as it goes, so
//! placeholder numbers are always right and values never enter the SQL text.

use std::fmt::Write as _;

use crate::filter::{Filter, Mode, Op};
use crate::value::Value;

/// `"name"` with quotes doubled.
#[must_use]
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A table, optionally in a schema other than `public`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The schema, `None` for the search path.
    pub schema: Option<String>,
    /// The name.
    pub name: String,
}

impl Table {
    /// `name` in the default schema.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            schema: None,
            name: name.to_owned(),
        }
    }

    fn sql(&self) -> String {
        match &self.schema {
            Some(s) => format!("{}.{}", ident(s), ident(&self.name)),
            None => ident(&self.name),
        }
    }
}

/// Ascending or descending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Smallest first.
    Asc,
    /// Largest first.
    Desc,
}

/// SQL text and its parameters, in placeholder order.
#[derive(Debug, Clone, PartialEq)]
pub struct Built {
    /// The SQL.
    pub sql: String,
    /// `$1`, `$2`, ...
    pub params: Vec<Value>,
}

/// Collects SQL and parameters together.
#[derive(Debug, Default)]
struct Writer {
    sql: String,
    params: Vec<Value>,
}

impl Writer {
    fn push(&mut self, s: &str) {
        self.sql.push_str(s);
    }

    fn param(&mut self, value: Value) {
        // a NULL parameter would be typed `text`; the literal fits any column
        if value == Value::Null {
            self.push("NULL");
            return;
        }
        let cast = value.cast();
        self.params.push(value);
        let _ = write!(self.sql, "${}", self.params.len());
        if let Some(c) = cast {
            let _ = write!(self.sql, "::{c}");
        }
    }

    fn like(&mut self, column: &str, pattern: String, mode: Mode, negate: bool) {
        let op = match (mode, negate) {
            (Mode::Default, false) => "LIKE",
            (Mode::Default, true) => "NOT LIKE",
            (Mode::Insensitive, false) => "ILIKE",
            (Mode::Insensitive, true) => "NOT ILIKE",
        };
        let _ = write!(self.sql, "{} {op} ", ident(column));
        self.param(Value::Text(pattern));
    }

    fn compare(&mut self, column: &str, op: &str, value: Value, mode: Mode) {
        if mode == Mode::Insensitive {
            let _ = write!(self.sql, "LOWER({}) {op} LOWER(", ident(column));
            self.param(value);
            self.push(")");
        } else {
            let _ = write!(self.sql, "{} {op} ", ident(column));
            self.param(value);
        }
    }

    fn list(&mut self, column: &str, values: Vec<Value>, negate: bool) {
        if values.is_empty() {
            // `IN ()` is not SQL; an empty list matches nothing (or everything for NOT IN)
            self.push(if negate { "TRUE" } else { "FALSE" });
            return;
        }
        let _ = write!(
            self.sql,
            "{} {}IN (",
            ident(column),
            if negate { "NOT " } else { "" }
        );
        for (i, v) in values.into_iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.param(v);
        }
        self.push(")");
    }

    fn filter(&mut self, filter: &Filter) {
        match filter {
            Filter::And(items) | Filter::Or(items) if items.is_empty() => {
                self.push(if matches!(filter, Filter::And(_)) {
                    "TRUE"
                } else {
                    "FALSE"
                });
            }
            Filter::And(items) | Filter::Or(items) => {
                let joiner = if matches!(filter, Filter::And(_)) {
                    " AND "
                } else {
                    " OR "
                };
                self.push("(");
                for (i, f) in items.iter().enumerate() {
                    if i > 0 {
                        self.push(joiner);
                    }
                    self.filter(f);
                }
                self.push(")");
            }
            Filter::Not(inner) => {
                self.push("NOT (");
                self.filter(inner);
                self.push(")");
            }
            Filter::Column(column, op) => self.op(column, op.clone()),
        }
    }

    fn op(&mut self, column: &str, op: Op) {
        match op {
            Op::Equals(Value::Null, _) | Op::IsNull(true) => {
                let _ = write!(self.sql, "{} IS NULL", ident(column));
            }
            Op::NotEquals(Value::Null, _) | Op::IsNull(false) => {
                let _ = write!(self.sql, "{} IS NOT NULL", ident(column));
            }
            Op::Equals(v, mode) => self.compare(column, "=", v, mode),
            // Prisma's `not`: NULLs are not equal to anything, so they match too
            Op::NotEquals(v, mode) => {
                self.push("(");
                self.compare(column, "<>", v, mode);
                let _ = write!(self.sql, " OR {} IS NULL)", ident(column));
            }
            Op::In(values) => self.list(column, values, false),
            Op::NotIn(values) => self.list(column, values, true),
            Op::Lt(v) => self.compare(column, "<", v, Mode::Default),
            Op::Lte(v) => self.compare(column, "<=", v, Mode::Default),
            Op::Gt(v) => self.compare(column, ">", v, Mode::Default),
            Op::Gte(v) => self.compare(column, ">=", v, Mode::Default),
            Op::Contains(t, mode) => {
                self.like(column, format!("%{}%", escape_like(&t)), mode, false);
            }
            Op::StartsWith(t, mode) => {
                self.like(column, format!("{}%", escape_like(&t)), mode, false);
            }
            Op::EndsWith(t, mode) => {
                self.like(column, format!("%{}", escape_like(&t)), mode, false);
            }
        }
    }

    fn done(self) -> Built {
        Built {
            sql: self.sql,
            params: self.params,
        }
    }
}

/// `%`, `_` and `\` matched literally inside `LIKE`.
fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn columns_sql(columns: &[String]) -> String {
    if columns.is_empty() {
        "*".to_owned()
    } else {
        columns
            .iter()
            .map(|c| ident(c))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// `SELECT`.
#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    /// From.
    pub table: Table,
    /// The columns; empty means `*`.
    pub columns: Vec<String>,
    /// `WHERE`.
    pub filter: Option<Filter>,
    /// `ORDER BY`.
    pub order: Vec<(String, Order)>,
    /// `LIMIT`.
    pub limit: Option<i64>,
    /// `OFFSET`.
    pub offset: Option<i64>,
    /// `DISTINCT ON` these columns.
    pub distinct: Vec<String>,
}

impl Select {
    /// Every row of `table`.
    #[must_use]
    pub fn from(table: Table) -> Self {
        Self {
            table,
            columns: Vec::new(),
            filter: None,
            order: Vec::new(),
            limit: None,
            offset: None,
            distinct: Vec::new(),
        }
    }

    fn write_tail(&self, w: &mut Writer) {
        if let Some(f) = &self.filter {
            w.push(" WHERE ");
            w.filter(f);
        }
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

    /// The SQL.
    #[must_use]
    pub fn build(&self) -> Built {
        let mut w = Writer::default();
        w.push("SELECT ");
        if !self.distinct.is_empty() {
            let _ = write!(w.sql, "DISTINCT ON ({}) ", columns_sql(&self.distinct));
        }
        let _ = write!(
            w.sql,
            "{} FROM {}",
            columns_sql(&self.columns),
            self.table.sql()
        );
        self.write_tail(&mut w);
        w.done()
    }

    /// `SELECT COUNT(*)` over the same rows (ordering ignored, paging kept).
    #[must_use]
    pub fn count(&self) -> Built {
        let mut w = Writer::default();
        let inner = Select {
            columns: Vec::new(),
            order: Vec::new(),
            ..self.clone()
        };
        w.push("SELECT COUNT(*) FROM (SELECT 1 FROM ");
        w.push(&inner.table.sql());
        inner.write_tail(&mut w);
        w.push(") AS counted");
        w.done()
    }
}

/// `INSERT`, one or more rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Insert {
    /// Into.
    pub table: Table,
    /// The columns given.
    pub columns: Vec<String>,
    /// One value per column, per row.
    pub rows: Vec<Vec<Value>>,
    /// `ON CONFLICT (..) DO UPDATE SET ..` or `DO NOTHING`.
    pub on_conflict: Option<OnConflict>,
    /// `RETURNING`; empty returns nothing.
    pub returning: Vec<String>,
}

/// What to do when a unique key already exists.
#[derive(Debug, Clone, PartialEq)]
pub struct OnConflict {
    /// The unique columns.
    pub columns: Vec<String>,
    /// Columns to set (from the values given, or explicit values); empty means `DO NOTHING`.
    pub update: Vec<(String, Value)>,
}

impl Insert {
    /// The SQL.
    #[must_use]
    pub fn build(&self) -> Built {
        let mut w = Writer::default();
        let _ = write!(w.sql, "INSERT INTO {}", self.table.sql());
        if self.columns.is_empty() {
            w.push(" DEFAULT VALUES");
        } else {
            let _ = write!(w.sql, " ({}) VALUES ", columns_sql(&self.columns));
            for (i, row) in self.rows.iter().enumerate() {
                w.push(if i == 0 { "(" } else { ", (" });
                for (j, v) in row.iter().enumerate() {
                    if j > 0 {
                        w.push(", ");
                    }
                    w.param(v.clone());
                }
                w.push(")");
            }
        }
        if let Some(c) = &self.on_conflict {
            let _ = write!(w.sql, " ON CONFLICT ({}) ", columns_sql(&c.columns));
            if c.update.is_empty() {
                w.push("DO NOTHING");
            } else {
                w.push("DO UPDATE SET ");
                for (i, (column, v)) in c.update.iter().enumerate() {
                    if i > 0 {
                        w.push(", ");
                    }
                    let _ = write!(w.sql, "{} = ", ident(column));
                    w.param(v.clone());
                }
            }
        }
        if !self.returning.is_empty() {
            let _ = write!(w.sql, " RETURNING {}", columns_sql(&self.returning));
        }
        w.done()
    }
}

/// `UPDATE`.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    /// The table.
    pub table: Table,
    /// Columns and their new values.
    pub set: Vec<(String, Value)>,
    /// Which rows.
    pub filter: Option<Filter>,
    /// `RETURNING`.
    pub returning: Vec<String>,
}

impl Update {
    /// The SQL.
    #[must_use]
    pub fn build(&self) -> Built {
        let mut w = Writer::default();
        let _ = write!(w.sql, "UPDATE {} SET ", self.table.sql());
        for (i, (column, v)) in self.set.iter().enumerate() {
            if i > 0 {
                w.push(", ");
            }
            let _ = write!(w.sql, "{} = ", ident(column));
            w.param(v.clone());
        }
        if let Some(f) = &self.filter {
            w.push(" WHERE ");
            w.filter(f);
        }
        if !self.returning.is_empty() {
            let _ = write!(w.sql, " RETURNING {}", columns_sql(&self.returning));
        }
        w.done()
    }
}

/// `DELETE`.
#[derive(Debug, Clone, PartialEq)]
pub struct Delete {
    /// The table.
    pub table: Table,
    /// Which rows.
    pub filter: Option<Filter>,
    /// `RETURNING`.
    pub returning: Vec<String>,
}

impl Delete {
    /// The SQL.
    #[must_use]
    pub fn build(&self) -> Built {
        let mut w = Writer::default();
        let _ = write!(w.sql, "DELETE FROM {}", self.table.sql());
        if let Some(f) = &self.filter {
            w.push(" WHERE ");
            w.filter(f);
        }
        if !self.returning.is_empty() {
            let _ = write!(w.sql, " RETURNING {}", columns_sql(&self.returning));
        }
        w.done()
    }
}

#[cfg(test)]
mod tests {
    use super::{Delete, Insert, OnConflict, Order, Select, Table, Update};
    use crate::filter::col;
    use crate::value::Value;

    #[test]
    fn select_with_filters_order_and_paging() {
        let mut q = Select::from(Table::new("User"));
        q.columns = vec!["id".into(), "email".into()];
        q.filter = Some(
            col("email")
                .contains_insensitive("50%_off")
                .and(col("age").gte(18).or(col("role").is_in(["A", "B"])))
                .and(col("name").not_equals("x")),
        );
        q.order = vec![("id".into(), Order::Desc)];
        q.limit = Some(10);
        q.offset = Some(20);
        let b = q.build();
        assert_eq!(
            b.sql,
            "SELECT \"id\", \"email\" FROM \"User\" WHERE (\"email\" ILIKE $1 AND (\"age\" >= $2 OR \"role\" IN ($3, $4)) AND (\"name\" <> $5 OR \"name\" IS NULL)) ORDER BY \"id\" DESC LIMIT $6 OFFSET $7"
        );
        assert_eq!(b.params[0], Value::Text("%50\\%\\_off%".into()));
        assert_eq!(b.params.len(), 7);
    }

    #[test]
    fn casts_nulls_and_empty_lists() {
        let mut q = Select::from(Table {
            schema: Some("billing".into()),
            name: "Order".into(),
        });
        q.filter = Some(
            col("id")
                .equals(Value::Uuid("u".into()))
                .and(col("deleted").equals(Value::Null))
                .and(col("tag").is_in(Vec::<Value>::new())),
        );
        assert_eq!(
            q.build().sql,
            "SELECT * FROM \"billing\".\"Order\" WHERE (\"id\" = $1::uuid AND \"deleted\" IS NULL AND FALSE)"
        );
        assert_eq!(
            q.count().sql,
            "SELECT COUNT(*) FROM (SELECT 1 FROM \"billing\".\"Order\" WHERE (\"id\" = $1::uuid AND \"deleted\" IS NULL AND FALSE)) AS counted"
        );
    }

    #[test]
    fn writes() {
        let insert = Insert {
            table: Table::new("User"),
            columns: vec!["email".into(), "role".into()],
            rows: vec![vec!["a".into(), Value::Enum("A".into(), "\"Role\"".into())]],
            on_conflict: Some(OnConflict {
                columns: vec!["email".into()],
                update: vec![("role".into(), Value::Enum("B".into(), "\"Role\"".into()))],
            }),
            returning: vec!["id".into()],
        };
        assert_eq!(
            insert.build().sql,
            "INSERT INTO \"User\" (\"email\", \"role\") VALUES ($1, $2::\"Role\") ON CONFLICT (\"email\") DO UPDATE SET \"role\" = $3::\"Role\" RETURNING \"id\""
        );
        let update = Update {
            table: Table::new("User"),
            set: vec![("name".into(), "N".into())],
            filter: Some(col("id").equals(1)),
            returning: vec![],
        };
        assert_eq!(
            update.build().sql,
            "UPDATE \"User\" SET \"name\" = $1 WHERE \"id\" = $2"
        );
        let delete = Delete {
            table: Table::new("User"),
            filter: None,
            returning: vec!["id".into()],
        };
        assert_eq!(delete.build().sql, "DELETE FROM \"User\" RETURNING \"id\"");
    }
}
