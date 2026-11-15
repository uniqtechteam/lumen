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
- MongoDB (`lumen-orm` feature `mongodb`): `MongoDb` with find (filters, sort, skip, limit,
  projection), count, insert, update and delete. The same `Filter` renders to a BSON query
  (`$regex` with escaping and case-insensitive mode, `$in`, `$nor` for NOT, ObjectIds) and gives
  the same results as on Postgres. Unique-index errors are P2002.
- `lumen_migrate::mongo::push`: `db push` for MongoDB, managing collections and Prisma-named
  indexes (created, rebuilt when changed, dropped when removed; collections are never dropped).
- MongoDB relations: `mongo::load_many` and `load_one` load related documents for any number of
  parents in one `$in` query per level; `take` per parent runs on the server with
  `$setWindowFields`. `MongoDb::transaction(async |tx| ..)` (replica sets) commits on `Ok` and
  aborts on `Err`; the `MongoRun` trait works on the database and inside a transaction.
- `lumen_migrate::mongo_pull::pull`: `db pull` for MongoDB samples up to 1000 documents per
  collection and suggests a formatted schema: ObjectId ids, optional fields, composite types for
  nested documents, lists, `@map` for names that are not identifiers, indexes, and a comment where
  a field holds several types.
- `lumen generate` for MongoDB: the same client API as on Postgres (`find_many().filter(..)`,
  `include_<relation>()`, `create`, `update`, `upsert`, `delete`, `count`, ...) with records that
  are serde documents, `ObjectId`, BSON dates and decimals, structs for composite types, defaults
  filled by the client (`now()`, literals, `@updatedAt`; the driver makes `_id`), relations through
  `$in` per level and everything runnable inside a transaction.
- The `lumen` command (also installed as `lumen-db`): `init` (schema, `.env` and the migrations
  folder for Postgres or MongoDB), `format [--check]`, `validate`, `generate [--output]`,
  `migrate dev|deploy|status|reset|diff|resolve` and `db push|pull`. It finds
  `schema.prisma` or `prisma/schema.prisma` like Prisma, reads `DATABASE_URL` from `.env` (the shell
  wins), asks before losing data when someone is at the terminal and refuses otherwise unless
  `--accept-data-loss` is given, and runs `generate` after `migrate dev`, `migrate reset` and
  `db push`. `db pull` reads MongoDB for now; Postgres follows in 0.2.
- `lumen_migrate::commands::push`, `diff_sql` and `resolve` behind `db push`, `migrate diff` and
  `migrate resolve`.
- Release binaries for Linux (x86_64, arm64), macOS (arm64, x86_64) and Windows, attached to each
  `v*` tag with SHA-256 sums.
- `Error::http_status()` (404 / 409 / 400 / 503 / 500 from the code) and `Error::is_not_found()`,
  so a web framework maps lumen errors in one line; `MongoDb::connect_url` takes the database
  from the URL and connects on first use.
- MongoDB clients fill `@default(uuid())` / `uuid(7)` ids themselves, as Prisma does, so a
  collection with string UUID ids (for example one written by another app) works as it is.
