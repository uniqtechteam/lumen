//! Connecting to the project's Postgres database, and creating it on first use the way
//! `prisma migrate dev` does.

use sqlx::postgres::PgConnection;
use sqlx::{AssertSqlSafe, Connection};

use crate::MigrateError;
use crate::postgres::render::ident;

/// Postgres's code for "database does not exist".
const NO_SUCH_DATABASE: &str = "3D000";

/// Connect to `url`, with a message a person can act on when it fails.
pub(crate) async fn connect(url: &str) -> Result<PgConnection, MigrateError> {
    check_url(url)?;
    PgConnection::connect(url)
        .await
        .map_err(|e| MigrateError::new(format!("cannot connect to the database: {}", reason(&e))))
}

/// Create the database `url` names when the server says it does not exist. Returns its name
/// when it was created, and `None` when it was already there.
///
/// # Errors
/// When the URL is not a Postgres URL, the server cannot be reached, or the database cannot be
/// created (the user needs `CREATEDB`).
pub async fn ensure_database(url: &str) -> Result<Option<String>, MigrateError> {
    check_url(url)?;
    match PgConnection::connect(url).await {
        Ok(conn) => {
            drop(conn);
            Ok(None)
        }
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some(NO_SUCH_DATABASE) => {
            let name = database_of(url).ok_or_else(|| {
                MigrateError::new(
                    "DATABASE_URL does not name a database, e.g. postgresql://localhost:5432/app",
                )
            })?;
            let mut admin = connect(&super::commands::with_database(url, "postgres")).await?;
            sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {}", ident(&name))))
                .execute(&mut admin)
                .await
                .map_err(|e| {
                    MigrateError::new(format!(
                        "database `{name}` does not exist and cannot be created: {}",
                        reason(&e)
                    ))
                })?;
            Ok(Some(name))
        }
        Err(e) => Err(MigrateError::new(format!(
            "cannot connect to the database: {}",
            reason(&e)
        ))),
    }
}

/// A Postgres project needs a `postgresql://` (or `postgres://`) URL. Only the scheme is shown,
/// so a password in the URL never ends up in the terminal.
fn check_url(url: &str) -> Result<(), MigrateError> {
    if url.starts_with("postgresql://") || url.starts_with("postgres://") {
        return Ok(());
    }
    let scheme = url.split_once(':').map_or("", |(s, _)| s);
    Err(MigrateError::new(format!(
        "DATABASE_URL must start with postgresql://, but it starts with `{scheme}:`. \
         A DATABASE_URL set in your shell wins over the one in .env, so check `echo $DATABASE_URL` too."
    )))
}

/// The server's own message, without sqlx's extra detail (such as the line of Postgres's source
/// that raised it).
fn reason(e: &sqlx::Error) -> String {
    match e {
        sqlx::Error::Database(db) => db.message().to_owned(),
        other => other.to_string(),
    }
}

/// The database name in a Postgres URL: the path after the host, without the query.
fn database_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let path = rest.split_once('/')?.1;
    let name = path.split_once('?').map_or(path, |(n, _)| n);
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests: a failure should stop the test")]
mod tests {
    use super::{check_url, database_of};

    #[test]
    fn the_database_name_comes_from_the_path() {
        assert_eq!(
            database_of("postgresql://u:p@h:5432/app").as_deref(),
            Some("app")
        );
        assert_eq!(
            database_of("postgres://h/app?schema=public").as_deref(),
            Some("app")
        );
        assert_eq!(database_of("postgresql://h:5432/"), None);
        assert_eq!(database_of("postgresql://h:5432"), None);
    }

    #[test]
    fn a_url_that_is_not_postgres_names_only_its_scheme() {
        assert!(check_url("postgresql://u:secret@h/app").is_ok());
        let e = check_url("file:/data/app.db").unwrap_err();
        assert!(e.message.contains("starts with `file:`"), "{}", e.message);
        let e = check_url("mysql://u:secret@h/app").unwrap_err();
        assert!(!e.message.contains("secret"), "{}", e.message);
    }
}
