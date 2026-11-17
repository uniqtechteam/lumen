//! Recursive descent from tokens to the syntax tree. A broken line becomes one diagnostic and
//! parsing goes on with the next line, so one typo never hides the rest of the file.

use crate::ast::{
    Argument, Arity, Attribute, Comments, ConfigBlock, ConfigKind, Enum, EnumValue, Expr, Field,
    FieldType, Ident, Model, ModelKind, Property, SchemaAst, Top,
};
use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::lexer::{Token, TokenKind as T};
use crate::span::Span;

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    diagnostics: &'a mut Diagnostics,
}

/// A line inside a block failed; the caller skips to the next line.
struct LineError;

type LineResult<V> = Result<V, LineError>;

impl Parser<'_> {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn kind(&self) -> &T {
        &self.peek().kind
    }

    fn nth_kind(&self, n: usize) -> &T {
        &self.tokens[(self.pos + n).min(self.tokens.len() - 1)].kind
    }

    fn bump(&mut self) -> Token {
        let token = self.peek().clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        token
    }

    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn error(&mut self, diagnostic: Diagnostic) -> LineError {
        self.diagnostics.push(diagnostic);
        LineError
    }

    fn expected(&mut self, what: &str) -> LineError {
        let token = self.peek().clone();
        self.error(Diagnostic::expected(
            token.span,
            what,
            &token.kind.describe(),
        ))
    }

    /// Skip the rest of a broken line, stopping before a `}` that closes the block.
    fn skip_line(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.kind() {
                T::Eof => return,
                T::Newline if depth == 0 => {
                    self.bump();
                    return;
                }
                T::RBrace if depth == 0 => return,
                T::LParen | T::LBracket | T::LBrace => depth += 1,
                T::RParen | T::RBracket | T::RBrace => depth = depth.saturating_sub(1),
                _ => {}
            }
            self.bump();
        }
    }

    /// Skip a broken top-level line; when it opens a block, skip the whole block.
    fn skip_top_level(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.kind() {
                T::Eof => return,
                T::Newline if depth == 0 => {
                    self.bump();
                    return;
                }
                T::LBrace => depth += 1,
                T::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        self.bump();
                        return;
                    }
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// Blank lines and comments before the next item.
    fn comments(&mut self) -> Comments {
        let mut comments = Comments::default();
        loop {
            match self.kind().clone() {
                T::Newline => {}
                T::DocComment(text) => comments.doc.push(text),
                T::Comment(text) => comments.leading.push(text),
                _ => return comments,
            }
            self.bump();
        }
    }

    /// The end of an item's line: an optional trailing comment, then a line break, `}` or the end.
    /// A trailing `//` comment is kept as `trailing`; a trailing `///` one is documentation and
    /// is added to `comments.doc`, after the `///` lines above the item.
    fn end_of_line(&mut self, kind: &str, mut comments: Comments) -> LineResult<Comments> {
        match self.kind().clone() {
            T::Comment(text) => {
                self.bump();
                comments.trailing = Some(text);
            }
            T::DocComment(text) => {
                self.bump();
                comments.doc.push(text);
            }
            _ => {}
        }
        match self.kind() {
            T::Newline => {
                self.bump();
                Ok(comments)
            }
            T::RBrace | T::Eof => Ok(comments),
            _ => {
                let span = self.peek().span;
                Err(self.error(Diagnostic::invalid_line(span, kind)))
            }
        }
    }

    fn ident(&mut self, what: &str) -> LineResult<Ident> {
        if let T::Ident(name) = self.kind().clone() {
            let span = self.bump().span;
            Ok(Ident { name, span })
        } else {
            Err(self.expected(what))
        }
    }

    /// `db.VarChar` and other dotted names.
    fn path(&mut self, what: &str) -> LineResult<Ident> {
        let first = self.ident(what)?;
        let mut name = first.name;
        let mut span = first.span;
        while *self.kind() == T::Dot && matches!(self.nth_kind(1), T::Ident(_)) {
            self.bump();
            let next = self.ident(what)?;
            name.push('.');
            name.push_str(&next.name);
            span = span.to(next.span);
        }
        Ok(Ident { name, span })
    }

    fn skip_newlines(&mut self) {
        while matches!(self.kind(), T::Newline | T::Comment(_) | T::DocComment(_)) {
            self.bump();
        }
    }

    /// The arguments after `(`, up to and including `)`. Line breaks are allowed inside.
    fn arguments(&mut self) -> LineResult<Vec<Argument>> {
        self.bump(); // (
        let mut args = Vec::new();
        loop {
            self.skip_newlines();
            if *self.kind() == T::RParen {
                self.bump();
                return Ok(args);
            }
            let start = self.peek().span;
            let name = if matches!(self.kind(), T::Ident(_)) && *self.nth_kind(1) == T::Colon {
                let name = self.ident("an argument name")?;
                self.bump(); // :
                Some(name)
            } else {
                None
            };
            let value = self.expr()?;
            args.push(Argument {
                span: start.to(value.span()),
                name,
                value,
            });
            self.skip_newlines();
            match self.kind() {
                T::Comma => {
                    self.bump();
                }
                T::RParen => {}
                _ => return Err(self.expected("`,` or `)`")),
            }
        }
    }

    fn expr(&mut self) -> LineResult<Expr> {
        let token = self.peek().clone();
        match token.kind {
            T::String(value) => {
                self.bump();
                Ok(Expr::String(value, token.span))
            }
            T::Number(value) => {
                self.bump();
                Ok(Expr::Number(value, token.span))
            }
            T::LBracket => {
                self.bump();
                let mut items = Vec::new();
                loop {
                    self.skip_newlines();
                    if *self.kind() == T::RBracket {
                        let end = self.bump().span;
                        return Ok(Expr::Array(items, token.span.to(end)));
                    }
                    items.push(self.expr()?);
                    self.skip_newlines();
                    match self.kind() {
                        T::Comma => {
                            self.bump();
                        }
                        T::RBracket => {}
                        _ => return Err(self.expected("`,` or `]`")),
                    }
                }
            }
            T::LBrace => {
                self.bump();
                let mut members = Vec::new();
                loop {
                    self.skip_newlines();
                    if *self.kind() == T::RBrace {
                        let end = self.bump().span;
                        return Ok(Expr::Object(members, token.span.to(end)));
                    }
                    let key = self.ident("a key")?;
                    if *self.kind() != T::Colon {
                        return Err(self.expected("`:`"));
                    }
                    self.bump();
                    members.push((key, self.expr()?));
                    self.skip_newlines();
                    match self.kind() {
                        T::Comma => {
                            self.bump();
                        }
                        T::RBrace => {}
                        _ => return Err(self.expected("`,` or `}`")),
                    }
                }
            }
            T::Ident(_) => {
                let name = self.path("a value")?;
                if *self.kind() == T::LParen {
                    let args = self.arguments()?;
                    let span = name.span.to(self.prev_span());
                    Ok(Expr::Function(name, args, span))
                } else {
                    Ok(Expr::Constant(name.name, name.span))
                }
            }
            _ => Err(self.expected("a value")),
        }
    }

    /// `@name` or `@@name`, with optional arguments.
    fn attribute(&mut self) -> LineResult<Attribute> {
        let at = self.bump().span;
        let name = self.path("an attribute name")?;
        let (args, has_parens) = if *self.kind() == T::LParen {
            (self.arguments()?, true)
        } else {
            (Vec::new(), false)
        };
        Ok(Attribute {
            span: at.to(self.prev_span()),
            name,
            args,
            has_parens,
        })
    }

    fn field_attributes(&mut self) -> LineResult<Vec<Attribute>> {
        let mut attributes = Vec::new();
        while *self.kind() == T::At {
            attributes.push(self.attribute()?);
        }
        Ok(attributes)
    }

    fn field_type(&mut self) -> LineResult<FieldType> {
        let name = self.ident("a type")?;
        let mut span = name.span;
        let mut unsupported = None;
        if name.name == "Unsupported" && *self.kind() == T::LParen {
            self.bump();
            match self.kind().clone() {
                T::String(text) => {
                    self.bump();
                    unsupported = Some(text);
                }
                _ => return Err(self.expected("the database type as a string")),
            }
            if *self.kind() != T::RParen {
                return Err(self.expected("`)`"));
            }
            span = span.to(self.bump().span);
        }
        let mut arity = Arity::Required;
        loop {
            match self.kind() {
                T::Question => {
                    span = span.to(self.bump().span);
                    if arity == Arity::List {
                        return Err(self.error(Diagnostic::optional_list(span)));
                    }
                    arity = Arity::Optional;
                }
                T::LBracket if *self.nth_kind(1) == T::RBracket => {
                    self.bump();
                    span = span.to(self.bump().span);
                    if arity == Arity::Optional {
                        return Err(self.error(Diagnostic::optional_list(span)));
                    }
                    arity = Arity::List;
                }
                _ => break,
            }
        }
        Ok(FieldType {
            name,
            unsupported,
            arity,
            span,
        })
    }

    fn field(&mut self, comments: Comments) -> LineResult<Field> {
        let name = self.ident("a field name")?;
        let ty = if matches!(self.kind(), T::Ident(_)) {
            Some(self.field_type()?)
        } else {
            if !matches!(
                self.kind(),
                T::At | T::Newline | T::RBrace | T::Eof | T::Comment(_) | T::DocComment(_)
            ) {
                return Err(self.expected("a type"));
            }
            self.diagnostics
                .push(Diagnostic::missing_field_type(name.span, &name.name));
            None
        };
        let attributes = self.field_attributes()?;
        let span = name.span.to(self.prev_span());
        let comments = self.end_of_line("model", comments)?;
        Ok(Field {
            name,
            ty,
            attributes,
            comments,
            span,
        })
    }

    /// `{`, the block's lines, `}`. `line` parses one line given its leading comments.
    fn block<L>(
        &mut self,
        kind: &str,
        name: &Ident,
        mut line: impl FnMut(&mut Self, Comments) -> LineResult<Option<L>>,
    ) -> LineResult<(Vec<L>, Vec<Attribute>, Span)> {
        if *self.kind() != T::LBrace {
            return Err(self.expected("`{`"));
        }
        self.bump();
        let mut items = Vec::new();
        let mut attributes = Vec::new();
        loop {
            let comments = self.comments();
            match self.kind() {
                T::RBrace => {
                    let end = self.bump().span;
                    return Ok((items, attributes, end));
                }
                T::Eof => {
                    self.diagnostics
                        .push(Diagnostic::unclosed_block(name.span, kind, &name.name));
                    return Ok((items, attributes, self.peek().span));
                }
                T::AtAt => match self.attribute() {
                    Ok(attribute) => {
                        attributes.push(attribute);
                        if self.end_of_line(kind, Comments::default()).is_err() {
                            self.skip_line();
                        }
                    }
                    Err(LineError) => self.skip_line(),
                },
                _ => match line(self, comments) {
                    Ok(Some(item)) => items.push(item),
                    Ok(None) => {}
                    Err(LineError) => self.skip_line(),
                },
            }
        }
    }

    fn model(&mut self, kind: ModelKind, comments: Comments) -> LineResult<Model> {
        let keyword = self.bump().span;
        let name = self.ident("a name")?;
        if kind == ModelKind::CompositeType && *self.kind() == T::Equals {
            let span = keyword.to(name.span);
            return Err(self.error(Diagnostic::type_alias(span, &name.name)));
        }
        let label = match kind {
            ModelKind::Model => "model",
            ModelKind::View => "view",
            ModelKind::CompositeType => "type",
        };
        let (fields, attributes, end) = self.block(label, &name.clone(), |p, c| {
            if matches!(p.kind(), T::Ident(_)) {
                p.field(c).map(Some)
            } else {
                let span = p.peek().span;
                Err(p.error(Diagnostic::invalid_line(span, label)))
            }
        })?;
        Ok(Model {
            kind,
            name,
            fields,
            attributes,
            comments,
            span: keyword.to(end),
        })
    }

    fn enum_block(&mut self, comments: Comments) -> LineResult<Enum> {
        let keyword = self.bump().span;
        let name = self.ident("an enum name")?;
        let (values, attributes, end) = self.block("enum", &name.clone(), |p, c| {
            let Ok(value) = p.ident("an enum value") else {
                return Err(LineError);
            };
            let attributes = p.field_attributes()?;
            let span = value.span.to(p.prev_span());
            let comments = p.end_of_line("enum", c)?;
            Ok(Some(EnumValue {
                name: value,
                attributes,
                comments,
                span,
            }))
        })?;
        Ok(Enum {
            name,
            values,
            attributes,
            comments,
            span: keyword.to(end),
        })
    }

    fn config(&mut self, kind: ConfigKind, comments: Comments) -> LineResult<ConfigBlock> {
        let keyword = self.bump().span;
        let name = self.ident("a name")?;
        let label = match kind {
            ConfigKind::Datasource => "datasource",
            ConfigKind::Generator => "generator",
        };
        let (properties, _attributes, end) = self.block(label, &name.clone(), |p, c| {
            let key = p.ident("a setting name")?;
            if *p.kind() != T::Equals {
                let span = p.peek().span;
                return Err(p.error(Diagnostic::invalid_line(span, label)));
            }
            p.bump();
            let value = p.expr()?;
            let span = key.span.to(value.span());
            let comments = p.end_of_line(label, c)?;
            Ok(Some(Property {
                name: key,
                value: Some(value),
                comments,
                span,
            }))
        })?;
        Ok(ConfigBlock {
            kind,
            name,
            properties,
            comments,
            span: keyword.to(end),
        })
    }

    fn schema(&mut self) -> SchemaAst {
        let mut ast = SchemaAst::default();
        loop {
            let comments = self.comments();
            let keyword = match self.kind() {
                T::Eof => {
                    ast.trailing_comments =
                        comments.doc.into_iter().chain(comments.leading).collect();
                    return ast;
                }
                T::Ident(word) => word.clone(),
                _ => String::new(),
            };
            let parsed = match keyword.as_str() {
                "model" => self.model(ModelKind::Model, comments).map(Top::Model),
                "view" => self.model(ModelKind::View, comments).map(Top::Model),
                "type" => self
                    .model(ModelKind::CompositeType, comments)
                    .map(Top::Model),
                "enum" => self.enum_block(comments).map(Top::Enum),
                "datasource" => self
                    .config(ConfigKind::Datasource, comments)
                    .map(Top::Config),
                "generator" => self
                    .config(ConfigKind::Generator, comments)
                    .map(Top::Config),
                _ => {
                    let token = self.peek().clone();
                    let found = match &token.kind {
                        T::Ident(word) => word.clone(),
                        other => other.describe().trim_matches('`').to_owned(),
                    };
                    Err(self.error(Diagnostic::unexpected_top_level(token.span, &found)))
                }
            };
            match parsed {
                Ok(top) => ast.tops.push(top),
                Err(LineError) => self.skip_top_level(),
            }
        }
    }
}

/// The syntax tree of `tokens`. It is always returned, even when there are errors.
pub fn parse_tokens(tokens: &[Token], diagnostics: &mut Diagnostics) -> SchemaAst {
    let mut parser = Parser {
        tokens,
        pos: 0,
        diagnostics,
    };
    parser.schema()
}
