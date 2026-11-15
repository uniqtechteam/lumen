//! `lumen format`: lay a schema out in the canonical style. Works line by line on tokens, so
//! comments are kept exactly and a broken line is echoed as written without disturbing the rest.
//!
//! - two-space indent, one blank line between blocks, no runs of blank lines;
//! - fields as a table: name, type and attributes in aligned columns, a blank line starts a new
//!   table; settings aligned on `=`;
//! - field attributes in the order `@id @unique @default @updatedAt @map @relation @db.X @ignore`;
//! - block attributes after the fields, after one blank line, in the order
//!   `@@id @@unique @@index @@fulltext @@map @@schema @@ignore`;
//! - canonical spacing inside values: `[a, b]`, `name: value`, `{ key: value }`.

use std::fmt::Write as _;

use crate::ast::{Arity, Top};
use crate::checked::{FieldKind, Schema};
use crate::diagnostics::Diagnostics;
use crate::lexer::{Token, TokenKind as T, lex};
use crate::span::{FileId, SourceFile};

const FIELD_ORDER: [&str; 6] = ["id", "unique", "default", "updatedAt", "map", "relation"];
const BLOCK_ORDER: [&str; 7] = [
    "id", "unique", "index", "fulltext", "map", "schema", "ignore",
];

/// One line inside a block.
enum Line {
    Blank,
    /// A comment on its own line, as written.
    Comment(String),
    /// A field or enum value: name, type (empty for enum values), attributes, trailing comment.
    Row(String, String, Vec<String>, Option<String>),
    /// `key = value`.
    Setting(String, String, Option<String>),
    /// `@@attr(..)` with the comment lines just above it.
    BlockAttribute(Vec<String>, String, Option<String>),
    /// Anything else, echoed as written.
    Raw(String),
}

struct Formatter<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    pos: usize,
}

fn needs_space(prev: &T, next: &T) -> bool {
    let wordish = |t: &T| matches!(t, T::Ident(_) | T::Number(_) | T::String(_));
    match (prev, next) {
        (T::LBrace, T::RBrace) => false,
        (T::Comma | T::Colon | T::Equals | T::LBrace, _)
        | (_, T::Equals | T::LBrace | T::RBrace) => true,
        (T::At | T::AtAt | T::Dot | T::LParen | T::LBracket, _)
        | (
            _,
            T::Dot
            | T::LParen
            | T::RParen
            | T::LBracket
            | T::RBracket
            | T::Comma
            | T::Colon
            | T::Question,
        ) => false,
        (a, b) => wordish(a) && wordish(b) || matches!(b, T::At),
    }
}

impl Formatter<'_> {
    fn raw(&self, token: &Token) -> &str {
        &self.text[token.span.start..token.span.end]
    }

    /// Tokens joined with canonical spacing; strings and numbers as written.
    fn join(&self, tokens: &[Token]) -> String {
        let mut out = String::new();
        let mut prev: Option<&T> = None;
        for t in tokens {
            if let Some(p) = prev
                && needs_space(p, &t.kind)
            {
                out.push(' ');
            }
            out.push_str(self.raw(t));
            prev = Some(&t.kind);
        }
        out
    }

    /// The tokens of one logical line: up to a line break outside brackets, or a closing `}` of
    /// the block. The line break itself is consumed.
    fn line(&mut self) -> Vec<Token> {
        let mut depth = 0usize;
        let mut out = Vec::new();
        loop {
            let t = self.tokens[self.pos].clone();
            match t.kind {
                T::Eof => return out,
                T::Newline if depth == 0 => {
                    self.pos += 1;
                    return out;
                }
                T::RBrace if depth == 0 => return out,
                T::Newline => {
                    self.pos += 1;
                    continue;
                }
                T::LParen | T::LBracket | T::LBrace => depth += 1,
                T::RParen | T::RBracket | T::RBrace => depth = depth.saturating_sub(1),
                _ => {}
            }
            out.push(t);
            self.pos += 1;
        }
    }

    /// The original text of a line, trimmed.
    fn source_of(&self, tokens: &[Token]) -> String {
        match (tokens.first(), tokens.last()) {
            (Some(a), Some(b)) => self.text[a.span.start..b.span.end].trim().to_owned(),
            _ => String::new(),
        }
    }

    fn split_trailing(&self, tokens: &[Token]) -> (Vec<Token>, Option<String>) {
        match tokens.last() {
            Some(t) if matches!(t.kind, T::Comment(_)) => (
                tokens[..tokens.len() - 1].to_vec(),
                Some(self.raw(t).to_owned()),
            ),
            _ => (tokens.to_vec(), None),
        }
    }

    /// Attributes starting at each `@`, joined, in canonical order.
    fn attributes(&self, tokens: &[Token]) -> Option<Vec<String>> {
        if tokens.is_empty() {
            return Some(Vec::new());
        }
        if tokens[0].kind != T::At {
            return None;
        }
        let mut groups: Vec<Vec<Token>> = Vec::new();
        let mut depth = 0usize;
        for t in tokens {
            if t.kind == T::At && depth == 0 {
                groups.push(Vec::new());
            }
            match t.kind {
                T::LParen | T::LBracket | T::LBrace => depth += 1,
                T::RParen | T::RBracket | T::RBrace => depth = depth.saturating_sub(1),
                _ => {}
            }
            groups.last_mut()?.push(t.clone());
        }
        let mut attrs: Vec<(usize, String)> = groups
            .iter()
            .map(|g| {
                let name = match g.get(1).map(|t| &t.kind) {
                    Some(T::Ident(n)) => n.clone(),
                    _ => String::new(),
                };
                let rank = match FIELD_ORDER.iter().position(|o| *o == name) {
                    Some(i) => i,
                    None if g.get(2).map(|t| &t.kind) == Some(&T::Dot) => FIELD_ORDER.len(),
                    None if name == "ignore" => FIELD_ORDER.len() + 1,
                    None => FIELD_ORDER.len() + 2,
                };
                (rank, self.join(g))
            })
            .collect();
        attrs.sort_by_key(|(rank, _)| *rank);
        Some(attrs.into_iter().map(|(_, text)| text).collect())
    }

    /// A field line: name, type, attributes.
    fn row(&self, tokens: &[Token], with_type: bool) -> Option<(String, String, Vec<String>)> {
        let T::Ident(name) = &tokens.first()?.kind else {
            return None;
        };
        let rest = &tokens[1..];
        let type_end = rest
            .iter()
            .position(|t| t.kind == T::At)
            .unwrap_or(rest.len());
        let (ty, attrs) = rest.split_at(type_end);
        if with_type {
            // a type is a name, maybe `Unsupported("..")`, then `?` or `[]`
            let ok = matches!(ty.first().map(|t| &t.kind), Some(T::Ident(_)))
                && ty[1..].iter().all(|t| {
                    matches!(
                        t.kind,
                        T::Question
                            | T::LBracket
                            | T::RBracket
                            | T::LParen
                            | T::RParen
                            | T::String(_)
                    )
                });
            if !ok {
                return None;
            }
        } else if !ty.is_empty() {
            return None;
        }
        Some((name.clone(), self.join(ty), self.attributes(attrs)?))
    }

    fn block_lines(&mut self, kind: &str) -> Vec<Line> {
        let mut lines = Vec::new();
        loop {
            match self.tokens[self.pos].kind {
                T::RBrace => {
                    self.pos += 1;
                    return lines;
                }
                T::Eof => return lines,
                _ => {}
            }
            let tokens = self.line();
            let (body, trailing) = self.split_trailing(&tokens);
            let line = if tokens.is_empty() {
                Line::Blank
            } else if body.is_empty() {
                Line::Comment(trailing.unwrap_or_default())
            } else if body
                .iter()
                .all(|t| matches!(t.kind, T::Comment(_) | T::DocComment(_)))
            {
                Line::Comment(self.source_of(&tokens))
            } else if body[0].kind == T::DocComment(String::new())
                || matches!(body[0].kind, T::DocComment(_))
            {
                Line::Raw(self.source_of(&tokens))
            } else if body[0].kind == T::AtAt {
                Line::BlockAttribute(Vec::new(), self.join(&body), trailing)
            } else if kind == "datasource" || kind == "generator" {
                match body.iter().position(|t| t.kind == T::Equals) {
                    Some(1) if matches!(body[0].kind, T::Ident(_)) && body.len() > 2 => {
                        Line::Setting(
                            self.raw(&body[0]).to_owned(),
                            self.join(&body[2..]),
                            trailing,
                        )
                    }
                    _ => Line::Raw(self.source_of(&tokens)),
                }
            } else {
                match self.row(&body, kind != "enum") {
                    Some((name, ty, attrs)) => Line::Row(name, ty, attrs, trailing),
                    None => Line::Raw(self.source_of(&tokens)),
                }
            };
            lines.push(line);
        }
    }
}

/// Fields and other lines, then block attributes; runs of blank lines collapsed.
fn arrange(lines: Vec<Line>) -> (Vec<Line>, Vec<Line>) {
    let mut body: Vec<Line> = Vec::new();
    let mut attrs: Vec<(usize, Line)> = Vec::new();
    for line in lines {
        if let Line::BlockAttribute(_, text, trailing) = line {
            // comment lines right above an attribute move with it
            let mut comments = Vec::new();
            while let Some(Line::Comment(_)) = body.last() {
                if let Some(Line::Comment(c)) = body.pop() {
                    comments.insert(0, c);
                }
            }
            let name = text.trim_start_matches('@');
            let name = name
                .split(|c: char| c == '(' || c.is_whitespace())
                .next()
                .unwrap_or("");
            let rank = BLOCK_ORDER
                .iter()
                .position(|o| *o == name)
                .unwrap_or(BLOCK_ORDER.len());
            attrs.push((rank, Line::BlockAttribute(comments, text, trailing)));
        } else {
            if matches!(line, Line::Blank) && matches!(body.last(), None | Some(Line::Blank)) {
                continue;
            }
            body.push(line);
        }
    }
    while matches!(body.last(), Some(Line::Blank)) {
        body.pop();
    }
    attrs.sort_by_key(|(rank, _)| *rank);
    (body, attrs.into_iter().map(|(_, l)| l).collect())
}

fn width(s: &str) -> usize {
    s.chars().count()
}

/// The rows of a table group, aligned.
fn render_body(body: &[Line], out: &mut String) {
    let mut i = 0;
    while i < body.len() {
        // a group runs to the next blank line
        let end = body[i..]
            .iter()
            .position(|l| matches!(l, Line::Blank))
            .map_or(body.len(), |p| i + p);
        let group = &body[i..end];
        let name_w = group
            .iter()
            .filter_map(|l| match l {
                Line::Row(n, ..) | Line::Setting(n, ..) => Some(width(n)),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let type_w = group
            .iter()
            .filter_map(|l| match l {
                Line::Row(_, t, ..) => Some(width(t)),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let any_attrs = group
            .iter()
            .any(|l| matches!(l, Line::Row(_, _, a, _) if !a.is_empty()));
        for line in group {
            let mut text = String::from("  ");
            match line {
                Line::Row(name, ty, attrs, trailing) => {
                    let _ = write!(text, "{name:<name_w$}");
                    if !ty.is_empty() {
                        text.push(' ');
                        text.push_str(ty);
                    }
                    if !attrs.is_empty() {
                        let pad = if ty.is_empty() {
                            name_w.saturating_sub(width(name))
                        } else {
                            type_w - width(ty)
                        };
                        text.push_str(&" ".repeat(pad));
                        text.push(' ');
                        text.push_str(&attrs.join(" "));
                    } else if any_attrs && trailing.is_some() && !ty.is_empty() {
                        text.push_str(&" ".repeat(type_w - width(ty)));
                    }
                    if let Some(c) = trailing {
                        text = text.trim_end().to_owned();
                        text.push(' ');
                        text.push_str(c);
                    }
                }
                Line::Setting(key, value, trailing) => {
                    let _ = write!(text, "{key:<name_w$} = {value}");
                    if let Some(c) = trailing {
                        text.push(' ');
                        text.push_str(c);
                    }
                }
                Line::Comment(c) | Line::Raw(c) => text.push_str(c),
                Line::Blank | Line::BlockAttribute(..) => {}
            }
            out.push_str(text.trim_end());
            out.push('\n');
        }
        if end < body.len() {
            out.push('\n');
            i = end + 1;
        } else {
            i = end;
        }
    }
}

fn render_block(header: &str, lines: Vec<Line>, out: &mut String) {
    let (body, attrs) = arrange(lines);
    out.push_str(header);
    out.push_str(" {\n");
    render_body(&body, out);
    if !attrs.is_empty() && !body.is_empty() {
        out.push('\n');
    }
    for line in attrs {
        if let Line::BlockAttribute(comments, text, trailing) = line {
            for c in comments {
                out.push_str("  ");
                out.push_str(&c);
                out.push('\n');
            }
            out.push_str("  ");
            out.push_str(&text);
            if let Some(c) = trailing {
                out.push(' ');
                out.push_str(&c);
            }
            out.push('\n');
        }
    }
    out.push_str("}\n");
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_lowercase().chain(c).collect())
}

/// The opposite field to add for an unpaired relation field, with its foreign key when needed.
fn opposite_lines(schema: &Schema, model: usize, field: usize) -> Option<(usize, Vec<String>)> {
    let m = &schema.models[model];
    let f = &m.fields[field];
    let FieldKind::Model(target) = f.kind else {
        return None;
    };
    let t = &schema.models[target.0];
    // the other model already points back: not a missing side
    if t.fields
        .iter()
        .any(|g| g.kind == FieldKind::Model(crate::checked::ModelId(model)))
    {
        return None;
    }
    let base = lower_first(&m.name);
    if f.arity == Arity::List {
        // `posts Post[]` on User: Post gets `user User? @relation(..)` and `userId`
        let pk = m.primary_key()?;
        let [pk_field] = pk.fields.as_slice() else {
            return None;
        };
        let key = &m.fields[pk_field.field];
        let FieldKind::Scalar(scalar) = key.kind else {
            return None;
        };
        let fk = format!("{base}{}", capitalize(&key.name));
        if t.field(&base).is_some() || t.field(&fk).is_some() {
            return None;
        }
        let native = key.native_type.as_ref().map_or_else(String::new, |n| {
            let ds = schema.datasource.as_ref().map_or("db", |d| d.name.as_str());
            let args = if n.args.is_empty() {
                String::new()
            } else {
                format!(
                    "({})",
                    n.args
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            format!(" @{ds}.{}{args}", n.name)
        });
        Some((
            target.0,
            vec![
                format!(
                    "{base} {}? @relation(fields: [{fk}], references: [{}])",
                    m.name, key.name
                ),
                format!("{fk} {}?{native}", scalar.name()),
            ],
        ))
    } else {
        let name = format!("{base}s");
        if t.field(&name).is_some() {
            return None;
        }
        Some((target.0, vec![format!("{name} {}[]", m.name)]))
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// Add the missing opposite side of relations, as text, so the layout step can align it. Only done
/// when the file parses cleanly.
pub(crate) fn complete(file: &SourceFile) -> String {
    let (ast, mut d) = crate::parse(file);
    if d.has_errors() {
        return file.text.clone();
    }
    let schema = crate::checked::check(&ast, &mut d);
    // like Prisma, complete only a schema whose only errors are the missing sides themselves
    let other_errors = d.all().iter().any(|e| {
        e.severity == crate::diagnostics::Severity::Error
            && !e
                .help
                .as_deref()
                .is_some_and(|h| h.contains("lumen format"))
    });
    if other_errors {
        return file.text.clone();
    }
    let model_asts: Vec<_> = ast
        .tops
        .iter()
        .filter_map(|t| match t {
            Top::Model(m) if m.kind != crate::ast::ModelKind::CompositeType => Some(m),
            _ => None,
        })
        .collect();
    // (byte offset, text) to insert, later offsets first
    let mut inserts: Vec<(usize, String)> = Vec::new();
    for (mi, m) in schema.models.iter().enumerate() {
        for (fi, field) in m.fields.iter().enumerate() {
            if field.relation.is_some() {
                continue;
            }
            let Some((target, lines)) = opposite_lines(&schema, mi, fi) else {
                continue;
            };
            let block = model_asts[target];
            let at = block.fields.last().map_or(block.span.end - 1, |last| {
                file.text[last.span.end..]
                    .find('\n')
                    .map_or(file.text.len(), |n| last.span.end + n + 1)
            });
            let text = lines.iter().fold(String::new(), |mut acc, l| {
                let _ = writeln!(acc, "  {l}");
                acc
            });
            let text = if block.fields.is_empty() {
                format!("\n{text}")
            } else {
                text
            };
            inserts.push((at, text));
        }
    }
    inserts.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    let mut out = file.text.clone();
    for (at, text) in inserts {
        out.insert_str(at, &text);
    }
    out
}

/// The file in the canonical layout. Problems found while reading go into `diagnostics`; the text
/// is formatted as far as it can be, with broken lines kept as written.
#[must_use]
pub fn format_text(file: &SourceFile, diagnostics: &mut Diagnostics) -> String {
    let completed = complete(file);
    let tokens = lex(&completed, FileId(0), diagnostics);
    let mut f = Formatter {
        text: &completed,
        tokens,
        pos: 0,
    };
    let mut out = String::new();
    let mut pending_blank = false;
    loop {
        let start = f.pos;
        let first = f.tokens[start].kind.clone();
        if first == T::Eof {
            break;
        }
        let keyword = match &first {
            T::Ident(k) => k.clone(),
            _ => String::new(),
        };
        let is_block = matches!(
            keyword.as_str(),
            "model" | "view" | "type" | "enum" | "datasource" | "generator"
        ) && matches!(f.tokens.get(start + 1).map(|t| &t.kind), Some(T::Ident(_)))
            && f.tokens.get(start + 2).map(|t| &t.kind) == Some(&T::LBrace);
        if is_block {
            let name = f.raw(&f.tokens[start + 1].clone()).to_owned();
            f.pos += 3;
            // the rest of the header line
            if f.tokens[f.pos].kind == T::Newline {
                f.pos += 1;
            }
            if pending_blank && !out.is_empty() {
                out.push('\n');
            }
            let lines = f.block_lines(&keyword);
            render_block(&format!("{keyword} {name}"), lines, &mut out);
            pending_blank = true;
            continue;
        }
        let tokens = f.line();
        if tokens.is_empty() {
            if f.tokens[f.pos].kind == T::RBrace {
                f.pos += 1; // a stray `}` at the top level
            }
            pending_blank = !out.is_empty();
            continue;
        }
        if pending_blank && !out.is_empty() {
            out.push('\n');
        }
        pending_blank = false;
        out.push_str(&f.source_of(&tokens));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::format_text;
    use crate::diagnostics::Diagnostics;
    use crate::span::SourceFile;

    fn fmt(text: &str) -> String {
        format_text(
            &SourceFile::new("s.prisma", text),
            &mut Diagnostics::default(),
        )
    }

    #[test]
    fn aligns_orders_and_spaces() {
        let input = "model Post {\n  id Int @default(autoincrement()) @id\n  published Boolean @map(\"p\") @default(false)\n  @@map(\"posts\")\n  @@index([published],where:{published:true})\n}\n";
        assert_eq!(
            fmt(input),
            "model Post {\n  id        Int     @id @default(autoincrement())\n  published Boolean @default(false) @map(\"p\")\n\n  @@index([published], where: { published: true })\n  @@map(\"posts\")\n}\n"
        );
    }

    #[test]
    fn settings_align_per_group_and_comments_stay() {
        let input = "generator g {\n  provider = \"lumen\"\n\n  // ours\n  output = \"x\" // note\n  previewFeatures = []\n}\n";
        assert_eq!(
            fmt(input),
            "generator g {\n  provider = \"lumen\"\n\n  // ours\n  output          = \"x\" // note\n  previewFeatures = []\n}\n"
        );
    }

    #[test]
    fn missing_relation_sides_are_added() {
        let input = "model User {\n  id Int @id\n}\n\nmodel Post {\n  id Int @id\n  authorId Int\n  author User @relation(fields: [authorId], references: [id])\n}\n";
        let out = fmt(input);
        assert!(
            out.contains("model User {\n  id    Int    @id\n  posts Post[]\n}"),
            "{out}"
        );
        let input = "model User {\n  id Int @id\n  posts Post[]\n}\n\nmodel Post {\n  id String @id\n\n  @@map(\"posts\")\n}\n";
        let out = fmt(input);
        assert!(
            out.contains("  id     String @id\n  user   User?  @relation(fields: [userId], references: [id])\n  userId Int?\n\n  @@map(\"posts\")"),
            "{out}"
        );
    }

    #[test]
    fn broken_lines_are_kept_and_do_not_change_the_table() {
        let input =
            "model Post {\n  id   Int @id\n  this is an invalid line\n  anotherField String\n}\n";
        assert_eq!(
            fmt(input),
            "model Post {\n  id           Int    @id\n  this is an invalid line\n  anotherField String\n}\n"
        );
    }
}
