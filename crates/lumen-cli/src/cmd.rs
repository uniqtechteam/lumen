//! What each command does. Everything a command prints for people goes to stderr except the
//! things meant to be piped (`migrate diff`, `db pull --print`).

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lumen_migrate::check::{Finding, Severity};
use lumen_migrate::commands::{self, DevOptions, DevOutcome, Project, PushOutcome, Resolution};
use lumen_migrate::{history, mongo, mongo_pull};
use lumen_schema::SourceFile;
use lumen_schema::checked::{Provider, Schema, StringOrEnv};

use crate::project::{self, Env, Fail};

const POSTGRES_SCHEMA: &str = r#"// The schema: models, enums and the database. Run `lumen migrate dev` after a change.

datasource db {
  provider = "postgresql"
  url      = env("DATABASE_URL")
}

generator client {
  provider = "lumen"
  output   = "../src/db.rs"
}

model User {
  id        Int      @id @default(autoincrement())
  email     String   @unique
  name      String?
  createdAt DateTime @default(now())
}
"#;

const MONGO_SCHEMA: &str = r#"// The schema: models, types and the database. Run `lumen db push` after a change.

datasource db {
  provider = "mongodb"
  url      = env("DATABASE_URL")
}

generator client {
  provider = "lumen"
  output   = "../src/db.rs"
}

model User {
  id        String   @id @default(auto()) @map("_id") @db.ObjectId
  email     String   @unique
  name      String?
  createdAt DateTime @default(now())
}
"#;

/// `lumen init`.
pub fn init(mongo: bool, url: Option<String>) -> Result<ExitCode, Fail> {
    let schema = Path::new("prisma/schema.prisma");
    if schema.exists() || Path::new("schema.prisma").exists() {
        return Err("a schema.prisma is already here; nothing was changed".to_owned());
    }
    std::fs::create_dir_all("prisma").map_err(|e| format!("cannot create prisma/: {e}"))?;
    let text = if mongo { MONGO_SCHEMA } else { POSTGRES_SCHEMA };
    std::fs::write(schema, text).map_err(|e| format!("cannot write {}: {e}", schema.display()))?;
    if !mongo {
        std::fs::create_dir_all("prisma/migrations")
            .map_err(|e| format!("cannot create prisma/migrations: {e}"))?;
    }
    let url = url.unwrap_or_else(|| {
        if mongo {
            "mongodb://localhost:27017/app?directConnection=true".to_owned()
        } else {
            "postgresql://postgres:postgres@localhost:5432/app".to_owned()
        }
    });
    let env = Path::new(".env");
    let existing = std::fs::read_to_string(env).unwrap_or_default();
    if existing
        .lines()
        .any(|l| l.trim_start().starts_with("DATABASE_URL"))
    {
        eprintln!(".env already sets DATABASE_URL; left as it is");
    } else {
        let mut text = existing;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("DATABASE_URL=\"");
        text.push_str(&url);
        text.push_str("\"\n");
        std::fs::write(env, text).map_err(|e| format!("cannot write .env: {e}"))?;
    }
    let next = if mongo {
        "lumen db push"
    } else {
        "lumen migrate dev --name init"
    };
    eprintln!(
        "Created prisma/schema.prisma and DATABASE_URL in .env.\n\nNext:\n  1. Point DATABASE_URL at your database.\n  2. Add your models to prisma/schema.prisma.\n  3. Run `{next}`, then use the client in src/db.rs."
    );
    Ok(ExitCode::SUCCESS)
}

/// `lumen format`.
pub fn format(given: Option<&Path>, check: bool) -> Result<ExitCode, Fail> {
    let path = project::schema_path(given)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = SourceFile::new(path.display().to_string(), text.clone());
    let (formatted, diagnostics) = lumen_schema::format(&file);
    if diagnostics.has_errors() {
        return Err(diagnostics.render(&file).trim_end().to_owned());
    }
    if formatted == text {
        eprintln!("{} is formatted", path.display());
        return Ok(ExitCode::SUCCESS);
    }
    if check {
        eprintln!("{} is not formatted; run `lumen format`", path.display());
        return Ok(ExitCode::FAILURE);
    }
    std::fs::write(&path, formatted)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    eprintln!("formatted {}", path.display());
    Ok(ExitCode::SUCCESS)
}

/// `lumen validate`.
pub fn validate(given: Option<&Path>) -> Result<ExitCode, Fail> {
    let path = project::schema_path(given)?;
    project::load(&path)?;
    eprintln!("{} is valid", path.display());
    Ok(ExitCode::SUCCESS)
}

/// Where `generate` writes: `--output`, else the generator's `output` (next to the schema), else
/// `src/db.rs`.
fn output_path(schema_path: &Path, schema: &Schema, given: Option<&Path>) -> PathBuf {
    if let Some(p) = given {
        return p.to_path_buf();
    }
    let from_schema = schema
        .generators
        .iter()
        .find(|g| matches!(&g.provider, Some(StringOrEnv::Literal(p)) if p.starts_with("lumen")))
        .and_then(|g| g.output.as_ref());
    match from_schema {
        Some(out) => schema_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(out),
        None => PathBuf::from("src/db.rs"),
    }
}

/// `lumen generate`.
pub fn generate(given: Option<&Path>, output: Option<&Path>) -> Result<ExitCode, Fail> {
    let path = project::schema_path(given)?;
    let (_, schema) = project::load(&path)?;
    write_client(&path, &schema, output)?;
    Ok(ExitCode::SUCCESS)
}

fn write_client(path: &Path, schema: &Schema, output: Option<&Path>) -> Result<(), Fail> {
    let out = output_path(path, schema, output);
    let shown = path.display().to_string().replace('\\', "/");
    let code = lumen_codegen::generate(schema, &shown)?;
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    if std::fs::read_to_string(&out).is_ok_and(|old| old == code) {
        eprintln!("{} is up to date", out.display());
        return Ok(());
    }
    std::fs::write(&out, code).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    eprintln!("wrote the client to {}", out.display());
    Ok(())
}

/// The project a Postgres command works on.
fn postgres(given: Option<&Path>) -> Result<(PathBuf, Project), Fail> {
    let path = project::schema_path(given)?;
    let (_, schema) = project::load(&path)?;
    if schema.provider() != Provider::Postgres {
        return Err("migrations are for PostgreSQL; with MongoDB use `lumen db push`".to_owned());
    }
    let env = Env::load(&path);
    let project = Project {
        database_url: project::database_url(&schema, &env)?,
        shadow_database_url: project::shadow_url(&schema, &env),
        migrations_dir: history::default_dir(&path),
        schema,
    };
    Ok((path, project))
}

fn print_findings(findings: &[Finding]) {
    for f in findings {
        let mark = if f.severity == Severity::Unexecutable {
            "cannot run"
        } else {
            "warning"
        };
        eprintln!("  - {mark}: {}", f.message);
    }
}

/// Ask a yes/no question when someone is at the terminal; `false` otherwise.
fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim(), "y" | "Y" | "yes" | "Yes")
}

fn listed(title: &str, names: &[String]) {
    if !names.is_empty() {
        eprintln!("{title}:");
        for n in names {
            eprintln!("  {n}");
        }
    }
}

/// `lumen migrate dev` options.
pub struct DevArgs {
    /// The migration's name.
    pub name: Option<String>,
    /// Write without applying.
    pub create_only: bool,
    /// Accept data loss without asking.
    pub accept_data_loss: bool,
    /// Skip `generate`.
    pub skip_generate: bool,
}

/// `lumen migrate dev`.
pub async fn migrate_dev(given: Option<&Path>, args: DevArgs) -> Result<ExitCode, Fail> {
    let (path, project) = postgres(given)?;
    let mut options = DevOptions {
        name: args.name.unwrap_or_else(|| "migration".to_owned()),
        create_only: args.create_only,
        accept_data_loss: args.accept_data_loss,
        now: None,
    };
    let mut outcome = commands::dev(&project, &options)
        .await
        .map_err(|e| e.message)?;
    if let DevOutcome::NeedsConfirmation(findings) = &outcome {
        eprintln!("This migration may lose data:");
        print_findings(findings);
        if !confirm("Go ahead?") {
            eprintln!("Nothing was changed. Run again with --accept-data-loss to go ahead.");
            return Ok(ExitCode::FAILURE);
        }
        options.accept_data_loss = true;
        outcome = commands::dev(&project, &options)
            .await
            .map_err(|e| e.message)?;
    }
    match outcome {
        DevOutcome::UpToDate { applied } => {
            listed("Applied", &applied);
            eprintln!("The database is in sync with the schema.");
        }
        DevOutcome::Created {
            applied,
            folder,
            ran,
            warnings,
            ..
        } => {
            listed("Applied", &applied);
            print_findings(&warnings);
            let dir = project.migrations_dir.join(&folder);
            if ran {
                eprintln!("Created and applied {}", dir.display());
            } else {
                eprintln!(
                    "Created {}; edit it if needed, then run `lumen migrate dev`",
                    dir.display()
                );
            }
        }
        DevOutcome::NeedsConfirmation(_) => return Ok(ExitCode::FAILURE),
        DevOutcome::Blocked(findings) => {
            eprintln!("The change cannot run on the data in the database:");
            print_findings(&findings);
            return Ok(ExitCode::FAILURE);
        }
        DevOutcome::NeedsReset(reason) => {
            eprintln!("{reason}\n\nTo start over (this deletes all data): lumen migrate reset");
            return Ok(ExitCode::FAILURE);
        }
    }
    if !args.skip_generate && !args.create_only {
        write_client(&path, &project.schema, None)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `lumen migrate deploy`.
pub async fn migrate_deploy(given: Option<&Path>) -> Result<ExitCode, Fail> {
    let (_, project) = postgres(given)?;
    let applied = commands::deploy(&project).await.map_err(|e| e.message)?;
    if applied.is_empty() {
        eprintln!("No pending migrations.");
    } else {
        listed("Applied", &applied);
    }
    Ok(ExitCode::SUCCESS)
}

/// `lumen migrate status`.
pub async fn migrate_status(given: Option<&Path>) -> Result<ExitCode, Fail> {
    let (_, project) = postgres(given)?;
    let s = commands::status(&project).await.map_err(|e| e.message)?;
    eprintln!("{} migrations applied", s.applied.len());
    listed("Not applied yet", &s.pending);
    listed("Failed (run `lumen migrate resolve`)", &s.failed);
    listed("Changed after they were applied", &s.edited);
    listed("Applied but missing from the folder", &s.missing);
    let clean =
        s.pending.is_empty() && s.failed.is_empty() && s.edited.is_empty() && s.missing.is_empty();
    if clean {
        eprintln!("The database is up to date.");
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// `lumen migrate reset`.
pub async fn migrate_reset(
    given: Option<&Path>,
    force: bool,
    skip_generate: bool,
) -> Result<ExitCode, Fail> {
    let (path, project) = postgres(given)?;
    if !force && !confirm("This drops every table and all data in the database. Go ahead?") {
        eprintln!("Nothing was changed. Pass --force to reset without asking.");
        return Ok(ExitCode::FAILURE);
    }
    let applied = commands::reset(&project).await.map_err(|e| e.message)?;
    eprintln!("The database was reset.");
    listed("Applied", &applied);
    if !skip_generate {
        write_client(&path, &project.schema, None)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `lumen migrate diff`.
pub async fn migrate_diff(given: Option<&Path>, from_empty: bool) -> Result<ExitCode, Fail> {
    let path = project::schema_path(given)?;
    let (_, schema) = project::load(&path)?;
    if schema.provider() != Provider::Postgres {
        return Err("`migrate diff` is for PostgreSQL".to_owned());
    }
    let database_url = if from_empty {
        String::new()
    } else {
        project::database_url(&schema, &Env::load(&path))?
    };
    let project = Project {
        database_url,
        shadow_database_url: None,
        migrations_dir: history::default_dir(&path),
        schema,
    };
    let sql = commands::diff_sql(&project, from_empty)
        .await
        .map_err(|e| e.message)?;
    if sql.is_empty() {
        eprintln!("No difference.");
    } else {
        print!("{sql}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `lumen migrate resolve`.
pub async fn migrate_resolve(
    given: Option<&Path>,
    applied: Option<String>,
    rolled_back: Option<String>,
) -> Result<ExitCode, Fail> {
    let (_, project) = postgres(given)?;
    let (name, how) = match (applied, rolled_back) {
        (Some(n), _) => (n, Resolution::Applied),
        (None, Some(n)) => (n, Resolution::RolledBack),
        (None, None) => return Err("pass --applied <name> or --rolled-back <name>".to_owned()),
    };
    commands::resolve(&project, &name, how)
        .await
        .map_err(|e| e.message)?;
    let word = if how == Resolution::Applied {
        "applied"
    } else {
        "rolled back"
    };
    eprintln!("Marked `{name}` as {word}.");
    Ok(ExitCode::SUCCESS)
}

async fn mongo_database(schema: &Schema, path: &Path) -> Result<mongodb::Database, Fail> {
    let url = project::database_url(schema, &Env::load(path))?;
    let name = project::mongo_database(&url)?;
    let client = mongodb::Client::with_uri_str(&url)
        .await
        .map_err(|e| format!("cannot connect to MongoDB: {e}"))?;
    Ok(client.database(&name))
}

/// `lumen db push`.
pub async fn db_push(
    given: Option<&Path>,
    accept_data_loss: bool,
    skip_generate: bool,
) -> Result<ExitCode, Fail> {
    let path = project::schema_path(given)?;
    let (_, schema) = project::load(&path)?;
    if schema.provider() == Provider::MongoDb {
        let db = mongo_database(&schema, &path).await?;
        let steps = mongo::push(&db, &schema).await.map_err(|e| e.message)?;
        if steps.is_empty() {
            eprintln!("The database is in sync with the schema.");
        }
        for step in &steps {
            match step {
                mongo::MongoStep::CreateCollection(c) => eprintln!("  created collection {c}"),
                mongo::MongoStep::DropIndex(c, i) => eprintln!("  dropped index {i} on {c}"),
                mongo::MongoStep::CreateIndex(c, i) => eprintln!("  created index {i} on {c}"),
            }
        }
    } else {
        let (_, project) = postgres(Some(&path))?;
        let mut outcome = commands::push(&project, accept_data_loss)
            .await
            .map_err(|e| e.message)?;
        if let PushOutcome::NeedsConfirmation(findings) = &outcome {
            eprintln!("This change may lose data:");
            print_findings(findings);
            if !confirm("Go ahead?") {
                eprintln!("Nothing was changed. Run again with --accept-data-loss to go ahead.");
                return Ok(ExitCode::FAILURE);
            }
            outcome = commands::push(&project, true)
                .await
                .map_err(|e| e.message)?;
        }
        match outcome {
            PushOutcome::UpToDate => eprintln!("The database is in sync with the schema."),
            PushOutcome::Applied { warnings, .. } => {
                print_findings(&warnings);
                eprintln!("The database now matches the schema.");
            }
            PushOutcome::NeedsConfirmation(_) => return Ok(ExitCode::FAILURE),
            PushOutcome::Blocked(findings) => {
                eprintln!("The change cannot run on the data in the database:");
                print_findings(&findings);
                return Ok(ExitCode::FAILURE);
            }
        }
    }
    if !skip_generate {
        write_client(&path, &schema, None)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The generator blocks of the schema at `path`, as text, to keep across `db pull`.
fn kept_generators(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return "\ngenerator client {\n  provider = \"lumen\"\n  output   = \"../src/db.rs\"\n}\n"
            .to_owned();
    };
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim_start().starts_with("generator ") {
            inside = true;
            out.push('\n');
        }
        if inside {
            out.push_str(line);
            out.push('\n');
            if line.trim() == "}" {
                inside = false;
            }
        }
    }
    out
}

/// `lumen db pull`.
pub async fn db_pull(given: Option<&Path>, print: bool) -> Result<ExitCode, Fail> {
    let path = match project::schema_path(given) {
        Ok(p) => p,
        Err(e) => {
            return Err(format!(
                "{e}\n(db pull needs a schema with the datasource to read from)"
            ));
        }
    };
    let (_, schema) = project::load(&path)?;
    if schema.provider() != Provider::MongoDb {
        return Err(
            "`db pull` for PostgreSQL is coming in 0.2; for now it reads MongoDB only".to_owned(),
        );
    }
    let db = mongo_database(&schema, &path).await?;
    let pulled = mongo_pull::pull(&db).await.map_err(|e| e.message)?;
    let (head, rest) = pulled.split_once("}\n").unwrap_or((&pulled, ""));
    let text = format!("{head}}}\n{}{rest}", kept_generators(&path));
    let file = SourceFile::new(path.display().to_string(), text.clone());
    let (formatted, diagnostics) = lumen_schema::format(&file);
    let formatted = if diagnostics.has_errors() {
        text
    } else {
        formatted
    };
    if print {
        print!("{formatted}");
    } else {
        std::fs::write(&path, formatted)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        eprintln!(
            "wrote {} from the database; check it, then run `lumen generate`",
            path.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}
