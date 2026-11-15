//! Reads, checks and formats `schema.prisma` files.
//!
//! ```
//! use lumen_schema::{SourceFile, parse};
//!
//! let file = SourceFile::new("schema.prisma", "model User {\n  id Int @id\n}\n");
//! let (ast, diagnostics) = parse(&file);
//! assert!(diagnostics.is_empty());
//! assert_eq!(ast.tops.len(), 1);
//! ```

pub mod ast;
pub mod diagnostics;
pub mod lexer;
mod parser;
pub mod span;

pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use span::{FileId, SourceFile, Span};

/// The version of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Parse one schema file. The syntax tree always comes back, together with every problem found;
/// one broken line never hides the rest of the file.
#[must_use]
pub fn parse(file: &SourceFile) -> (ast::SchemaAst, Diagnostics) {
    parse_file(file, FileId(0))
}

/// Like [`parse`], for one file among several (spans carry `id`).
#[must_use]
pub fn parse_file(file: &SourceFile, id: FileId) -> (ast::SchemaAst, Diagnostics) {
    let mut diagnostics = Diagnostics::default();
    let tokens = lexer::lex(&file.text, id, &mut diagnostics);
    let ast = parser::parse_tokens(&tokens, &mut diagnostics);
    (ast, diagnostics)
}
