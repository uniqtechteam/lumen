//! Conditions for `WHERE`, with Prisma's operators: equals, not, in, notIn, lt, lte, gt, gte,
//! contains, startsWith, endsWith (optionally case-insensitive), null checks, AND, OR, NOT.

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

    /// Ends with the text.
    pub fn ends_with(&self, text: impl Into<String>) -> Filter {
        self.op(Op::EndsWith(text.into(), Mode::Default))
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
