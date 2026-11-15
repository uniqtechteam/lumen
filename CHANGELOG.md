# Changelog

All notable changes to lumen are written down here, for people who use it.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Project setup: the `lumen-orm`, `lumen-schema`, `lumen-migrate`, `lumen-codegen` and `lumen-cli` crates,
  and CI that tests against Postgres 16 and MongoDB 7.
- `lumen_schema::parse`: reads `schema.prisma` files (models, views, composite types, enums,
  datasource and generator blocks, field types with `?`/`[]`/`Unsupported("..")`, `@` and `@@`
  attributes with arguments, arrays, functions and `{ .. }` objects) into a syntax tree with spans.
  `//`, `///`, `/* */` and `/** */` comments are kept. A broken line becomes one diagnostic and the rest
  of the file still parses; diagnostics print the source line with the problem underlined and a hint.
- `lumen_schema::check`: the checked schema (models, views, enums, composite types, datasource,
  generators, indexes, defaults, native types and paired relations) with Prisma's rules. Clear
  messages with "did you mean" hints for unknown types, attributes and native types; a missing
  opposite relation field names both models and the field to add. `SetNull` on a required foreign
  key is a warning, as in Prisma. `// lumen: renamed from old` comments are kept for migrations.
- `lumen_schema::format`: the canonical layout (the same as Prisma's): aligned field and setting
  columns, attributes in canonical order, block attributes after the fields, canonical spacing in
  values, comments kept exactly. It adds the missing side of a one-sided relation (with its foreign
  key field when needed), and formats around broken lines without disturbing the rest.
- `lumen_migrate::Shape`, `calculate` and `postgres::describe`: the shape of a database (tables,
  columns with `format_type()` types, normalised defaults, primary keys, indexes, foreign keys,
  enums), calculated from a checked schema with Prisma's rules (types, constraint names, default
  referential actions, `_AToB` join tables) or read from a live Postgres through `pg_catalog`.
- `lumen_migrate::diff`, `postgres::script` and `check`: the ordered steps from one shape to
  another, rendered as Postgres SQL in the style of Prisma's migration files. Renames from
  `// lumen: renamed from` hints are real renames (tables, columns, and the keys and indexes whose
  names follow them). Safe type changes run in place; casts that cannot work drop and add the column
  with a warning; removing enum values rebuilds the type. Destructive and impossible steps are found
  first, worded without a database for the file header or with real counts against one.
- `lumen_migrate::commands`: `dev`, `deploy`, `status` and `reset`, compatible with Prisma Migrate.
  Same folder layout (`YYYYMMDDHHMMSS_name/migration.sql`, `migration_lock.toml`), same
  `_prisma_migrations` table and checksums (CRLF-tolerant), same advisory lock during deploy, so
  lumen and Prisma can take turns on one project. `dev` replays the migrations on a shadow database
  (cached by checksums), reports drift and edited or failed migrations, asks before data loss,
  refuses steps that cannot run, and writes warnings into the migration file. `deploy` never needs
  a shadow database and records failures.
- Tested against the real Prisma CLI (6.19.3) in both directions.
- `lumen-orm`: a query builder for Postgres (select with filters, ordering, paging, `DISTINCT ON`
  and counts; insert of many rows with `RETURNING`; upsert with `ON CONFLICT`; update; delete; raw
  queries). Values are always parameters, cast where Postgres needs it (`uuid`, `numeric`, enums);
  NULL is written as a literal. Prisma's operators, including case-insensitive matching and `not`
  keeping NULL rows. Errors carry Prisma's codes (P2002, P2003, P2011, P2025, ...) and name the
  fields behind a constraint.
- Relations without N+1: `load::load_many`, `load_one` and `load_many_composite` load related rows
  for any number of parents in one statement per level (`= ANY($1)` with one array parameter;
  composite keys in chunks under the parameter limit), with `take` per parent done in SQL.
- Transactions: `db.transaction(async |tx| ..)` commits on `Ok` and rolls back on `Err`; nested
  `tx.transaction(..)` uses a savepoint; `transaction_with(Isolation::Serializable, ..)`. The `Run`
  trait lets the same code work on the pool and in a transaction. `db.statements()` counts statements.
- `lumen_codegen::generate`: the type-safe client as one plain Rust file. Per model: the record
  (serde, hand-written row decoding, no derive macros needed in your crate), typed filter functions
  per field (`user::email::contains(..)`; a wrong value type does not compile), ordering,
  `Create` (required fields are plain fields) and `Update` (`Set::To` / `Set::NotSet`) inputs,
  `UniqueWhere` from the schema's unique keys, `find_many` / `find_unique` / `find_first`,
  `create` / `create_many` / `update` / `update_many` / `upsert` / `delete` / `delete_many` /
  `count`, and `include_<relation>()` for one-to-one and one-to-many (one more query for all
  records; a required to-one is not an `Option`). Enums become Rust enums. `@updatedAt` and
  `uuid()` are filled by the client. Errors name fields through the schema's constraint names.
  Everything runs on the pool or inside a transaction. The output is byte-for-byte stable.
