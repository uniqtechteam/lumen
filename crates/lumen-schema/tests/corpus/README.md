# Test corpus

Real schema files that lumen's tests read: `tests/corpus.rs` in this crate (each file parses,
checks without errors and formats to a fixed point) and `tests/corpus.rs` in `lumen-migrate`
(each Postgres schema migrates from nothing, the database reads back as the schema, and for the
schemas Prisma 6 accepts, the database Prisma's own SQL builds needs no change from lumen).

The files are only used by tests and are not part of the published crate.

## lumen's own

`blog.prisma`, `shop.prisma` and `mongo.prisma` are written for lumen's tests. `shop.prisma` uses
a partial index (`@@index(.., where: ..)`), which Prisma does not have.

## Prisma examples

`prisma-examples/` holds every distinct Postgres and MongoDB `schema.prisma` from
[prisma/prisma-examples](https://github.com/prisma/prisma-examples) at commit
`45d6bf6193c66bc869240df90c75b2e4b45d008f` (2026-11-11), named after the example's folder
(`orm/nextjs` → `orm-nextjs.prisma`). Licence: Apache-2.0, see `prisma-examples/LICENSE`.

Left out: examples for SQLite, SQL Server and CockroachDB, which lumen does not support, files
identical to one already kept, and the `compute/*/contract.prisma` files, which are Prisma Next
contracts (`// use prisma-8`) and not schema files.

## Open-source projects

One schema per project, copied unchanged with the project's licence next to it.

| Folder | Project | Commit | File | Licence | Database |
| --- | --- | --- | --- | --- | --- |
| `amplication/` | [amplication/amplication](https://github.com/amplication/amplication) | `7656495d27f0dceff89657590c3f14149e45c7a6` | `packages/amplication-prisma-db/prisma/schema.prisma` | Apache-2.0 (outside `ee/`) | Postgres |
| `hoppscotch/` | [hoppscotch/hoppscotch](https://github.com/hoppscotch/hoppscotch) | `55908a76a1d0555c9bbb5d74244c14173df95b06` | `packages/hoppscotch-backend/prisma/schema.prisma` | MIT | Postgres |
| `jitsu/` | [jitsucom/jitsu](https://github.com/jitsucom/jitsu) | `010cf1c772e950a67efeb633bc5383b0fe971b84` | `webapps/console/prisma/schema.prisma` | MIT | Postgres |
| `langfuse/` | [langfuse/langfuse](https://github.com/langfuse/langfuse) | `9b35df6cd47437254182c573a9a528bfb38921d5` | `packages/shared/prisma/schema.prisma` | MIT (outside the `ee` folders) | Postgres |
| `nestjs-prisma-starter/` | [notiz-dev/nestjs-prisma-starter](https://github.com/notiz-dev/nestjs-prisma-starter) | `225e5a906865df237a681c1ad228a3ecf2909326` | `prisma/schema.prisma` | MIT | Postgres |
| `next-auth/` | [nextauthjs/next-auth](https://github.com/nextauthjs/next-auth) | `a1a16a5a7780488c7449feece410033f445d0b31` | `packages/adapter-prisma/prisma/mongodb.prisma` | ISC | MongoDB |
| `open-saas/` | [wasp-lang/open-saas](https://github.com/wasp-lang/open-saas) | `1793ce9f58c4690034e19d1548e6de8e4cc13154` | `template/app/schema.prisma` | MIT | Postgres |
| `saas-starter-kit/` | [boxyhq/saas-starter-kit](https://github.com/boxyhq/saas-starter-kit) | `abc9b686823cbfb4973c79bc36fea37a3244be6c` | `prisma/schema.prisma` | Apache-2.0 | Postgres |
| `spliit/` | [spliit-app/spliit](https://github.com/spliit-app/spliit) | `f293974bb3f0da063c1ced76a4abeb48eddee1e3` | `prisma/schema.prisma` | MIT | Postgres |
| `trigger.dev/` | [triggerdotdev/trigger.dev](https://github.com/triggerdotdev/trigger.dev) | `15f8fb48751bfea2378eda53df05c3359fa6db44` | `internal-packages/database/prisma/schema.prisma` | Apache-2.0 | Postgres |
| `umami/` | [umami-software/umami](https://github.com/umami-software/umami) | `b18d5144dfe7342edeeca6f61d13ba3baabed9ad` | `prisma/schema.prisma` | MIT | Postgres |

Only projects under MIT, ISC, BSD or Apache-2.0 are taken. Projects under AGPL or other
copyleft licences are not copied here, even when their schemas would make good tests.

## Adding a schema

1. Check the licence of the file you copy (some projects license parts of the repository
   differently).
2. Copy the file unchanged, with the licence, into its own folder, and add a row above with the
   commit it came from.
3. Run `cargo test -p lumen-schema --test corpus`, and with `LUMEN_TEST_POSTGRES_URL` set,
   `cargo test -p lumen-migrate --test corpus`. A failure is a bug in lumen until shown otherwise.
