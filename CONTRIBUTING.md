# Contributing

Thanks for helping. A bug report with a schema that shows the problem is the most useful kind. A few
things make a pull request easy to merge:

- **Open an issue first** for anything larger than a small fix, so we can agree on the shape.
- **One change per pull request**, with a short description of what and why, and how you tested it.
- **Tests** come with every new feature and every bug fix. Database code is tested against a real
  database, not only against mocks.
- **Checks** must pass (CI runs the same):

  ```sh
  cargo fmt --all --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  cargo deny --workspace check
  ```

- **Databases**: without any setup, `cargo test --workspace` runs everything that needs no database
  and skips the rest. To run the database tests, start Postgres and a one-node MongoDB 7 replica set
  (transactions need one) and point the tests at them:

  ```sh
  docker run -d --rm --name lumen-pg -p 55432:5432 \
    -e POSTGRES_PASSWORD=lumen -e POSTGRES_DB=lumen postgres:16
  docker run -d --rm --name lumen-mongo -p 27017:27017 mongo:7 --replSet rs0 --bind_ip_all
  docker exec lumen-mongo mongosh --quiet \
    --eval 'rs.initiate({ _id: "rs0", members: [{ _id: 0, host: "localhost:27017" }] })'

  export LUMEN_TEST_POSTGRES_URL=postgres://postgres:lumen@localhost:55432/lumen
  export LUMEN_TEST_MONGO_URL='mongodb://localhost:27017/?directConnection=true'
  cargo test --workspace
  ```

  CI runs them on Postgres 13 to 17. `LUMEN_TEST_PRISMA=1` also checks the migrations against the
  real Prisma CLI; it needs `npx` (Node 22) and a Postgres URL.

- **The `chrono` feature**: dates are `time` types by default. If you touch dates, also run
  `cargo test --workspace --features lumen-orm/chrono`.
- **Generated clients**: `crates/lumen-db-codegen/tests/generated/*.rs` are committed, and a test
  fails when one no longer matches what the generator writes. After changing the generator,
  regenerate them from the `tests` folder, for example:

  ```sh
  cd crates/lumen-db-codegen/tests
  cargo run -q -p lumen-db-codegen --example gen -- blog.prisma > generated/blog.rs
  ```

  and read the diff before you commit it. The compile errors users see are checked by trybuild tests
  in `tests/ui`; refresh them on purpose with `TRYBUILD=overwrite`.
- **Real schemas**: the parser, checker, formatter and migration engine are tested against the
  schema files in `crates/lumen-schema/tests/corpus`, with their sources and licences. A schema that
  lumen reads wrong is a good addition there.
- **CHANGELOG.md**: add a line under *Unreleased*, written for people who use lumen.
- Be kind.

By contributing you agree that your work is licensed under MIT OR Apache-2.0, like the rest of the
project.
