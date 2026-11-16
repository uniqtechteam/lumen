# lumen for Prisma users

Your `schema.prisma` and your `prisma/migrations` folder work as they are. This page lists what
maps to what, and the few places where Rust makes lumen look different.

## Commands

| Prisma | lumen |
| --- | --- |
| `prisma init` | `lumen init` (`--provider mongodb` for MongoDB) |
| `prisma format`, `prisma validate` | `lumen format [--check]`, `lumen validate` |
| `prisma generate` | `lumen generate`; it uses the generator with `provider = "lumen"` |
| `prisma migrate dev --name x` | `lumen migrate dev --name x` |
| `prisma migrate deploy / status / reset / resolve` | the same names |
| `prisma migrate diff --from-.. --to-schema-datamodel` | `lumen migrate diff [--from-empty]` |
| `prisma db push`, `prisma db pull` | `lumen db push`; `lumen db pull` (MongoDB in 0.1) |

Both tools write to the same `_prisma_migrations` table, so you can run one after the other on the
same database.

## Client

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
one-to-many and (on Postgres) many-to-many relations. As in Prisma, `every` is true for a record
with no related records, and `is_not` matches records with no related record. On Postgres they are
`EXISTS` subqueries. MongoDB has no joins in a plain find, so lumen first collects the keys of the
related documents that match, with one aggregation per relation filter, and then matches on those
keys with `$in`. Many-to-many relation filters are not available on MongoDB yet.

## Values

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

## Errors

Every error has Prisma's `code` (`P2002`, `P2003`, `P2025`, ...) and a plain `message`. A unique
violation also lists the fields behind the constraint in `err.fields`. `err.http_status()` turns
the code into the status a web API should answer with.
