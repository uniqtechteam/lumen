//! Write the client for a schema to stdout: `cargo run -p lumen-codegen --example gen -- schema.prisma`.
fn main() {
    let p = std::env::args().nth(1).unwrap_or_default();
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    let (schema, d) = lumen_schema::check(&lumen_schema::SourceFile::new(&p, text));
    if d.has_errors() {
        eprintln!("the schema has errors");
        std::process::exit(1);
    }
    match lumen_codegen::generate(&schema, &p) {
        Ok(code) => print!("{code}"),
        Err(e) => eprintln!("{e}"),
    }
}
