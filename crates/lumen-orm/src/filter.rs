//! Conditions for `WHERE`, with Prisma's operators: equals, not, in, notIn, lt, lte, gt, gte,
//! contains, startsWith, endsWith (optionally case-insensitive), null checks, AND, OR, NOT, and
//! the relation filters some, every, none, is and isNot.

use crate::query::Table;
use crate::value::Value;

/// Case-sensitive (the default) or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Exact match.
    #[default]
    Default,
    /// Ignore case (`ILIKE`, `LOWER(..)`).
    Insensitive,
}

/// What a column is compared with.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// `= value` (`IS NULL` for `Value::Null`).
    Equals(Value, Mode),
    /// `<> value` (`IS NOT NULL` for `Value::Null`).
    NotEquals(Value, Mode),
    /// `IN (..)`.
    In(Vec<Value>),
    /// `NOT IN (..)`.
    NotIn(Vec<Value>),
    /// `< value`.
    Lt(Value),
    /// `<= value`.
    Lte(Value),
    /// `> value`.
    Gt(Value),
    /// `>= value`.
    Gte(Value),
    /// `LIKE '%value%'`.
    Contains(String, Mode),
    /// `LIKE 'value%'`.
    StartsWith(String, Mode),
    /// `LIKE '%value'`.
    EndsWith(String, Mode),
    /// `IS NULL` (`true`) or `IS NOT NULL` (`false`).
    IsNull(bool),
    /// `= ANY($1)` with one array parameter: loading by many keys without a parameter limit.
    Any(Vec<Value>),
}

/// A condition tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// All must hold (an empty `And` is true).
    And(Vec<Filter>),
    /// One must hold (an empty `Or` is false).
    Or(Vec<Filter>),
    /// Must not hold.
    Not(Box<Filter>),
    /// A column condition.
    Column(String, Op),
    /// `("a", "b") IN (($1, $2), ..)`: loading by composite keys.
    Tuples(Vec<String>, Vec<Vec<Value>>),
    /// A condition on related rows: Prisma's `some`, `every`, `none`, `is` and `isNot`.
    Relation(Quantifier, Box<Related>),
}

/// How many related rows must match a relation filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantifier {
    /// At least one related row matches (`EXISTS`).
    Some,
    /// Every related row matches; true when there are none (`NOT EXISTS` of the rest).
    Every,
    /// No related row matches (`NOT EXISTS`).
    None,
    /// The one related row exists and matches (`EXISTS`).
    Is,
    /// There is no related row, or it does not match (`NOT EXISTS`).
    IsNot,
}

/// How the related rows link to the filtered ones.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// `(related column, this column)` pairs that must be equal.
    Direct(Vec<(String, String)>),
    /// Many-to-many through a join table.
    Through {
        /// The join table.
        table: Table,
        /// `(join column, this column)` pairs.
        this: Vec<(String, String)>,
        /// `(join column, related column)` pairs.
        related: Vec<(String, String)>,
    },
}

/// The related rows a relation filter looks at: where they are, how they link, which must match.
#[derive(Debug, Clone, PartialEq)]
pub struct Related {
    /// The related table (or collection).
    pub table: Table,
    /// How its rows link to the filtered rows.
    pub link: Link,
    /// The condition on the related rows, with their own column names.
    pub filter: Filter,
}

impl Related {
    /// Rows of `table` where each `(related column, this column)` pair is equal.
    #[must_use]
    pub fn new(table: Table, on: &[(&str, &str)]) -> Self {
        Self {
            table,
            link: Link::Direct(pairs(on)),
            filter: Filter::And(Vec::new()),
        }
    }

    /// Rows of `table` linked through the join table `through`: `this` pairs join columns with this
    /// table's columns, `related` pairs them with the related table's columns.
    #[must_use]
    pub fn through(
        table: Table,
        through: Table,
        this: &[(&str, &str)],
        related: &[(&str, &str)],
    ) -> Self {
        Self {
            table,
            link: Link::Through {
                table: through,
                this: pairs(this),
                related: pairs(related),
            },
            filter: Filter::And(Vec::new()),
        }
    }

    fn quantified(mut self, quantifier: Quantifier, filter: Filter) -> Filter {
        self.filter = filter;
        Filter::Relation(quantifier, Box::new(self))
    }

    /// At least one related row matches `filter`.
    #[must_use]
    pub fn some(self, filter: Filter) -> Filter {
        self.quantified(Quantifier::Some, filter)
    }

    /// Every related row matches `filter` (true when there are none, as in Prisma).
    #[must_use]
    pub fn every(self, filter: Filter) -> Filter {
        self.quantified(Quantifier::Every, filter)
    }

    /// No related row matches `filter`.
    #[must_use]
    pub fn none(self, filter: Filter) -> Filter {
        self.quantified(Quantifier::None, filter)
    }

    /// The related row exists and matches `filter`.
    #[must_use]
    pub fn is(self, filter: Filter) -> Filter {
        self.quantified(Quantifier::Is, filter)
    }

    /// There is no related row, or it does not match `filter`.
    #[must_use]
    pub fn is_not(self, filter: Filter) -> Filter {
        self.quantified(Quantifier::IsNot, filter)
    }

    /// There is no related row (Prisma's `is: null`).
    #[must_use]
    pub fn is_null(self) -> Filter {
        self.quantified(Quantifier::None, Filter::And(Vec::new()))
    }

    /// There is a related row (Prisma's `isNot: null`).
    #[must_use]
    pub fn is_not_null(self) -> Filter {
        self.quantified(Quantifier::Some, Filter::And(Vec::new()))
    }
}

fn pairs(on: &[(&str, &str)]) -> Vec<(String, String)> {
    on.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

impl Filter {
    /// This and `other`.
    #[must_use]
    pub fn and(self, other: Filter) -> Filter {
        match self {
            Filter::And(mut items) => {
                items.push(other);
                Filter::And(items)
            }
            first => Filter::And(vec![first, other]),
        }
    }

    /// This or `other`.
    #[must_use]
    pub fn or(self, other: Filter) -> Filter {
        match self {
            Filter::Or(mut items) => {
                items.push(other);
                Filter::Or(items)
            }
            first => Filter::Or(vec![first, other]),
        }
    }
}

/// `!filter` is `NOT (filter)`.
impl std::ops::Not for Filter {
    type Output = Filter;

    fn not(self) -> Filter {
        Filter::Not(Box::new(self))
    }
}

/// A column to build conditions on: `col("email").contains("@")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Col(pub String);

/// The column `name`.
#[must_use]
pub fn col(name: &str) -> Col {
    Col(name.to_owned())
}

impl Col {
    fn op(&self, op: Op) -> Filter {
        Filter::Column(self.0.clone(), op)
    }

    /// `= value`.
    pub fn equals(&self, value: impl Into<Value>) -> Filter {
        self.op(Op::Equals(value.into(), Mode::Default))
    }

    /// `<> value`.
    pub fn not_equals(&self, value: impl Into<Value>) -> Filter {
        self.op(Op::NotEquals(value.into(), Mode::Default))
    }

    /// Equal, ignoring case.
    pub fn equals_insensitive(&self, value: impl Into<String>) -> Filter {
        self.op(Op::Equals(Value::Text(value.into()), Mode::Insensitive))
    }

    /// `IN (..)`.
    pub fn is_in<V: Into<Value>>(&self, values: impl IntoIterator<Item = V>) -> Filter {
        self.op(Op::In(values.into_iter().map(Into::into).collect()))
    }

    /// `NOT IN (..)`.
    pub fn not_in<V: Into<Value>>(&self, values: impl IntoIterator<Item = V>) -> Filter {
        self.op(Op::NotIn(values.into_iter().map(Into::into).collect()))
    }

    /// `< value`.
    pub fn lt(&self, value: impl Into<Value>) -> Filter {
        self.op(Op::Lt(value.into()))
    }

    /// `<= value`.
    pub fn lte(&self, value: impl Into<Value>) -> Filter {
        self.op(Op::Lte(value.into()))
    }

    /// `> value`.
    pub fn gt(&self, value: impl Into<Value>) -> Filter {
        self.op(Op::Gt(value.into()))
    }

    /// `>= value`.
    pub fn gte(&self, value: impl Into<Value>) -> Filter {
        self.op(Op::Gte(value.into()))
    }

    /// Contains the text.
    pub fn contains(&self, text: impl Into<String>) -> Filter {
        self.op(Op::Contains(text.into(), Mode::Default))
    }

    /// Contains the text, ignoring case.
    pub fn contains_insensitive(&self, text: impl Into<String>) -> Filter {
        self.op(Op::Contains(text.into(), Mode::Insensitive))
    }

    /// Starts with the text.
    pub fn starts_with(&self, text: impl Into<String>) -> Filter {
        self.op(Op::StartsWith(text.into(), Mode::Default))
    }

    /// Starts with the text, ignoring case.
    pub fn starts_with_insensitive(&self, text: impl Into<String>) -> Filter {
        self.op(Op::StartsWith(text.into(), Mode::Insensitive))
    }

    /// Ends with the text.
    pub fn ends_with(&self, text: impl Into<String>) -> Filter {
        self.op(Op::EndsWith(text.into(), Mode::Default))
    }

    /// Ends with the text, ignoring case.
    pub fn ends_with_insensitive(&self, text: impl Into<String>) -> Filter {
        self.op(Op::EndsWith(text.into(), Mode::Insensitive))
    }

    /// `IS NULL`.
    #[must_use]
    pub fn is_null(&self) -> Filter {
        self.op(Op::IsNull(true))
    }

    /// `IS NOT NULL`.
    #[must_use]
    pub fn is_not_null(&self) -> Filter {
        self.op(Op::IsNull(false))
    }
}
