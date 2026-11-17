# lumen for Prisma users

This page is for people who already know Prisma and want to try lumen, or want to know what
happens to an existing Prisma project. The rest of lumen's documentation does not assume you know
Prisma; everything Prisma related lives here.

lumen was designed after studying Prisma closely. It reads the same schema language, keeps
migrations in the same folders and the same history table, and uses Prisma's error codes. What is
different is the client: it is generated Rust instead of TypeScript, so it follows Rust's habits.

## What stays the same

- **The schema.** `schema.prisma` and `prisma/schema.prisma` are found the same way, and the
  language is the same: models, enums, composite types, views, `datasource` and `generator` blocks,
  `@` and `@@` attributes, native types. `lumen format` writes the same layout as `prisma format`.
- **The migrations.** Same folder layout (`prisma/migrations/YYYYMMDDHHMMSS_name/migration.sql`
  and `migration_lock.toml`), same `_prisma_migrations` table with the same columns and checksums,
  and the same advisory lock during `migrate deploy`, so the two tools never deploy at the same
  moment.
- **The SQL.** `lumen migrate dev` writes the SQL Prisma would write for the same change, down to
  the constraint names (`User_pkey`, `User_email_key`, `Post_authorId_fkey`) and the implicit
  many-to-many tables (`_PostToTag` with columns `A` and `B`).
- **The error codes.** `P2002` for a duplicate, `P2003` for a foreign key, `P2025` for a missing
  record, and so on.
- **The behaviour you rely on.** `not` keeps NULL rows, `every` is true when there are no related
  records, paging inside an include is per parent, and MongoDB uses `db push` instead of migration
  files.

This is checked, not hoped for. lumen's CI runs the real Prisma CLI (6.19.3) against the same
database in both directions: a project started with Prisma carries on with `lumen migrate dev`, a
project started with lumen carries on with `prisma migrate dev`, and neither reports drift. To run
those tests yourself, set `LUMEN_TEST_PRISMA=1` next to `LUMEN_TEST_POSTGRES_URL`.

## Moving a Prisma project to lumen

1. Add a generator for lumen next to the one you have. Other generator blocks are left alone, so
   a TypeScript service and a Rust service can share one schema.

   **schema** · `prisma/schema.prisma`

   ```prisma
   generator client {
     provider = "prisma-client-js"
   }

   generator rust {
     provider = "lumen"
     output   = "../src/db.rs"
   }
   ```

2. Run `lumen migrate status`. It reads your `prisma/migrations` folder and the
   `_prisma_migrations` table and should say the database is up to date.
3. Run `lumen generate` and include the client in your crate (see the README's quick start).
4. From now on, `lumen migrate dev --name ..` writes the next migration into the same folder.

## Going back, or using both

Nothing in the migrations folder or the database is lumen specific, so `prisma migrate dev` and
`prisma migrate deploy` carry on from a lumen project as they are. Add a `prisma-client-js`
generator if you want the TypeScript client too.

One lumen addition is a comment: `// lumen: renamed from oldName` above a model or field makes
`lumen migrate dev` write a real `RENAME` instead of a drop and an add. Prisma ignores the comment,
and the migration that was already written keeps its rename.

## The same thing in both

The schema is the same file for both tools. The examples below use this one:

**schema** · `prisma/schema.prisma`

```prisma
model User {
  id        Int      @id @default(autoincrement())
  email     String   @unique
  name      String?
  createdAt DateTime @default(now())
  posts     Post[]
}

model Post {
  id        Int     @id @default(autoincrement())
  title     String
  published Boolean @default(false)
  author    User    @relation(fields: [authorId], references: [id])
  authorId  Int
}
```

### Connecting

**Prisma** · `src/db.ts`

```ts
import { PrismaClient } from "@prisma/client";

export const prisma = new PrismaClient();
```

**lumen** · `src/main.rs`

```rust
#[rustfmt::skip]
mod db;

let client = db::Client::connect(&std::env::var("DATABASE_URL")?).await?;
```

Prisma Client reads `.env` itself. The lumen client does not (the `lumen` command does), so export
`DATABASE_URL` or call `dotenvy::dotenv()` first.

### A query with a filter, an order and an include

**Prisma** · `src/users.ts`

```ts
const writers = await prisma.user.findMany({
  where: { email: { endsWith: "@example.com" } },
  orderBy: { id: "desc" },
  take: 10,
  include: { posts: true },
});
```

**lumen** · `src/users.rs`

```rust
use db::user;

let writers = client
    .user()
    .find_many()
    .filter(user::email::ends_with("@example.com"))
    .order_by(user::id::desc())
    .take(10)
    .include_posts()
    .exec()
    .await?; // Vec<user::WithPosts>
```

### Finding one record

**Prisma** · `src/users.ts`

```ts
const ana = await prisma.user.findUnique({ where: { email: "ana@example.com" } });
```

**lumen** · `src/users.rs`

```rust
let ana = client
    .user()
    .find_unique(user::UniqueWhere::Email("ana@example.com".into()))
    .await?; // Option<user::User>
```

### Updating

**Prisma** · `src/users.ts`

```ts
await prisma.user.update({
  where: { id: 1 },
  data: { name: "Ana Lima" },
});
```

**lumen** · `src/users.rs`

```rust
use lumen_orm::Set;

client
    .user()
    .update(
        user::UniqueWhere::Id(1),
        user::Update { name: Set::To(Some("Ana Lima".into())), ..Default::default() },
    )
    .await?;
```

### A transaction

**Prisma** · `src/signup.ts`

```ts
await prisma.$transaction(async (tx) => {
  const ana = await tx.user.create({ data: newUser });
  await tx.post.create({ data: { title: "Hello", authorId: ana.id } });
});
```

**lumen** · `src/signup.rs`

```rust
use db::{post, user};

client
    .db()
    .transaction(async |tx| {
        let ana = user::on(&mut *tx).create(new_user).await?;
        post::on(&mut *tx).create(first_post(ana.id)).await?;
        Ok(ana)
    })
    .await?;
```

### Handling a duplicate

**Prisma** · `src/signup.ts`

```ts
try {
  await prisma.user.create({ data: newUser });
} catch (e) {
  if (e instanceof Prisma.PrismaClientKnownRequestError && e.code === "P2002") {
    // e.meta.target names the fields
  }
}
```

**lumen** · `src/signup.rs`

```rust
match client.user().create(new_user).await {
    Err(err) if err.code == "P2002" => { /* err.fields names the columns */ }
    other => { other?; }
}
```

## Commands side by side

| Prisma | lumen |
| --- | --- |
| `prisma init` | `lumen init` (`--provider mongodb` for MongoDB) |
| `prisma format`, `prisma validate` | `lumen format [--check]`, `lumen validate` |
| `prisma generate` | `lumen generate`; it uses the generator with `provider = "lumen"` |
| `prisma migrate dev --name x` | `lumen migrate dev --name x` |
| `prisma migrate deploy / status / reset / resolve` | the same names |
| `prisma migrate diff --from-.. --to-schema-datamodel` | `lumen migrate diff [--from-empty]` |
| `prisma db push`, `prisma db pull` | `lumen db push`; `lumen db pull` (MongoDB in 0.1) |

## Client calls side by side

Model and field names become snake_case: model `User` is `client.user()` and module `user`, and
field `createdAt` is `user::created_at`.

| Prisma Client | lumen |
| --- | --- |
| `findMany({ where, orderBy, take, skip })` | `find_many().filter(..).order_by(user::id::desc()).take(10).skip(20).exec()` |
| `findUnique({ where: { email } })` | `find_unique(user::UniqueWhere::Email(email))`, which returns `Option` |
| `findFirst({ where })` | `find_first(filter)` |
| `include: { posts: true }` | `.include_posts()`, which returns `user::WithPosts` with a `posts` field |
| `create({ data })` | `create(user::Create { .. })`; fields with defaults are `Option`, so `None` uses the default |
| `update({ where, data })` | `update(where, user::Update { name: Set::To(..), ..Default::default() })` |
| `upsert({ where, create, update })` | `upsert(where, create, update)` |
| `create({ data: { .., posts: { create: [..], connect: [..] } } })` | `create_nested(user::Create { .. }, user::Nested { posts: Many::create(vec![..]).and_connect(vec![..]), .. })` |
| `post.create({ data: { .., author: { connect: { email } } } })` | `create_with_author(post::CreateWithoutAuthor { .. }, One::Connect(user::UniqueWhere::Email(..)))` |
| `delete`, `deleteMany`, `updateMany`, `createMany`, `count` | `delete`, `delete_many`, `update_many`, `create_many`, `count` |
| `$transaction(async (tx) => ..)` | `client.db().transaction(async \|tx\| { user::on(&mut *tx).create(..).await })` |

Filters are functions on the field's module, and each one only takes values of that field's type.
Passing text to an `Int` field does not compile.

| Prisma `where` | lumen |
| --- | --- |
| `{ email: "a" }` | `user::email::equals("a")` |
| `{ email: { endsWith: "@x.com" } }` | `user::email::ends_with("@x.com")` |
| `{ email: { contains: "a", mode: "insensitive" } }` | `user::email::contains_insensitive("a")` |
| `{ email: { startsWith: "A", mode: "insensitive" } }` | `user::email::starts_with_insensitive("A")` (and `ends_with_insensitive`) |
| `{ id: { in: [1, 2] } }` | `user::id::is_in([1, 2])` |
| `{ name: null }` | `user::name::is_null()` |
| `AND`, `OR`, `NOT` | `a.and(b)`, `a.or(b)`, `!a` |
| `{ posts: { some: { published: true } } }` | `user::posts::some(post::published::equals(true))` |
| `{ posts: { every: .. } }`, `{ posts: { none: .. } }` | `user::posts::every(..)`, `user::posts::none(..)` |
| `{ author: { is: { email: "a" } } }` | `post::author::is(user::email::equals("a"))` |
| `{ author: { isNot: .. } }` | `post::author::is_not(..)` |
| `{ profile: { is: null } }`, `{ profile: { isNot: null } }` | `user::profile::is_null()`, `user::profile::is_not_null()` (optional relations only) |

Relation filters nest inside each other and inside `and`, `or` and `!`, and work on one-to-one,
one-to-many and many-to-many relations. As in Prisma, `every` is true for a record
with no related records, and `is_not` matches records with no related record. On Postgres they are
`EXISTS` subqueries. MongoDB has no joins in a plain find, so lumen first collects the keys of the
related documents that match, with one aggregation per relation filter, and then matches on those
keys with `$in`. A many-to-many relation on MongoDB is written as in Prisma, with a list of ids on
both sides (`labelIds String[] @db.ObjectId` and `@relation(fields: [labelIds], references: [id])`),
and its filters match on that list.

## Types side by side

| Prisma type | Rust (Postgres) | Rust (MongoDB) |
| --- | --- | --- |
| `String` | `String` (`Uuid` with `@db.Uuid`) | `String` (`ObjectId` with `@db.ObjectId`) |
| `Int`, `BigInt`, `Float` | `i32`, `i64`, `f64` | the same |
| `Decimal` | `rust_decimal::Decimal` | `Decimal128` |
| `DateTime` | `chrono::NaiveDateTime` (`DateTime<Utc>` with `@db.Timestamptz`) | BSON `DateTime` |
| `Json` | `serde_json::Value` | the same |
| `Bytes` | `Vec<u8>` | BSON `Binary` |
| `Type?` | `Option<Type>` | the same |
| `Type[]` | `Vec<Type>` | the same |

## Errors side by side

Every error has Prisma's `code` (`P2002`, `P2003`, `P2025`, ...) and a plain `message`. A unique
violation also lists the schema fields behind the constraint (on MongoDB, the unique index) in
`err.fields`, like Prisma's `meta.target`. `err.http_status()` turns the code into the status a web
API should answer with.

## Differences to keep in mind

- **Names follow Rust.** Model `User` is `client.user()` and module `user`; field `createdAt` is
  `user::created_at`. The database names do not change.
- **The client is a file you can read.** `lumen generate` writes one Rust file (by default
  `src/db.rs`) instead of a package in `node_modules`. Commit it, and include it with
  `#[rustfmt::skip]` so `cargo fmt` leaves it as generated.
- **Inputs are structs.** `create` takes `user::Create` with every field spelled out. Fields with
  a default are `Option`, and `None` means "use the default". `update` takes `user::Update`, where
  each field is `Set::To(..)` or `Set::NotSet`.
- **Wrong types do not compile.** A filter only takes values of its field's type, and a field that
  does not exist is a compile error, not a runtime one.
- **Includes return their own types.** `.include_posts()` returns `user::WithPosts`, a record with
  a `posts` field, rather than adding a field to `User`.
- **Not in 0.1 yet:** `select`, aggregates and `groupBy`, cursor pagination, ordering by relation
  count, includes of many-to-many relations and nested includes in the generated client,
  `connect_or_create`, `disconnect`, `set` and nested writes inside `update`, and `db pull` for
  Postgres. The README's Limits section has the current list.
- **`migrate diff`** takes `--from-empty` or compares with the database, instead of Prisma's
  `--from-..` and `--to-..` flags.
- **MongoDB:** nested writes need a replica set, as transactions do. Many-to-many relations can
  be filtered but not included yet, and lumen does not keep the two id lists in step for you.
- **Databases:** Postgres and MongoDB only. No SQLite, MySQL or SQL Server.

## Credits

lumen's design learns from [Prisma](https://github.com/prisma/prisma-engines) (Apache-2.0) and
[SeaORM](https://github.com/SeaQL/sea-orm) (MIT OR Apache-2.0), whose query builder shaped
`lumen-orm`. lumen is written from scratch and is not affiliated with or endorsed by Prisma.
