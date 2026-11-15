//! Destructive and impossible changes, found before anything runs. Each check says which count it
//! needs (rows in a table, values in a column, NULLs in a column); [`pure`] words them without a
//! database for the migration file's header, [`against`] runs the counts so only real problems are
//! reported.

use sqlx::{AssertSqlSafe, PgPool};

use crate::diff::{Step, TableChange, TableRef};
use crate::postgres::render::ident;

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Data may be lost; needs `--accept-data-loss` or a yes at the prompt.
    Warning,
    /// The step cannot run on this data; fix the schema or the data first.
    Unexecutable,
}

/// What must be counted to know whether a check matters.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Count {
    Rows(TableRef),
    Values(TableRef, String),
    Nulls(TableRef, String),
    Nothing,
}

/// One check: what it says with and without a count.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Check {
    severity: Severity,
    count: Count,
    /// `{n}` is replaced by the count.
    with_count: String,
    without_count: String,
}

/// A finding to show before migrating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Warning or unexecutable.
    pub severity: Severity,
    /// What would happen, in plain words.
    pub message: String,
}

fn name(t: &TableRef) -> String {
    if t.schema == "public" {
        t.name.clone()
    } else {
        format!("{}.{}", t.schema, t.name)
    }
}

fn plan(steps: &[Step]) -> Vec<Check> {
    let mut out = Vec::new();
    for step in steps {
        match step {
            Step::DropTable(t) => out.push(Check {
                severity: Severity::Warning,
                count: Count::Rows(t.clone()),
                with_count: format!("You are about to drop the `{}` table, which has {{n}} rows.", name(t)),
                without_count: format!("You are about to drop the `{}` table. If it has rows, they will be lost.", name(t)),
            }),
            Step::AlterTable(t, changes) => {
                for change in changes {
                    if let Some(c) = column_check(t, change) {
                        out.push(c);
                    }
                }
            }
            Step::RewriteEnum { to, .. } => out.push(Check {
                severity: Severity::Warning,
                count: Count::Nothing,
                with_count: String::new(),
                without_count: format!(
                    "Values are being removed from the enum `{}`. Rows that still use them will make the migration fail.",
                    to.name
                ),
            }),
            Step::CreateIndex(t, i) if i.unique => out.push(Check {
                severity: Severity::Warning,
                count: Count::Rows(t.clone()),
                with_count: format!(
                    "A unique constraint `{}` is added on `{}` ({{n}} rows). The migration fails if there are duplicate values.",
                    i.name,
                    name(t)
                ),
                without_count: format!(
                    "A unique constraint `{}` is added on `{}`. The migration fails if there are duplicate values.",
                    i.name,
                    name(t)
                ),
            }),
            _ => {}
        }
    }
    out
}

fn column_check(t: &TableRef, change: &TableChange) -> Option<Check> {
    let table = name(t);
    Some(match change {
        TableChange::DropColumn(c) => Check {
            severity: Severity::Warning,
            count: Count::Values(t.clone(), c.clone()),
            with_count: format!(
                "You are about to drop the column `{c}` on `{table}`, which still has {{n}} non-null values."
            ),
            without_count: format!(
                "You are about to drop the column `{c}` on `{table}`. All the data in it will be lost."
            ),
        },
        TableChange::RecreateColumn(c) => Check {
            severity: Severity::Warning,
            count: Count::Values(t.clone(), c.name.clone()),
            with_count: format!(
                "The column `{}` on `{table}` changes to a type its values cannot be cast to, so it is dropped and added again; {{n}} values will be lost.",
                c.name
            ),
            without_count: format!(
                "The column `{}` on `{table}` changes to a type its values cannot be cast to, so it is dropped and added again. Its data will be lost.",
                c.name
            ),
        },
        TableChange::AddColumn(c) if !c.nullable && c.default.is_none() => Check {
            severity: Severity::Unexecutable,
            count: Count::Rows(t.clone()),
            with_count: format!(
                "The required column `{}` is added to `{table}` without a default, but the table has {{n}} rows.",
                c.name
            ),
            without_count: format!(
                "The required column `{}` is added to `{table}` without a default. This fails if the table has rows.",
                c.name
            ),
        },
        TableChange::AlterColumn(a) if a.nullable == Some(false) => Check {
            severity: Severity::Unexecutable,
            count: Count::Nulls(t.clone(), a.name.clone()),
            with_count: format!(
                "The column `{}` on `{table}` becomes required, but {{n}} rows have NULL in it.",
                a.name
            ),
            without_count: format!(
                "The column `{}` on `{table}` becomes required. This fails if any row has NULL in it.",
                a.name
            ),
        },
        TableChange::AlterColumn(a) => {
            let (from, to) = a.ty.as_ref()?;
            if crate::diff::cast(from, to).1 {
                return None;
            }
            Check {
                severity: Severity::Warning,
                count: Count::Values(t.clone(), a.name.clone()),
                with_count: format!(
                    "The column `{}` on `{table}` changes from `{from}` to `{to}`; {{n}} values must fit or the migration fails.",
                    a.name
                ),
                without_count: format!(
                    "The column `{}` on `{table}` changes from `{from}` to `{to}`. Values that do not fit will make the migration fail.",
                    a.name
                ),
            }
        }
        _ => return None,
    })
}

/// What could go wrong, without looking at the data (for the migration file's header).
#[must_use]
pub fn pure(steps: &[Step]) -> Vec<Finding> {
    plan(steps)
        .into_iter()
        .map(|c| Finding {
            severity: c.severity,
            message: c.without_count,
        })
        .collect()
}

fn qualified(t: &TableRef) -> String {
    if t.schema == "public" {
        ident(&t.name)
    } else {
        format!("{}.{}", ident(&t.schema), ident(&t.name))
    }
}

/// What will go wrong on this database: counts are fetched, checks that do not matter (an empty
/// table, a column with no values) are dropped.
///
/// # Errors
/// When a count query fails.
pub async fn against(pool: &PgPool, steps: &[Step]) -> Result<Vec<Finding>, sqlx::Error> {
    let mut out = Vec::new();
    for c in plan(steps) {
        let sql = match &c.count {
            Count::Nothing => {
                out.push(Finding {
                    severity: c.severity,
                    message: c.without_count,
                });
                continue;
            }
            Count::Rows(t) => format!("SELECT COUNT(*) FROM {}", qualified(t)),
            Count::Values(t, col) => format!(
                "SELECT COUNT(*) FROM {} WHERE {} IS NOT NULL",
                qualified(t),
                ident(col)
            ),
            Count::Nulls(t, col) => format!(
                "SELECT COUNT(*) FROM {} WHERE {} IS NULL",
                qualified(t),
                ident(col)
            ),
        };
        // the names come from the shapes and are quoted by `ident`
        let n: i64 = sqlx::query_scalar(AssertSqlSafe(sql))
            .fetch_one(pool)
            .await?;
        if n > 0 {
            out.push(Finding {
                severity: c.severity,
                message: c.with_count.replace("{n}", &n.to_string()),
            });
        }
    }
    Ok(out)
}
