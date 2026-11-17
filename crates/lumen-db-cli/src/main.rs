//! `lumen`: one schema file, migrations and a type-safe client for Rust.

mod cmd;
mod project;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

/// One schema file, migrations and a type-safe client for Rust.
#[derive(Parser)]
#[command(name = "lumen", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The database `init` sets up.
#[derive(Clone, Copy, ValueEnum)]
enum InitProvider {
    /// PostgreSQL.
    Postgresql,
    /// MongoDB.
    Mongodb,
}

#[derive(Subcommand)]
enum Command {
    /// Start a project: prisma/schema.prisma, .env and the migrations folder.
    Init {
        /// The database.
        #[arg(long, value_enum, default_value = "postgresql")]
        provider: InitProvider,
        /// The `DATABASE_URL` to write to .env.
        #[arg(long)]
        url: Option<String>,
    },
    /// Format the schema in place.
    Format {
        /// The schema file.
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Only check; exit 1 when the file is not formatted.
        #[arg(long)]
        check: bool,
    },
    /// Check the schema and print its problems.
    Validate {
        /// The schema file.
        #[arg(long)]
        schema: Option<PathBuf>,
    },
    /// Write the Rust client for the schema.
    Generate {
        /// The schema file.
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Where to write it (else the generator's `output`, else src/db.rs).
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Migrations (PostgreSQL).
    Migrate {
        #[command(subcommand)]
        command: Migrate,
    },
    /// Change the database without migration files.
    Db {
        #[command(subcommand)]
        command: Db,
    },
}

#[derive(Subcommand)]
enum Migrate {
    /// Write a migration for the schema's changes and apply it. Development only.
    Dev {
        /// The schema file.
        #[arg(long)]
        schema: Option<PathBuf>,
        /// The migration's name.
        #[arg(long, short)]
        name: Option<String>,
        /// Write the migration without applying it.
        #[arg(long)]
        create_only: bool,
        /// Go ahead although data may be lost.
        #[arg(long)]
        accept_data_loss: bool,
        /// Do not run `generate` afterwards.
        #[arg(long)]
        skip_generate: bool,
    },
    /// Apply pending migrations. Safe for production.
    Deploy {
        /// The schema file.
        #[arg(long)]
        schema: Option<PathBuf>,
    },
    /// Show applied, pending and failed migrations; exit 1 unless up to date.
    Status {
        /// The schema file.
        #[arg(long)]
        schema: Option<PathBuf>,
    },
    /// Drop everything and apply all migrations again. Development only.
    Reset {
        /// The schema file.
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Do not ask.
        #[arg(long, short)]
        force: bool,
        /// Do not run `generate` afterwards.
        #[arg(long)]
        skip_generate: bool,
    },
    /// Print the SQL from the database (or nothing) to the schema.
    Diff {
        /// The schema file.
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Start from an empty database instead of the real one.
        #[arg(long)]
        from_empty: bool,
    },
    /// Record a failed migration as applied or rolled back.
    Resolve {
        /// The schema file.
        #[arg(long)]
        schema: Option<PathBuf>,
        /// The migration was finished by hand.
        #[arg(
            long,
            conflicts_with = "rolled_back",
            required_unless_present = "rolled_back"
        )]
        applied: Option<String>,
        /// The migration was undone by hand; it runs again on the next deploy.
        #[arg(long)]
        rolled_back: Option<String>,
    },
}

#[derive(Subcommand)]
enum Db {
    /// Make the database match the schema (tables or collections and indexes).
    Push {
        /// The schema file.
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Go ahead although data may be lost.
        #[arg(long)]
        accept_data_loss: bool,
        /// Do not run `generate` afterwards.
        #[arg(long)]
        skip_generate: bool,
    },
    /// Write a schema from the database (MongoDB: by sampling documents).
    Pull {
        /// The schema file.
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Print the schema instead of writing it.
        #[arg(long)]
        print: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cannot start: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli.command)) {
        Ok(code) => code,
        Err(message) => {
            // schema problems come rendered, each with its own `error:`
            if message.starts_with("error") {
                eprintln!("{message}");
            } else {
                eprintln!("error: {message}");
            }
            ExitCode::FAILURE
        }
    }
}

async fn run(command: Command) -> Result<ExitCode, project::Fail> {
    match command {
        Command::Init { provider, url } => {
            cmd::init(matches!(provider, InitProvider::Mongodb), url)
        }
        Command::Format { schema, check } => cmd::format(schema.as_deref(), check),
        Command::Validate { schema } => cmd::validate(schema.as_deref()),
        Command::Generate { schema, output } => cmd::generate(schema.as_deref(), output.as_deref()),
        Command::Migrate { command } => match command {
            Migrate::Dev {
                schema,
                name,
                create_only,
                accept_data_loss,
                skip_generate,
            } => {
                let options = cmd::DevArgs {
                    name,
                    create_only,
                    accept_data_loss,
                    skip_generate,
                };
                cmd::migrate_dev(schema.as_deref(), options).await
            }
            Migrate::Deploy { schema } => cmd::migrate_deploy(schema.as_deref()).await,
            Migrate::Status { schema } => cmd::migrate_status(schema.as_deref()).await,
            Migrate::Reset {
                schema,
                force,
                skip_generate,
            } => cmd::migrate_reset(schema.as_deref(), force, skip_generate).await,
            Migrate::Diff { schema, from_empty } => {
                cmd::migrate_diff(schema.as_deref(), from_empty).await
            }
            Migrate::Resolve {
                schema,
                applied,
                rolled_back,
            } => cmd::migrate_resolve(schema.as_deref(), applied, rolled_back).await,
        },
        Command::Db { command } => match command {
            Db::Push {
                schema,
                accept_data_loss,
                skip_generate,
            } => cmd::db_push(schema.as_deref(), accept_data_loss, skip_generate).await,
            Db::Pull { schema, print } => cmd::db_pull(schema.as_deref(), print).await,
        },
    }
}
