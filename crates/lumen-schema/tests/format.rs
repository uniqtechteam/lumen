//! SPEC-SCHEMA-001 FR-006: `lumen format`.

use lumen_schema::{SourceFile, check, format};

fn fmt(text: &str) -> String {
    format(&SourceFile::new("schema.prisma", text)).0
}

#[test]
fn ac_003_a_formatted_file_is_left_exactly_as_it_is() {
    for text in [
        include_str!("corpus/blog.prisma"),
        include_str!("corpus/mongo.prisma"),
        include_str!("corpus/shop.prisma"),
    ] {
        assert_eq!(fmt(text), text);
    }
}

#[test]
fn formatting_twice_changes_nothing() {
    let messy = "datasource db {\nprovider = \"postgresql\"\n      url = env(\"DATABASE_URL\")\n}\nmodel User {\n    id Int @default(autoincrement()) @id\n  email String @unique\n\n\n  name String? // shown on posts\n  @@map(\"users\")\n}\nenum Role { \n A\n B @map(\"b\")\n}\n";
    let once = fmt(messy);
    assert_eq!(
        once,
        "datasource db {\n  provider = \"postgresql\"\n  url      = env(\"DATABASE_URL\")\n}\n\nmodel User {\n  id    Int    @id @default(autoincrement())\n  email String @unique\n\n  name String? // shown on posts\n\n  @@map(\"users\")\n}\n\nenum Role {\n  A\n  B @map(\"b\")\n}\n"
    );
    assert_eq!(fmt(&once), once);
}

#[test]
fn format_adds_the_missing_side_and_the_schema_then_checks() {
    let text = "datasource db {\n  provider = \"postgresql\"\n}\n\nmodel User {\n  id    Int    @id\n  posts Post[]\n}\n\nmodel Post {\n  id Int @id\n}\n";
    let file = SourceFile::new("schema.prisma", text);
    assert!(
        check(&file).1.has_errors(),
        "the relation is one-sided before formatting"
    );
    let formatted = fmt(text);
    assert!(
        formatted.contains(
            "  user   User? @relation(fields: [userId], references: [id])\n  userId Int?\n"
        ),
        "{formatted}"
    );
    let (_, d) = check(&SourceFile::new("schema.prisma", &formatted));
    assert!(
        !d.has_errors(),
        "{}",
        d.render(&SourceFile::new("schema.prisma", &formatted))
    );
}

#[test]
fn a_broken_file_is_formatted_around_the_broken_line() {
    let text = "model A {\n  id    Int @id\n  ???\n  name String\n}\n";
    assert_eq!(
        fmt(text),
        "model A {\n  id   Int    @id\n  ???\n  name String\n}\n"
    );
}

#[test]
fn a_trailing_doc_comment_is_kept() {
    let text = "model User {\n  id Int @id\n  name String? /// shown on posts\n}\n";
    assert_eq!(
        fmt(text),
        "model User {\n  id   Int     @id\n  name String? /// shown on posts\n}\n"
    );
}
