//! `migrate deploy`, `status`, `reset` and `dev`: write, check and apply the migrations in the
//! migrations folder, and keep the history table up to date.

use std::fmt::Write as _;
use std::path::PathBuf;

use lumen_schema::checked::Schema;
use sqlx::postgres::PgConnection;
use sqlx::{AssertSqlSafe, PgPool};

use crate::check::{self, Finding, Severity};
use crate::database::connect;
use crate::history::{self, ADVISORY_LOCK_KEY, AppliedMigration, LocalMigration};
use crate::postgres::render::ident;
use crate::postgres::{describe, script};
use crate::{MigrateError, Shape, calculate, diff, renames};

/// What a command works on.
#[derive(Debug, Clone)]
pub struct Project {
    /// The checked schema.
    pub schema: Schema,
    /// The migrations folder.
    pub migrations_dir: PathBuf,
    /// The database.
    pub database_url: String,
    /// A database `dev` may wipe to replay the migrations; when `None`, a temporary database is
    /// created on the same server and dropped afterwards.
    pub shadow_database_url: Option<String>,
}

impl Project {
    fn schemas(&self) -> Vec<String> {
        match &self.schema.datasource {
            Some(d) if !d.schemas.is_empty() => d.schemas.clone(),
            _ => vec!["public".to_owned()],
        }
    }
}

/// Where each migration stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Applied and unchanged.
    pub applied: Vec<String>,
    /// In the folder, not applied yet.
    pub pending: Vec<String>,
    /// Started but never finished; `lumen migrate resolve` or a reset is needed.
    pub failed: Vec<String>,
    /// Applied, but the file changed since.
    pub edited: Vec<String>,
    /// Applied, but the folder is gone.
    pub missing: Vec<String>,
}

fn compare(local: &[LocalMigration], applied: &[AppliedMigration]) -> Status {
    let mut s = Status::default();
    let live: Vec<&AppliedMigration> = applied.iter().filter(|a| !a.rolled_back).collect();
    for m in local {
        match live.iter().find(|a| a.name == m.name) {
            None => s.pending.push(m.name.clone()),
            Some(a) if !a.finished => s.failed.push(m.name.clone()),
            Some(a) if !m.matches(&a.checksum) => s.edited.push(m.name.clone()),
            Some(_) => s.applied.push(m.name.clone()),
        }
    }
    for a in live {
        if !local.iter().any(|m| m.name == a.name) {
            s.missing.push(a.name.clone());
        }
    }
    s
}

pub use crate::database::ensure_database;

/// Where each migration stands.
///
/// # Errors
/// When the folder or the database cannot be read.
pub async fn status(project: &Project) -> Result<Status, MigrateError> {
    let local = history::read_dir(&project.migrations_dir)?;
    let mut conn = connect(&project.database_url).await?;
    let applied = history::applied(&mut conn).await?;
    Ok(compare(&local, &applied))
}

/// Apply every pending migration, in order, under the migrations advisory lock. Never needs a shadow
/// database, so it is safe to run in production.
///
/// # Errors
/// When a migration failed before (fix it first), or one fails now.
pub async fn deploy(project: &Project) -> Result<Vec<String>, MigrateError> {
    let local = history::read_dir(&project.migrations_dir)?;
    let mut conn = connect(&project.database_url).await?;
    sqlx::query("select pg_advisory_lock($1)")
        .bind(ADVISORY_LOCK_KEY)
        .execute(&mut conn)
        .await?;
    let result = deploy_locked(&mut conn, &local).await;
    sqlx::query("select pg_advisory_unlock($1)")
        .bind(ADVISORY_LOCK_KEY)
        .execute(&mut conn)
        .await?;
    result
}

async fn deploy_locked(
    conn: &mut PgConnection,
    local: &[LocalMigration],
) -> Result<Vec<String>, MigrateError> {
    history::ensure_table(conn).await?;
    let state = compare(local, &history::applied(conn).await?);
    if let Some(name) = state.failed.first() {
        return Err(MigrateError::new(format!(
            "the migration `{name}` failed earlier; fix the database, then run `lumen migrate resolve --applied {name}` or `--rolled-back {name}`"
        )));
    }
    let mut done = Vec::new();
    for m in local.iter().filter(|m| state.pending.contains(&m.name)) {
        history::apply(conn, m).await?;
        done.push(m.name.clone());
    }
    Ok(done)
}

/// Drop everything in the project's schemas and apply all migrations again. Development only.
///
/// # Errors
/// When the database cannot be wiped or a migration fails.
pub async fn reset(project: &Project) -> Result<Vec<String>, MigrateError> {
    let mut conn = connect(&project.database_url).await?;
    wipe(&mut conn, &project.schemas()).await?;
    drop(conn);
    deploy(project).await
}

async fn wipe(conn: &mut PgConnection, schemas: &[String]) -> Result<(), MigrateError> {
    for s in schemas {
        let sql = format!(
            "DROP SCHEMA IF EXISTS {s} CASCADE; CREATE SCHEMA {s};",
            s = ident(s)
        );
        // schema names come from the checked schema and are quoted
        sqlx::raw_sql(AssertSqlSafe(sql))
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// The URL of `database` on the server of `url`.
pub(crate) fn with_database(url: &str, database: &str) -> String {
    let (base, query) = url
        .split_once('?')
        .map_or((url, None), |(b, q)| (b, Some(q)));
    let base = base.rsplit_once('/').map_or(base, |(b, _)| b);
    match query {
        Some(q) => format!("{base}/{database}?{q}"),
        None => format!("{base}/{database}"),
    }
}

/// The shape the migrations folder produces, by replaying it on a shadow database. Cached by the
/// checksums of the migrations, so running `dev` again without new migrations is quick.
async fn shadow_shape(project: &Project, local: &[LocalMigration]) -> Result<Shape, MigrateError> {
    let list = local.iter().fold(String::new(), |mut acc, m| {
        let _ = writeln!(acc, "{}:{}", m.name, m.checksum());
        acc
    });
    let key = history::checksum(&list);
    let cache = std::env::temp_dir().join(format!("lumen-shadow-{key}.json"));
    if let Ok(text) = std::fs::read_to_string(&cache)
        && let Ok(shape) = serde_json::from_str::<Shape>(&text)
    {
        return Ok(shape);
    }
    let shape = if let Some(url) = &project.shadow_database_url {
        let mut conn = connect(url).await?;
        wipe(&mut conn, &project.schemas()).await?;
        replay(url, &mut conn, local, &project.schemas()).await?
    } else {
        let name = format!(
            "lumen_shadow_{}",
            &history::checksum(&format!("{key}{:?}", std::time::SystemTime::now()))[..16]
        );
        let mut admin = connect(&project.database_url).await?;
        sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {}", ident(&name))))
            .execute(&mut admin)
            .await
            .map_err(|e| MigrateError::new(format!("cannot create a shadow database ({e}); give the user CREATEDB or set shadowDatabaseUrl")))?;
        let url = with_database(&project.database_url, &name);
        let result = async {
            let mut conn = connect(&url).await?;
            let shape = replay(&url, &mut conn, local, &project.schemas()).await;
            drop(conn);
            shape
        }
        .await;
        sqlx::raw_sql(AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            ident(&name)
        )))
        .execute(&mut admin)
        .await?;
        result?
    };
    if let Ok(text) = serde_json::to_string(&shape) {
        let _ = std::fs::write(&cache, text);
    }
    Ok(shape)
}

async fn replay(
    url: &str,
    conn: &mut PgConnection,
    local: &[LocalMigration],
    schemas: &[String],
) -> Result<Shape, MigrateError> {
    for m in local {
        sqlx::raw_sql(AssertSqlSafe(m.script.clone()))
            .execute(&mut *conn)
            .await
            .map_err(|e| {
                MigrateError::new(format!(
                    "the migration `{}` does not run on an empty database: {e}",
                    m.name
                ))
            })?;
    }
    let pool = PgPool::connect(url).await?;
    let shape = describe(&pool, schemas).await?;
    pool.close().await;
    Ok(shape)
}

/// What `migrate dev` should do.
#[derive(Debug, Clone, Default)]
pub struct DevOptions {
    /// The new migration's name.
    pub name: String,
    /// Write the migration but do not apply it.
    pub create_only: bool,
    /// Go ahead although data may be lost.
    pub accept_data_loss: bool,
    /// The time to name the folder with (seconds since 1970); now when `None`.
    pub now: Option<i64>,
}

/// What `migrate dev` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevOutcome {
    /// The database and the migrations already match the schema.
    UpToDate {
        /// Pending migrations that were applied first.
        applied: Vec<String>,
    },
    /// A migration was written (and applied unless `create_only`).
    Created {
        /// Pending migrations that were applied first.
        applied: Vec<String>,
        /// The new folder name.
        folder: String,
        /// Its SQL.
        sql: String,
        /// Whether it was applied.
        ran: bool,
        /// Data-loss warnings that were accepted.
        warnings: Vec<Finding>,
    },
    /// Data would be lost; run again with `accept_data_loss` (the CLI asks).
    NeedsConfirmation(Vec<Finding>),
    /// A step cannot run on this data; change the schema or the data.
    Blocked(Vec<Finding>),
    /// The history or the database does not match the migrations; a reset is needed.
    NeedsReset(String),
}

/// `migrate dev`: apply pending migrations, check for drift, and write (and apply) a new
/// migration for the changes in the schema.
///
/// # Errors
/// When a database or the folder cannot be used.
pub async fn dev(project: &Project, options: &DevOptions) -> Result<DevOutcome, MigrateError> {
    history::ensure_lock(&project.migrations_dir, "postgresql")?;
    let local = history::read_dir(&project.migrations_dir)?;
    let mut conn = connect(&project.database_url).await?;
    history::ensure_table(&mut conn).await?;
    let state = compare(&local, &history::applied(&mut conn).await?);
    if let Some(reason) = reset_reason(&state) {
        return Ok(DevOutcome::NeedsReset(reason));
    }
    drop(conn);
    let applied = deploy(project).await?;
    let expected = shadow_shape(project, &local).await?;
    let pool = PgPool::connect(&project.database_url).await?;
    let live = describe(&pool, &project.schemas()).await?;
    let drift = diff(&expected, &live, &crate::Renames::default());
    if !drift.is_empty() {
        pool.close().await;
        let changes = script(&drift, &[]);
        return Ok(DevOutcome::NeedsReset(format!(
            "the database has changes that are not in any migration:\n{changes}"
        )));
    }
    let steps = diff(
        &live,
        &calculate(&project.schema),
        &renames(&project.schema),
    );
    if steps.is_empty() {
        pool.close().await;
        return Ok(DevOutcome::UpToDate { applied });
    }
    let findings = check::against(&pool, &steps).await?;
    pool.close().await;
    if findings
        .iter()
        .any(|f| f.severity == Severity::Unexecutable)
    {
        return Ok(DevOutcome::Blocked(findings));
    }
    if !findings.is_empty() && !options.accept_data_loss {
        return Ok(DevOutcome::NeedsConfirmation(findings));
    }
    let warnings: Vec<String> = check::pure(&steps).into_iter().map(|f| f.message).collect();
    let sql = script(&steps, &warnings);
    let now = options.now.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
    });
    // a new migration always sorts after the newest one, even within the same second
    let newest = local
        .last()
        .and_then(|m| m.name.get(..14))
        .and_then(history::parse_timestamp);
    let now = newest.map_or(now, |n| now.max(n + 1));
    let folder = history::folder_name(now, &options.name);
    let dir = project.migrations_dir.join(&folder);
    std::fs::create_dir_all(&dir)
        .map_err(|e| MigrateError::new(format!("cannot create {}: {e}", dir.display())))?;
    std::fs::write(dir.join("migration.sql"), &sql)
        .map_err(|e| MigrateError::new(format!("cannot write the migration: {e}")))?;
    let ran = !options.create_only;
    if ran {
        deploy(project).await?;
    }
    Ok(DevOutcome::Created {
        applied,
        folder,
        sql,
        ran,
        warnings: findings,
    })
}

fn reset_reason(state: &Status) -> Option<String> {
    if let Some(name) = state.failed.first() {
        return Some(format!("the migration `{name}` failed when it was applied"));
    }
    if let Some(name) = state.edited.first() {
        return Some(format!(
            "the migration `{name}` was changed after it was applied"
        ));
    }
    if let Some(name) = state.missing.first() {
        return Some(format!(
            "the migration `{name}` is in the database but not in the migrations folder"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::with_database;

    #[test]
    fn shadow_urls() {
        assert_eq!(
            with_database("postgres://u:p@h:5432/app", "s"),
            "postgres://u:p@h:5432/s"
        );
        assert_eq!(
            with_database("postgres://h/app?sslmode=require", "s"),
            "postgres://h/s?sslmode=require"
        );
    }
}

/// What `db push` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// The database already matches the schema.
    UpToDate,
    /// The changes were applied.
    Applied {
        /// The SQL that ran.
        sql: String,
        /// Data-loss warnings that were accepted.
        warnings: Vec<Finding>,
    },
    /// Data would be lost; run again with `accept_data_loss`.
    NeedsConfirmation(Vec<Finding>),
    /// A step cannot run on this data.
    Blocked(Vec<Finding>),
}

/// `db push`: change the database to match the schema without writing a migration, in one
/// transaction. For prototyping; the migration history is not touched.
///
/// # Errors
/// When the database cannot be read or refuses a step.
pub async fn push(project: &Project, accept_data_loss: bool) -> Result<PushOutcome, MigrateError> {
    let pool = PgPool::connect(&project.database_url).await?;
    let live = describe(&pool, &project.schemas()).await?;
    let steps = diff(
        &live,
        &calculate(&project.schema),
        &renames(&project.schema),
    );
    if steps.is_empty() {
        pool.close().await;
        return Ok(PushOutcome::UpToDate);
    }
    let findings = check::against(&pool, &steps).await?;
    if findings
        .iter()
        .any(|f| f.severity == Severity::Unexecutable)
    {
        pool.close().await;
        return Ok(PushOutcome::Blocked(findings));
    }
    if !findings.is_empty() && !accept_data_loss {
        pool.close().await;
        return Ok(PushOutcome::NeedsConfirmation(findings));
    }
    let sql = script(&steps, &[]);
    let mut tx = pool.begin().await?;
    // the script is rendered from the checked schema, every name quoted
    sqlx::raw_sql(AssertSqlSafe(sql.clone()))
        .execute(&mut *tx)
        .await
        .map_err(|e| MigrateError::new(format!("the database refused the change: {e}")))?;
    tx.commit().await?;
    pool.close().await;
    Ok(PushOutcome::Applied {
        sql,
        warnings: findings,
    })
}

/// The SQL that takes the database (or an empty one, when `from_empty`) to the schema.
///
/// # Errors
/// When the database cannot be read.
pub async fn diff_sql(project: &Project, from_empty: bool) -> Result<String, MigrateError> {
    let live = if from_empty {
        Shape::default()
    } else {
        let pool = PgPool::connect(&project.database_url).await?;
        let shape = describe(&pool, &project.schemas()).await?;
        pool.close().await;
        shape
    };
    let steps = diff(
        &live,
        &calculate(&project.schema),
        &renames(&project.schema),
    );
    let warnings: Vec<String> = check::pure(&steps).into_iter().map(|f| f.message).collect();
    Ok(script(&steps, &warnings))
}

/// How `migrate resolve` marks a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// It was applied by hand; record it as applied.
    Applied,
    /// It was undone by hand; record it as rolled back so it runs again.
    RolledBack,
}

/// `migrate resolve`: record a migration as applied or rolled back without running it.
///
/// # Errors
/// When the migration is not in the folder, or (for rolled back) has not failed.
pub async fn resolve(project: &Project, name: &str, how: Resolution) -> Result<(), MigrateError> {
    let local = history::read_dir(&project.migrations_dir)?;
    let migration = local.iter().find(|m| m.name == name).ok_or_else(|| {
        MigrateError::new(format!("there is no migration `{name}` in the folder"))
    })?;
    let mut conn = connect(&project.database_url).await?;
    history::ensure_table(&mut conn).await?;
    let state = compare(&local, &history::applied(&mut conn).await?);
    match how {
        Resolution::RolledBack => {
            if !state.failed.iter().any(|f| f == name) {
                return Err(MigrateError::new(format!(
                    "`{name}` has not failed; only a failed migration can be marked rolled back"
                )));
            }
            sqlx::query(
                "update _prisma_migrations set rolled_back_at = now() \
                 where migration_name = $1 and finished_at is null and rolled_back_at is null",
            )
            .bind(name)
            .execute(&mut conn)
            .await?;
        }
        Resolution::Applied => {
            if state.applied.iter().any(|a| a == name) {
                return Err(MigrateError::new(format!("`{name}` is already applied")));
            }
            sqlx::query(
                "update _prisma_migrations set rolled_back_at = now() \
                 where migration_name = $1 and finished_at is null and rolled_back_at is null",
            )
            .bind(name)
            .execute(&mut conn)
            .await?;
            sqlx::query(
                "insert into _prisma_migrations (id, checksum, migration_name, started_at, finished_at, applied_steps_count) \
                 values (gen_random_uuid()::text, $1, $2, now(), now(), 0)",
            )
            .bind(migration.checksum())
            .bind(name)
            .execute(&mut conn)
            .await?;
        }
    }
    Ok(())
}

/// `db pull` on Postgres: read the database at `url` and write the schema for it over `file`
/// (see [`crate::postgres::pull::pull`]).
///
/// # Errors
/// When the database cannot be reached or read, or the schema file has errors.
pub async fn db_pull(
    url: &str,
    file: &lumen_schema::SourceFile,
    force: bool,
) -> Result<crate::postgres::pull::Pulled, MigrateError> {
    use crate::postgres::pull;
    drop(connect(url).await?);
    let pool = PgPool::connect(url).await?;
    let db = pull::Database::read(&pool, &pull::schemas_of(file)).await;
    pool.close().await;
    pull::pull(&db?, file, force)
}
