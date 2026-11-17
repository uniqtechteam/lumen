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

    /// A warning with a hint.
    #[must_use]
    pub fn warning_help(span: Span, message: impl Into<String>, help: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            span,
            help: Some(help.into()),
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

    /// `type Id = String`: the schema language has no type aliases.
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

    // ---- validation -----------------------------------------------------------------------

    /// A model, enum or type named like a built-in type.
    #[must_use]
    pub fn reserved_name(span: Span, name: &str) -> Self {
        Self::error(
            span,
            format!("`{name}` is a built-in type and cannot name a block"),
        )
    }

    /// Two blocks with one name.
    #[must_use]
    pub fn duplicate_top(span: Span, name: &str) -> Self {
        Self::error(span, format!("`{name}` is defined more than once"))
            .with_help("models, enums and types share one set of names")
    }

    /// Two fields with one name.
    #[must_use]
    pub fn duplicate_field(span: Span, field: &str, model: &str) -> Self {
        Self::error(span, format!("`{model}` has two fields named `{field}`"))
    }

    /// Two enum values with one name.
    #[must_use]
    pub fn duplicate_enum_value(span: Span, value: &str, name: &str) -> Self {
        Self::error(
            span,
            format!("the enum `{name}` has the value `{value}` twice"),
        )
    }

    /// An enum with no values.
    #[must_use]
    pub fn empty_enum(span: Span, name: &str) -> Self {
        Self::error(span, format!("the enum `{name}` has no values"))
    }

    /// A field type that is not a scalar, model, enum or type.
    #[must_use]
    pub fn unknown_type(span: Span, name: &str, did_you_mean: Option<&str>) -> Self {
        let d = Self::error(span, format!("unknown type `{name}`"));
        match did_you_mean {
            Some(other) => d.with_help(format!("did you mean `{other}`?")),
            None => d.with_help(
                "use a built-in type (String, Int, BigInt, Float, Decimal, Boolean, DateTime, Json, Bytes), or a model, enum or type from this file",
            ),
        }
    }

    /// A provider lumen does not support yet.
    #[must_use]
    pub fn unsupported_provider(span: Span, provider: &str) -> Self {
        Self::error(
            span,
            format!("lumen does not support the `{provider}` provider yet"),
        )
        .with_help("lumen 0.1 supports \"postgresql\" and \"mongodb\"")
    }

    /// A datasource without `provider`.
    #[must_use]
    pub fn missing_provider(span: Span, name: &str) -> Self {
        Self::error(span, format!("the datasource `{name}` has no provider"))
            .with_help("add `provider = \"postgresql\"` or `provider = \"mongodb\"`")
    }

    /// More than one datasource.
    #[must_use]
    pub fn second_datasource(span: Span) -> Self {
        Self::error(span, "a schema has one datasource; remove this one")
    }

    /// An attribute that does not exist where it is used.
    #[must_use]
    pub fn unknown_attribute(span: Span, name: &str, did_you_mean: Option<&str>) -> Self {
        let d = Self::error(span, format!("unknown attribute `{name}`"));
        match did_you_mean {
            Some(other) => d.with_help(format!("did you mean `{other}`?")),
            None => d,
        }
    }

    /// The same attribute twice on one item.
    #[must_use]
    pub fn duplicate_attribute(span: Span, name: &str) -> Self {
        Self::error(span, format!("`{name}` is used more than once here"))
    }

    /// An attribute used wrongly; the message says how.
    #[must_use]
    pub fn invalid_attribute(span: Span, name: &str, message: impl std::fmt::Display) -> Self {
        Self::error(span, format!("`{name}`: {message}"))
    }

    /// Like [`Diagnostic::invalid_attribute`], with a hint.
    #[must_use]
    pub fn invalid_attribute_help(
        span: Span,
        name: &str,
        message: impl std::fmt::Display,
        help: impl Into<String>,
    ) -> Self {
        Self::invalid_attribute(span, name, message).with_help(help)
    }

    /// A model without any way to find one row.
    #[must_use]
    pub fn no_unique_criteria(span: Span, model: &str) -> Self {
        Self::error(span, format!("the model `{model}` has no `@id`, `@@id` or unique field"))
            .with_help("mark a required field with `@id` or `@unique`, or add `@@id([..])` or `@@unique([..])`")
    }

    /// Two models stored under one table or collection name.
    #[must_use]
    pub fn duplicate_table(span: Span, table: &str, other: &str) -> Self {
        Self::error(span, format!("`{other}` already uses the table `{table}`"))
            .with_help("give one of them another `@@map(\"..\")`")
    }

    /// A relation problem; the message says which.
    #[must_use]
    pub fn relation(span: Span, message: impl Into<String>) -> Self {
        Self::error(span, message)
    }

    /// Like [`Diagnostic::relation`], with a hint.
    #[must_use]
    pub fn relation_help(span: Span, message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::error(span, message).with_help(help)
    }

    /// A relation field whose other side is missing (SPEC-SCHEMA-001 AC-002).
    #[must_use]
    pub fn missing_opposite(span: Span, field: &str, model: &str, target: &str, add: &str) -> Self {
        Self::error(
            span,
            format!(
                "the relation field `{field}` on `{model}` has no opposite field on `{target}`"
            ),
        )
        .with_help(format!(
            "add `{add}` to the model `{target}`, or run `lumen format` to add it"
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
