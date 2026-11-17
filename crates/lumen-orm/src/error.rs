//! Errors with stable codes. The numbers are Prisma's (P2002 unique, P2003 foreign key, P2025 not
//! found, ...) so what people already know carries over. The fields come from the structured
//! database error and the schema's constraint names, never from parsing the message text.

use std::collections::HashMap;

/// A database operation failed. Boxed so `Result<T>` stays small; the details are read through
/// `Deref` (`err.code`, `err.fields`).
#[derive(Debug)]
pub struct Error(Box<ErrorInfo>);

/// What an [`Error`] says.
#[derive(Debug)]
pub struct ErrorInfo {
    /// The stable code, for example `P2002`.
    pub code: &'static str,
    /// What went wrong, in plain words.
    pub message: String,
    /// The constraint the database named, if any.
    pub constraint: Option<String>,
    /// The fields behind that constraint, when the schema's names are known.
    pub fields: Vec<String>,
    source: Option<sqlx::Error>,
}

impl std::ops::Deref for Error {
    type Target = ErrorInfo;

    fn deref(&self) -> &ErrorInfo {
        &self.0
    }
}

impl Error {
    fn build(
        code: &'static str,
        message: String,
        constraint: Option<String>,
        fields: Vec<String>,
        source: Option<sqlx::Error>,
    ) -> Self {
        Self(Box::new(ErrorInfo {
            code,
            message,
            constraint,
            fields,
            source,
        }))
    }

    /// An error with a code and message.
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self::build(code, message.into(), None, Vec::new(), None)
    }

    /// P2002: a duplicate on the unique constraint (or index) `constraint`, made of `fields`.
    #[must_use]
    #[cfg(feature = "mongodb")]
    pub(crate) fn duplicate(constraint: Option<String>, fields: Vec<String>) -> Self {
        let named = if fields.is_empty() {
            constraint.as_deref().map_or_else(
                || "unique value".to_owned(),
                |c| format!("unique value (index {c})"),
            )
        } else {
            fields.join(", ")
        };
        let message = format!("a record with the same {named} already exists");
        Self::build("P2002", message, constraint, fields, None)
    }

    /// P2025: the record an operation needed does not exist.
    #[must_use]
    pub fn not_found(what: &str) -> Self {
        Self::new("P2025", format!("no {what} matches the conditions"))
    }

    /// P2009: a query lumen cannot run as asked, for example paging inside a to-one include.
    #[must_use]
    pub fn invalid_query(message: impl Into<String>) -> Self {
        Self::new("P2009", message)
    }

    /// Whether this is a unique-constraint violation (P2002).
    #[must_use]
    pub fn is_unique_violation(&self) -> bool {
        self.code == "P2002"
    }

    /// Whether no record matched (P2025).
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.code == "P2025"
    }

    /// The HTTP status a web API should answer with: 404 when nothing matched, 409 for a
    /// duplicate, a record still in use or a transaction conflict, 400 for a value the database
    /// refused, 503 when the database is unreachable, and 500 for everything else. Frameworks map
    /// an `Error` with this, `code` and `message`.
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self.code {
            "P2025" => 404,
            "P2002" | "P2003" | "P2034" => 409,
            "P2000" | "P2009" | "P2011" | "P2023" => 400,
            "P1001" => 503,
            _ => 500,
        }
    }

    /// Turn a driver error into a coded one. `constraints` maps constraint names to fields.
    #[must_use]
    pub fn from_sqlx(e: sqlx::Error, constraints: &HashMap<String, Vec<String>>) -> Self {
        let Some(db) = e.as_database_error() else {
            let (code, message) = match &e {
                sqlx::Error::PoolTimedOut | sqlx::Error::Io(_) | sqlx::Error::Tls(_) => {
                    ("P1001", format!("cannot reach the database: {e}"))
                }
                sqlx::Error::RowNotFound => {
                    ("P2025", "no record matches the conditions".to_owned())
                }
                _ => ("P2010", e.to_string()),
            };
            return Self::build(code, message, None, Vec::new(), Some(e));
        };
        let constraint = db.constraint().map(str::to_owned);
        let fields = constraint
            .as_ref()
            .and_then(|c| constraints.get(c))
            .cloned()
            .unwrap_or_default();
        let named = if fields.is_empty() {
            constraint.clone().unwrap_or_default()
        } else {
            fields.join(", ")
        };
        let (code, message) = match db.code().as_deref() {
            Some("23505") => (
                "P2002",
                format!("a record with the same {named} already exists"),
            ),
            Some("23503") => (
                "P2003",
                format!("the foreign key {named} points at a record that does not exist"),
            ),
            Some("23502") => (
                "P2011",
                format!("a required value is missing ({})", db.message()),
            ),
            Some("22001") => (
                "P2000",
                format!("a value is too long for its column ({})", db.message()),
            ),
            Some("40001" | "40P01") => (
                "P2034",
                "the transaction conflicted with another one; try again".to_owned(),
            ),
            _ => ("P2010", db.message().to_owned()),
        };
        Self::build(code, message, constraint, fields, Some(e))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0
            .source
            .as_ref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// Results of database operations.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn an_error_is_one_pointer_wide() {
        assert_eq!(size_of::<Error>(), size_of::<usize>());
    }

    #[test]
    fn codes_map_to_http_statuses() {
        let status = |code| Error::new(code, "x").http_status();
        assert_eq!(status("P2025"), 404);
        assert_eq!(status("P2002"), 409);
        assert_eq!(status("P2003"), 409);
        assert_eq!(status("P2011"), 400);
        assert_eq!(status("P2009"), 400);
        assert_eq!(status("P1001"), 503);
        assert_eq!(status("P2010"), 500);
        assert!(Error::not_found("user").is_not_found());
    }
}
