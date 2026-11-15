//! SPEC-SCHEMA-001 FR-004: the checked schema and its validation messages.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests: a failure should stop the test"
)]

use lumen_schema::checked::{
    DefaultValue, FieldKind, IndexKind, Literal, Provider, ReferentialAction, RelationKind,
    ScalarType, Schema, StringOrEnv,
};
use lumen_schema::{Severity, SourceFile, check};

fn ok(name: &str, text: &str) -> Schema {
    let file = SourceFile::new(name, text);
    let (schema, d) = check(&file);
    assert!(!d.has_errors(), "{}", d.render(&file));
    schema
}

/// The first error's message and help.
fn first_error(text: &str) -> (String, Option<String>) {
    let file = SourceFile::new("schema.prisma", text);
    let (_, d) = check(&file);
    let e = d
        .all()
        .iter()
        .find(|d| d.severity == Severity::Error)
        .unwrap_or_else(|| panic!("no error for:\n{text}"));
    (e.message.clone(), e.help.clone())
}

const PG: &str = "datasource db {\n  provider = \"postgresql\"\n  url = env(\"DATABASE_URL\")\n}\n";

#[test]
fn ac_001_the_corpus_checks_without_errors() {
    for (name, text) in [
        ("blog.prisma", include_str!("corpus/blog.prisma")),
        ("mongo.prisma", include_str!("corpus/mongo.prisma")),
        ("shop.prisma", include_str!("corpus/shop.prisma")),
    ] {
        ok(name, text);
    }
}

#[test]
fn the_checked_blog() {
    let s = ok("blog.prisma", include_str!("corpus/blog.prisma"));
    let ds = s.datasource.as_ref().unwrap();
    assert_eq!(ds.provider, Provider::Postgres);
    assert_eq!(ds.url, Some(StringOrEnv::Env("DATABASE_URL".into())));
    assert_eq!(s.generators[0].output.as_deref(), Some("../src/db.rs"));

    let (_, user) = s.model("User").unwrap();
    assert_eq!(user.database_name(), "users");
    assert_eq!(user.primary_key().unwrap().fields[0].field, 0);
    assert_eq!(user.fields[0].default, Some(DefaultValue::Autoincrement));
    assert_eq!(user.fields[1].native_type.as_ref().unwrap().args, [320]);
    assert_eq!(
        user.fields[3].default,
        Some(DefaultValue::Literal(Literal::EnumValue("READER".into())))
    );
    assert_eq!(user.fields[6].database_name(), "created_at");
    assert!(user.fields[7].updated_at);

    let kinds: Vec<(RelationKind, String)> = s
        .relations
        .iter()
        .map(|r| (r.kind, r.name.clone()))
        .collect();
    assert!(kinds.contains(&(RelationKind::OneToOne, "ProfileToUser".into())));
    assert!(kinds.contains(&(RelationKind::OneToMany, "PostToUser".into())));
    assert!(kinds.contains(&(RelationKind::ManyToMany, "PostToTag".into())));

    let profile = s
        .relations
        .iter()
        .find(|r| r.name == "ProfileToUser")
        .unwrap();
    assert_eq!(
        s.models[profile.a.model.0].name, "Profile",
        "the side with the foreign key is `a`"
    );
    assert_eq!(profile.on_delete, Some(ReferentialAction::Cascade));
    assert_eq!(s.field(profile.a).name, "user");
    let (_, p) = s.model("Profile").unwrap();
    assert_eq!(p.fields[profile.fields[0]].name, "userId");

    let (_, post) = s.model("Post").unwrap();
    let unique = post
        .indexes
        .iter()
        .find(|i| i.kind == IndexKind::Unique)
        .unwrap();
    assert_eq!(unique.client_name.as_deref(), Some("one_title_per_author"));
    assert!(matches!(post.fields[6].kind, FieldKind::Model(_)));
    assert_eq!(post.fields[7].kind, FieldKind::Scalar(ScalarType::BigInt));
}

#[test]
fn ac_002_a_missing_opposite_field_names_both_models() {
    let text = format!(
        "{PG}model User {{\n  id Int @id\n}}\nmodel Post {{\n  id Int @id\n  authorId Int\n  author User @relation(fields: [authorId], references: [id])\n}}\n"
    );
    let file = SourceFile::new("schema.prisma", &text);
    let (_, d) = check(&file);
    assert_eq!(
        d.render(&file),
        "error: the relation field `author` on `Post` has no opposite field on `User`
  --> schema.prisma:11:3
   |
11 |   author User @relation(fields: [authorId], references: [id])
   |   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   = help: add `posts Post[]` to the model `User`, or run `lumen format` to add it

"
    );
}

#[test]
fn unknown_types_suggest_the_closest_name() {
    let (msg, help) = first_error("model A {\n  id Int @id\n  name Strng\n}\n");
    assert_eq!(msg, "unknown type `Strng`");
    assert_eq!(help.as_deref(), Some("did you mean `String`?"));
}

#[test]
fn relation_mistakes() {
    let missing_fk = format!(
        "{PG}model User {{\n  id Int @id\n  posts Post[]\n}}\nmodel Post {{\n  id Int @id\n  author User\n}}\n"
    );
    let (msg, help) = first_error(&missing_fk);
    assert_eq!(
        msg,
        "the relation field `author` on `Post` needs `fields` and `references`"
    );
    assert_eq!(
        help.as_deref(),
        Some("add `@relation(fields: [authorId], references: [id])` and a `authorId` field")
    );

    let not_unique = format!(
        "{PG}model User {{\n  id Int @id\n  email String\n  profile Profile?\n}}\nmodel Profile {{\n  id Int @id\n  email String @unique\n  user User @relation(fields: [email], references: [email])\n}}\n"
    );
    assert_eq!(
        first_error(&not_unique).0,
        "`user` on `Profile`: the referenced fields are not unique on `User`"
    );

    let one_to_one = format!(
        "{PG}model User {{\n  id Int @id\n  profile Profile?\n}}\nmodel Profile {{\n  id Int @id\n  userId Int\n  user User @relation(fields: [userId], references: [id])\n}}\n"
    );
    assert_eq!(
        first_error(&one_to_one).0,
        "`user` on `Profile`: a one-to-one relation needs its foreign key to be unique"
    );

    let types = format!(
        "{PG}model User {{\n  id Int @id\n  posts Post[]\n}}\nmodel Post {{\n  id Int @id\n  authorId String\n  author User @relation(fields: [authorId], references: [id])\n}}\n"
    );
    assert_eq!(
        first_error(&types).0,
        "`author` on `Post`: `authorId` and `User.id` have different types"
    );

    let required = format!(
        "{PG}model User {{\n  id Int @id\n  posts Post[]\n}}\nmodel Post {{\n  id Int @id\n  authorId Int?\n  author User @relation(fields: [authorId], references: [id])\n}}\n"
    );
    assert_eq!(
        first_error(&required).0,
        "`author` on `Post`: the relation field is required but a foreign key field is optional"
    );
}

#[test]
fn set_null_on_a_required_key_is_a_warning() {
    let text = format!(
        "{PG}model User {{\n  id Int @id\n  posts Post[]\n}}\nmodel Post {{\n  id Int @id\n  authorId Int\n  author User? @relation(fields: [authorId], references: [id], onDelete: SetNull)\n}}\n"
    );
    let file = SourceFile::new("schema.prisma", &text);
    let (_, d) = check(&file);
    assert!(!d.has_errors(), "{}", d.render(&file));
    assert_eq!(d.all()[0].severity, Severity::Warning);
}

#[test]
fn defaults_must_fit() {
    let (msg, _) = first_error(&format!(
        "{PG}model A {{\n  id String @id @default(autoincrement())\n}}\n"
    ));
    assert_eq!(
        msg,
        "`@default`: `autoincrement()` needs an `Int` or `BigInt` field"
    );
    let (msg, _) = first_error(&format!(
        "{PG}enum Role {{\n  A\n  B\n}}\nmodel M {{\n  id Int @id\n  role Role @default(C)\n}}\n"
    ));
    assert_eq!(
        msg,
        "`@default`: `C` is not a value of the enum `Role` (A, B)"
    );
    let (msg, _) = first_error(&format!(
        "{PG}model A {{\n  id String @id @default(auto())\n}}\n"
    ));
    assert!(msg.contains("`auto()` is for MongoDB ObjectIds"));
}

#[test]
fn native_types_must_exist_and_fit() {
    let (msg, help) = first_error(&format!(
        "{PG}model A {{\n  id Int @id\n  name String @db.Varchar(10)\n}}\n"
    ));
    assert_eq!(msg, "`@db.Varchar`: unknown native type for this database");
    assert_eq!(help.as_deref(), Some("did you mean `@db.VarChar`?"));
    let (msg, _) = first_error(&format!("{PG}model A {{\n  id Int @id @db.Uuid\n}}\n"));
    assert_eq!(msg, "`@db.Uuid`: can only be used on String fields");
    let (msg, help) = first_error(&format!(
        "{PG}model A {{\n  id Int @id\n  name String @pg.Text\n}}\n"
    ));
    assert!(msg.contains("native types use the datasource's name, `db`"));
    assert_eq!(help.as_deref(), Some("write `@db.Text`"));
}

#[test]
fn model_level_rules() {
    let (msg, _) = first_error(&format!("{PG}model A {{\n  name String\n}}\n"));
    assert_eq!(msg, "the model `A` has no `@id`, `@@id` or unique field");
    let (msg, _) = first_error(&format!(
        "{PG}model A {{\n  id Int @id\n  @@map(\"t\")\n}}\nmodel B {{\n  id Int @id\n  @@map(\"t\")\n}}\n"
    ));
    assert_eq!(msg, "`A` already uses the table `t`");
    let mongo = "datasource db {\n  provider = \"mongodb\"\n}\nmodel A {\n  id String @id @default(auto()) @db.ObjectId\n}\n";
    let (msg, help) = first_error(mongo);
    assert_eq!(msg, "`@id`: a MongoDB id field is stored as `_id`");
    assert_eq!(help.as_deref(), Some("add `@map(\"_id\")` to the id field"));
    let (msg, help) = first_error("datasource db {\n  provider = \"mysql\"\n}\n");
    assert_eq!(msg, "lumen does not support the `mysql` provider yet");
    assert_eq!(
        help.as_deref(),
        Some("lumen 0.1 supports \"postgresql\" and \"mongodb\"")
    );
}

#[test]
fn rename_hints_reach_the_checked_schema() {
    let text = format!(
        "{PG}// lumen: renamed from accounts\nmodel User {{\n  id Int @id\n  // lumen: renamed from full_name\n  name String\n}}\n"
    );
    let s = ok("schema.prisma", &text);
    let (_, user) = s.model("User").unwrap();
    assert_eq!(user.renamed_from.as_deref(), Some("accounts"));
    assert_eq!(user.fields[1].renamed_from.as_deref(), Some("full_name"));
}
