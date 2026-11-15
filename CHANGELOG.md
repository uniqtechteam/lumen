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
