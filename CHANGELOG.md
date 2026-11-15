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
