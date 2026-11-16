# lumen

lumen is a Prisma-style data layer for Rust. You describe your data in a `schema.prisma` file, and
lumen writes the migrations and a type-safe client for it. Under the client sits a query builder
for the queries a generated client cannot express. It works with Postgres and MongoDB.

It reads the same `schema.prisma` files Prisma reads and keeps migrations in the same folders and
history table. A project can switch between the two tools, in either direction.

**Status:** 0.1 in development, not on crates.io yet.

```console
$ lumen init
Created prisma/schema.prisma and DATABASE_URL in .env.

$ lumen migrate dev --name init
Created and applied prisma/migrations/20261115091525_init
wrote the client to src/db.rs

$ cat prisma/migrations/20261115091525_init/migration.sql
-- CreateTable
CREATE TABLE "User" (
    "id" SERIAL NOT NULL,
    "email" TEXT NOT NULL,
    "name" TEXT,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "User_pkey" PRIMARY KEY ("id")
);

-- CreateIndex
CREATE UNIQUE INDEX "User_email_key" ON "User"("email");
```

That is the SQL Prisma writes for the same model, down to the constraint names.

## Install

```bash
cargo install lumen-cli
```

Release binaries for Linux, macOS and Windows are attached to each GitHub release. The same program
is also installed as `lumen-db`, for machines that already have a different `lumen` command.

## Quick start

```bash
lumen init                      # prisma/schema.prisma, .env, prisma/migrations
# point DATABASE_URL in .env at your database, add your models, then:
lumen migrate dev --name init   # writes and applies a migration, then runs `lumen generate`
```

Add `lumen-orm` to your crate and include the generated file. With MongoDB, turn on the `mongodb`
feature and run `lumen db push` instead of `migrate dev`.

```rust
#[rustfmt::skip]
mod db;

use db::{Client, post, user};

let client = Client::connect(&std::env::var("DATABASE_URL")?).await?;

let writers = client
    .user()
    .find_many()
    .filter(user::email::ends_with("@example.com"))
    .order_by(user::id::desc())
    .include_posts()
    .exec()
    .await?; // Vec<user::WithPosts>: each user with its posts, in two statements

let result = client
    .db()
    .transaction(async |tx| {
        let ana = user::on(&mut *tx).create(new_user).await?;
        post::on(&mut *tx).create(first_post(ana.id)).await?;
        Ok(ana)
    })
    .await; // both rows, or neither
```

Errors carry Prisma's codes. For example, `err.code == "P2002"` is a duplicate, and `err.fields`
names the columns. `err.http_status()` gives the status a web API should answer with.

## How it works

```mermaid
flowchart LR
    S[schema.prisma] --> P[lumen-schema<br/>parse, check, format]
    P --> C[lumen-migrate<br/>calculate]
    DB[(Postgres)] --> D[describe]
    C --> F{diff}
    D --> F
    F --> K[check<br/>counts rows first]
    K --> M[migrations/..._name/<br/>migration.sql]
    M --> H[_prisma_migrations]
    P --> G[lumen-codegen]
    G --> R[src/db.rs]
    R --> O[lumen-orm<br/>queries, includes, transactions]
    O --> DB
```

*`migrate dev` diffs the schema against the real database, not against the last migration. Before a
destructive step it counts the rows that step would touch, so it only warns when data would really
be lost.*

The generated client is a plain Rust file, not a macro. You can read it, grep it, and
rust-analyzer sees every type. It calls `lumen-orm`, which you can also use directly for dynamic
queries.

## Relations without N+1

An include loads each level with one statement that has `IN (..)` over the keys, however many
parents there are. Here is the same page of users with their posts, loaded by `include_posts()` and by
a hand-written loop:

![Median time and statements: include_posts() sends 2 statements for 10, 100 and 1000 users; a loop sends 11, 101 and 1001.](docs/img/includes.svg)

| Users (5 posts each) | `include_posts()` | A query per user |
| --- | --- | --- |
| 10 | 2 statements, 0.45 ms | 11 statements, 0.84 ms |
| 100 | 2 statements, 0.38 ms | 101 statements, 3.60 ms |
| 1,000 | 2 statements, 3.24 ms | 1,001 statements, 67.95 ms |

Measured with `cargo run --release -p lumen-codegen --example bench_includes` and drawn with
`python3 bench/plot.py`, on an i7-12700 against Postgres 16 in Docker on the same machine (median
of 21 runs, 2026-11-15). The statement counts are exact. The times move by a few milliseconds
between runs. Over a real network each statement costs a round trip, so the loop falls further
behind.

## What it does

| | Postgres | MongoDB |
| --- | --- | --- |
| Schema: parse, check with "did you mean" hints, `format` like `prisma format` | yes | yes |
| `migrate dev / deploy / status / reset / diff / resolve` | yes | (uses `db push`) |
| `db push` | tables, columns, indexes, foreign keys, enums | collections and indexes |
| `db pull` | 0.2 | by sampling documents |
| Generated client: filters, ordering, paging, create, update, upsert, delete, count | yes | yes |
| Includes, one statement per level | yes | yes (`$in`) |
| Transactions | yes, with isolation levels | yes, on a replica set |
| Errors with Prisma codes (P2002, P2003, P2025, ...) | yes | yes |
| Query builder for dynamic queries | yes | yes |

## Working with Prisma

- Migrations use Prisma's layout (`YYYYMMDDHHMMSS_name/migration.sql`, `migration_lock.toml`) and
  its `_prisma_migrations` table, checksums and advisory lock.
- CI checks this against the real Prisma 6.19.3 CLI in both directions. A Prisma project carries on
  with `lumen migrate dev`, and a lumen project carries on with `prisma migrate dev`. Neither sees
  drift.
- `lumen generate` uses the generator whose provider is `lumen`. Other generator blocks, such as
  `prisma-client-js`, are left alone.

See [docs/prisma-users.md](docs/prisma-users.md) for what maps to what.

## Configuration

- The schema is `--schema`, else `./schema.prisma`, else `./prisma/schema.prisma`.
- `DATABASE_URL` (or whatever `env("..")` the datasource names) comes from the shell first, then
  from `.env` next to the schema, then from `./.env`.
- `shadowDatabaseUrl` is optional. Without it, `migrate dev` creates a temporary database and drops
  it afterwards.
- The client goes where the generator's `output` says (relative to the schema), or to `src/db.rs`.

## In CI

```bash
lumen format --check     # exit 1 when the schema is not formatted
lumen validate
lumen migrate status     # exit 1 when a migration is pending, failed, edited or missing
lumen migrate deploy     # apply pending migrations; safe for production
```

Without a terminal, a command that would lose data stops and asks you to pass
`--accept-data-loss`, so a script can't drop a column by accident.

## Limits

- No SQLite, MySQL or SQL Server. Postgres and MongoDB only.
- Not in 0.1: many-to-many includes, nested includes in the generated client (the query builder
  has them), `db pull` for Postgres, `generate --watch`.
- MongoDB has no migrations, as in Prisma. `db push` creates collections and indexes, rebuilds
  an index whose definition in the schema changed, and never drops a collection. Indexes the
  schema does not have (an `@@index` you removed, or one made by hand) are listed and left in
  place; drop them yourself when they are no longer needed.

## Contributing

Bug reports with a schema that shows the problem are the most useful. The tests run without
databases. Set `LUMEN_TEST_POSTGRES_URL` and `LUMEN_TEST_MONGO_URL` (a replica set) to run the rest.
`LUMEN_TEST_PRISMA=1` also checks interop with the Prisma CLI.

## Credits

lumen's design learns from [Prisma](https://github.com/prisma/prisma-engines) (Apache-2.0) and
[SeaORM](https://github.com/SeaQL/sea-orm) (MIT OR Apache-2.0). It is written from scratch and is not
affiliated with or endorsed by Prisma.

## Licence

MIT OR Apache-2.0, at your option.

Made by the UniqTech team.
