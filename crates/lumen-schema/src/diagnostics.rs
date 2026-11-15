//! Errors and warnings about a schema. Every message is made by a named constructor in this file,
//! so the wording lives in one place and tests can pin it.

use std::fmt::Write as _;

use crate::span::{SourceFile, Span};

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The schema cannot be used until this is fixed.
    Error,
    /// The schema works, but something is probably not what was meant.
    Warning,
}

/// One problem, where it is, and how to fix it when we know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Error or warning.
    pub severity: Severity,
    /// What is wrong, in one sentence.
    pub message: String,
    /// Where.
    pub span: Span,
    /// How to fix it, when there is a clear fix.
    pub help: Option<String>,
}

impl Diagnostic {
    fn error(span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            span,
            help: None,
        }
    }

    fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// A character that cannot start any token.
    #[must_use]
    pub fn unexpected_character(span: Span, c: char) -> Self {
        Self::error(span, format!("unexpected character `{c}`"))
    }

    /// A string without its closing quote.
    #[must_use]
    pub fn unterminated_string(span: Span) -> Self {
        Self::error(span, "this string has no closing `\"`")
            .with_help("strings end on the same line with `\"`")
    }

    /// `/*` without `*/`.
    #[must_use]
    pub fn unterminated_comment(span: Span) -> Self {
        Self::error(span, "this comment has no closing `*/`")
    }

    /// `type Id = String`: aliases were removed from Prisma.
    #[must_use]
    pub fn type_alias(span: Span, name: &str) -> Self {
        Self::error(span, format!("type aliases are not supported (`type {name} = ...`)"))
            .with_help("write the type and its attributes on each field instead, or use a composite `type` block")
    }

    /// An escape such as `\q` inside a string.
    #[must_use]
    pub fn unknown_escape(span: Span, escape: char) -> Self {
        Self::error(span, format!("unknown escape `\\{escape}` in a string"))
            .with_help("use `\\\"`, `\\\\`, `\\n`, `\\r`, `\\t` or `\\u{..}`")
    }

    /// Something other than `model`, `enum`, `type`, `view`, `datasource` or `generator` at the
    /// top level.
    #[must_use]
    pub fn unexpected_top_level(span: Span, found: &str) -> Self {
        Self::error(span, format!("expected a block, found `{found}`")).with_help(
            "the top level holds `model`, `enum`, `type`, `view`, `datasource` and `generator` blocks",
        )
    }

    /// A token that does not fit where it is.
    #[must_use]
    pub fn expected(span: Span, expected: &str, found: &str) -> Self {
        Self::error(span, format!("expected {expected}, found {found}"))
    }

    /// A block that reaches the end of the file without `}`.
    #[must_use]
    pub fn unclosed_block(span: Span, kind: &str, name: &str) -> Self {
        Self::error(span, format!("the {kind} `{name}` is never closed"))
            .with_help("add `}` at the end of the block")
    }

    /// A line inside a block that is not a field, enum value or block attribute.
    #[must_use]
    pub fn invalid_line(span: Span, kind: &str) -> Self {
        let help = match kind {
            "enum" => {
                "an enum line is one value, for example `ADMIN`, optionally with `@map(\"..\")`"
            }
            "datasource" | "generator" => "a setting looks like `provider = \"postgresql\"`",
            _ => {
                "a field looks like `email String @unique`, a block attribute like `@@index([email])`"
            }
        };
        Self::error(span, format!("this line is not valid inside a {kind}")).with_help(help)
    }

    /// `String?[]` or `String[]?`.
    #[must_use]
    pub fn optional_list(span: Span) -> Self {
        Self::error(span, "a field cannot be both a list and optional")
            .with_help("use `String[]`; an empty list already means there is nothing")
    }

    /// A field written without a type.
    #[must_use]
    pub fn missing_field_type(span: Span, field: &str) -> Self {
        Self::error(span, format!("the field `{field}` has no type")).with_help(format!(
            "write its type after the name, for example `{field} String`"
        ))
    }
}

/// Every diagnostic found so far. Parsing and checking never stop at the first problem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    /// Add one.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    /// All of them, in the order found.
    #[must_use]
    pub fn all(&self) -> &[Diagnostic] {
        &self.items
    }

    /// Whether any error was found (warnings do not count).
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    /// Whether there is nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Every diagnostic, printed with its source line and a caret under the problem.
    #[must_use]
    pub fn render(&self, file: &SourceFile) -> String {
        let mut out = String::new();
        for d in &self.items {
            render_one(&mut out, d, file);
        }
        out
    }
}

fn render_one(out: &mut String, d: &Diagnostic, file: &SourceFile) {
    let label = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    let (line, col) = file.line_col(d.span.start);
    let text = file.line(line);
    let width = line.to_string().len();
    let pad = " ".repeat(width);
    let start_col = col - 1;
    let (end_line, end_col) = file.line_col(d.span.end);
    let underline = if end_line == line && end_col > col {
        end_col - col
    } else {
        text.chars().count().saturating_sub(start_col).max(1)
    };
    let _ = writeln!(out, "{label}: {}", d.message);
    let _ = writeln!(out, "{pad}--> {}:{line}:{col}", file.name);
    let _ = writeln!(out, "{pad} |");
    let _ = writeln!(out, "{line} | {text}");
    let _ = writeln!(
        out,
        "{pad} | {}{}",
        " ".repeat(start_col),
        "^".repeat(underline)
    );
    if let Some(help) = &d.help {
        let _ = writeln!(out, "{pad} = help: {help}");
    }
    out.push('\n');
}
