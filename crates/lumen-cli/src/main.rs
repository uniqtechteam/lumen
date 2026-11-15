//! `lumen`: a Prisma-style schema, migrations and client for Rust.

use clap::Parser;

/// A Prisma-style schema, migrations and a type-safe client for Rust.
#[derive(Parser)]
#[command(name = "lumen", version, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
