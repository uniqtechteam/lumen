//! SPEC-SCHEMA-001 FR-001..003, FR-005: parsing, the syntax tree and diagnostics.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests: a failure should stop the test"
)]

use lumen_schema::ast::{Arity, ConfigKind, Expr, ModelKind, Top};
use lumen_schema::{SourceFile, parse};

fn parse_ok(name: &str, text: &str) -> lumen_schema::ast::SchemaAst {
    let file = SourceFile::new(name, text);
    let (ast, diagnostics) = parse(&file);
    assert!(diagnostics.is_empty(), "{}", diagnostics.render(&file));
    ast
}

fn model<'a>(ast: &'a lumen_schema::ast::SchemaAst, name: &str) -> &'a lumen_schema::ast::Model {
    ast.tops
        .iter()
        .find_map(|t| match t {
            Top::Model(m) if m.name.name == name => Some(m),
            _ => None,
        })
        .unwrap()
}

#[test]
fn ac_001_the_corpus_parses_without_problems() {
    for (name, text) in [
        ("blog.prisma", include_str!("corpus/blog.prisma")),
        ("mongo.prisma", include_str!("corpus/mongo.prisma")),
        ("shop.prisma", include_str!("corpus/shop.prisma")),
    ] {
        let ast = parse_ok(name, text);
        assert!(ast.tops.len() >= 3, "{name}");
    }
}

#[test]
fn fields_types_and_attributes() {
    let ast = parse_ok("blog.prisma", include_str!("corpus/blog.prisma"));
    let user = model(&ast, "User");
    assert_eq!(user.comments.doc, ["Someone who can sign in."]);
    let email = &user.fields[1];
    assert_eq!(email.name.name, "email");
    let ty = email.ty.as_ref().unwrap();
    assert_eq!(
        (ty.name.name.as_str(), ty.arity),
        ("String", Arity::Required)
    );
    let names: Vec<&str> = email
        .attributes
        .iter()
        .map(|a| a.name.name.as_str())
        .collect();
    assert_eq!(names, ["unique", "db.VarChar"]);
    assert!(matches!(&email.attributes[1].args[0].value, Expr::Number(n, _) if n == "320"));
    assert_eq!(user.fields[2].ty.as_ref().unwrap().arity, Arity::Optional);
    assert_eq!(user.fields[4].ty.as_ref().unwrap().arity, Arity::List);
    assert_eq!(user.attributes[0].name.name, "map");

    let profile = model(&ast, "Profile");
    assert_eq!(profile.comments.doc, ["Extra details, one per user."]);
    let relation = &profile.fields[2].attributes[0];
    let arg_names: Vec<&str> = relation
        .args
        .iter()
        .map(|a| a.name.as_ref().unwrap().name.as_str())
        .collect();
    assert_eq!(arg_names, ["fields", "references", "onDelete"]);
    assert!(matches!(&relation.args[2].value, Expr::Constant(c, _) if c == "Cascade"));

    let post = model(&ast, "Post");
    let index = post
        .attributes
        .iter()
        .find(|a| a.name.name == "index")
        .unwrap();
    let Expr::Array(items, _) = &index.args[0].value else {
        panic!()
    };
    assert!(
        matches!(&items[1], Expr::Function(f, args, _) if f.name == "published" && args.len() == 1)
    );
}

#[test]
fn comments_are_kept_for_the_formatter() {
    let ast = parse_ok("blog.prisma", include_str!("corpus/blog.prisma"));
    let Top::Config(generator) = &ast.tops[1] else {
        panic!()
    };
    assert_eq!(generator.kind, ConfigKind::Generator);
    assert_eq!(
        generator.properties[1].comments.trailing.as_deref(),
        Some("generated, do not edit")
    );
    let Top::Config(datasource) = &ast.tops[0] else {
        panic!()
    };
    assert_eq!(
        datasource.comments.leading,
        ["A blog on Postgres: one-to-many, one-to-one, many-to-many, enums, native types."]
    );
}

#[test]
fn mongo_composite_types_and_views() {
    let ast = parse_ok("mongo.prisma", include_str!("corpus/mongo.prisma"));
    let address = model(&ast, "Address");
    assert_eq!(address.kind, ModelKind::CompositeType);
    assert_eq!(address.comments.doc, ["Embedded in an account document."]);
    let shop = parse_ok("shop.prisma", include_str!("corpus/shop.prisma"));
    assert_eq!(model(&shop, "ProductTotal").kind, ModelKind::View);
    let product = model(&shop, "Product");
    let location = product.fields[5].ty.as_ref().unwrap();
    assert_eq!(location.unsupported.as_deref(), Some("point"));
    assert_eq!(location.arity, Arity::Optional);
    let index = &product.attributes[0];
    assert!(
        matches!(&index.args[1].value, Expr::Object(members, _) if members[0].0.name == "archived")
    );
}

#[test]
fn ac_fr_005_errors_point_at_the_line_and_parsing_goes_on() {
    let text = "model User {\n  id Int @id\n  email String? []\n  name\n}\n\nmodle Post {\n  id Int\n}\n\nmodel Tag {\n  id String @id\n}\n";
    let file = SourceFile::new("schema.prisma", text);
    let (ast, diagnostics) = parse(&file);
    let rendered = diagnostics.render(&file);
    assert_eq!(
        rendered,
        "error: a field cannot be both a list and optional
 --> schema.prisma:3:9
  |
3 |   email String? []
  |         ^^^^^^^^^^
  = help: use `String[]`; an empty list already means there is nothing

error: the field `name` has no type
 --> schema.prisma:4:3
  |
4 |   name
  |   ^^^^
  = help: write its type after the name, for example `name String`

error: expected a block, found `modle`
 --> schema.prisma:7:1
  |
7 | modle Post {
  | ^^^^^
  = help: the top level holds `model`, `enum`, `type`, `view`, `datasource` and `generator` blocks

"
    );
    let names: Vec<&str> = ast
        .tops
        .iter()
        .filter_map(|t| match t {
            Top::Model(m) => Some(m.name.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        ["User", "Tag"],
        "the broken block is skipped, the next one still parses"
    );
}

#[test]
fn an_unclosed_block_names_the_block() {
    let file = SourceFile::new("schema.prisma", "model User {\n  id Int @id\n");
    let (_, diagnostics) = parse(&file);
    assert_eq!(diagnostics.all().len(), 1);
    assert_eq!(
        diagnostics.all()[0].message,
        "the model `User` is never closed"
    );
}
