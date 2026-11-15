# lumen

lumen is a Prisma-style data layer for Rust. You describe your data once in a `schema.prisma` file and
get migrations, a generated type-safe client, and a query builder underneath for the queries a
generated client cannot express. Postgres and MongoDB come first.

**Status:** early development, not released yet.

## The idea

```prisma
model User {
  id    Int    @id @default(autoincrement())
  email String @unique
  posts Post[]
}

model Post {
  id       Int    @id @default(autoincrement())
  title    String
  author   User   @relation(fields: [authorId], references: [id])
  authorId Int
}
```

```bash
lumen migrate dev --name init   # writes migrations/2026..._init/migration.sql and applies it
lumen generate                  # writes src/db.rs
```

```rust
let users = db
    .user()
    .find_many()
    .filter(user::email::ends_with("@example.com"))
    .include(user::posts())
    .exec()
    .await?;
```

It reads the same `schema.prisma` files Prisma reads and keeps migrations in the same layout, so a
project can move between the two.

## Credits

lumen's design learns from [Prisma](https://github.com/prisma/prisma-engines) (Apache-2.0) and
[SeaORM](https://github.com/SeaQL/sea-orm) (MIT OR Apache-2.0). It is written from scratch and is not
affiliated with or endorsed by Prisma.

## Licence

MIT OR Apache-2.0, at your option.
