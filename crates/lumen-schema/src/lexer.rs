//! Turn schema text into tokens. Line breaks are tokens too: inside a block, one line is one field,
//! enum value or block attribute.

use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::span::{FileId, Span};

/// What a token is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// A name: `model`, `User`, `autoincrement`, `VarChar`.
    Ident(String),
    /// `42`, `-1`, `3.14`, as written.
    Number(String),
    /// A string's value, with escapes resolved.
    String(String),
    /// `/// text`, the text after the slashes.
    DocComment(String),
    /// `// text`, the text after the slashes.
    Comment(String),
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `=`
    Equals,
    /// `?`
    Question,
    /// `@`
    At,
    /// `@@`
    AtAt,
    /// `.`
    Dot,
    /// A line break.
    Newline,
    /// The end of the file.
    Eof,
}

impl TokenKind {
    /// How the token is named in "expected …, found …" messages.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Ident(name) => format!("`{name}`"),
            Self::Number(n) => format!("the number `{n}`"),
            Self::String(_) => "a string".to_owned(),
            Self::DocComment(_) | Self::Comment(_) => "a comment".to_owned(),
            Self::LBrace => "`{`".to_owned(),
            Self::RBrace => "`}`".to_owned(),
            Self::LParen => "`(`".to_owned(),
            Self::RParen => "`)`".to_owned(),
            Self::LBracket => "`[`".to_owned(),
            Self::RBracket => "`]`".to_owned(),
            Self::Comma => "`,`".to_owned(),
            Self::Colon => "`:`".to_owned(),
            Self::Equals => "`=`".to_owned(),
            Self::Question => "`?`".to_owned(),
            Self::At => "`@`".to_owned(),
            Self::AtAt => "`@@`".to_owned(),
            Self::Dot => "`.`".to_owned(),
            Self::Newline => "the end of the line".to_owned(),
            Self::Eof => "the end of the file".to_owned(),
        }
    }
}

/// A token and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What it is.
    pub kind: TokenKind,
    /// Where it is.
    pub span: Span,
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

struct Lexer<'a> {
    text: &'a str,
    file: FileId,
    pos: usize,
    tokens: Vec<Token>,
    diagnostics: &'a mut Diagnostics,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.text[self.pos..].chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn span(&self, start: usize) -> Span {
        Span::new(self.file, start, self.pos)
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        let span = self.span(start);
        self.tokens.push(Token { kind, span });
    }

    fn take_while(&mut self, keep: impl Fn(char) -> bool) -> &str {
        let start = self.pos;
        while self.peek().is_some_and(&keep) {
            self.bump();
        }
        &self.text[start..self.pos]
    }

    fn comment(&mut self, start: usize) {
        self.pos += 2; // the two slashes
        let doc = self.peek() == Some('/');
        if doc {
            self.bump();
        }
        let body = self.take_while(|c| c != '\n').trim_end_matches('\r');
        let body = body.strip_prefix(' ').unwrap_or(body).to_owned();
        let kind = if doc {
            TokenKind::DocComment(body)
        } else {
            TokenKind::Comment(body)
        };
        self.push(kind, start);
    }

    /// `/* .. */`, or `/** .. */` as documentation. The leading `*` of each line is dropped.
    fn block_comment(&mut self, start: usize) {
        self.pos += 2; // /*
        let doc = self.peek() == Some('*') && self.peek_at(1) != Some('/');
        let body_start = if doc { self.pos + 1 } else { self.pos };
        let Some(close) = self.text[self.pos..].find("*/") else {
            self.pos = self.text.len();
            self.diagnostics
                .push(Diagnostic::unterminated_comment(self.span(start)));
            return;
        };
        let body = &self.text[body_start.min(self.pos + close)..self.pos + close];
        self.pos += close + 2;
        let lines: Vec<&str> = body
            .lines()
            .map(|l| {
                let l = l.trim();
                l.strip_prefix("* ")
                    .or_else(|| l.strip_prefix('*'))
                    .unwrap_or(l)
            })
            .collect();
        let first = lines
            .iter()
            .position(|l| !l.is_empty())
            .unwrap_or(lines.len());
        let last = lines
            .iter()
            .rposition(|l| !l.is_empty())
            .map_or(first, |i| i + 1);
        let text = lines[first..last].join("\n");
        let kind = if doc {
            TokenKind::DocComment(text)
        } else {
            TokenKind::Comment(text)
        };
        self.push(kind, start);
    }

    fn string(&mut self, start: usize) {
        self.bump(); // opening quote
        let mut value = String::new();
        loop {
            let Some(c) = self.peek() else {
                self.diagnostics
                    .push(Diagnostic::unterminated_string(self.span(start)));
                break;
            };
            if c == '\n' {
                self.diagnostics
                    .push(Diagnostic::unterminated_string(self.span(start)));
                break;
            }
            self.bump();
            match c {
                '"' => break,
                '\\' => self.escape(&mut value),
                other => value.push(other),
            }
        }
        self.push(TokenKind::String(value), start);
    }

    fn escape(&mut self, value: &mut String) {
        let at = self.pos - 1;
        match self.bump() {
            Some('"') => value.push('"'),
            Some('\\') => value.push('\\'),
            Some('n') => value.push('\n'),
            Some('r') => value.push('\r'),
            Some('t') => value.push('\t'),
            Some('u') if self.peek() == Some('{') => {
                self.bump();
                let hex = self.take_while(|c| c.is_ascii_hexdigit()).to_owned();
                if self.peek() == Some('}') {
                    self.bump();
                }
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(c) => value.push(c),
                    None => self
                        .diagnostics
                        .push(Diagnostic::unknown_escape(self.span(at), 'u')),
                }
            }
            Some(other) => {
                self.diagnostics
                    .push(Diagnostic::unknown_escape(self.span(at), other));
                value.push(other);
            }
            None => {}
        }
    }

    fn number(&mut self, start: usize) {
        if self.peek() == Some('-') {
            self.bump();
        }
        self.take_while(|c| c.is_ascii_digit());
        if self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            self.take_while(|c| c.is_ascii_digit());
        }
        let text = self.text[start..self.pos].to_owned();
        self.push(TokenKind::Number(text), start);
    }

    fn run(&mut self) {
        while let Some(c) = self.peek() {
            let start = self.pos;
            match c {
                '\n' => {
                    self.bump();
                    self.push(TokenKind::Newline, start);
                }
                c if c.is_whitespace() => {
                    self.bump();
                }
                '/' if self.peek_at(1) == Some('/') => self.comment(start),
                '/' if self.peek_at(1) == Some('*') => self.block_comment(start),
                '"' => self.string(start),
                c if c.is_ascii_digit() => self.number(start),
                '-' if self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) => self.number(start),
                c if is_ident_start(c) => {
                    let name = self.take_while(is_ident_char).to_owned();
                    self.push(TokenKind::Ident(name), start);
                }
                '@' => {
                    self.bump();
                    if self.peek() == Some('@') {
                        self.bump();
                        self.push(TokenKind::AtAt, start);
                    } else {
                        self.push(TokenKind::At, start);
                    }
                }
                _ => {
                    self.bump();
                    let kind = match c {
                        '{' => TokenKind::LBrace,
                        '}' => TokenKind::RBrace,
                        '(' => TokenKind::LParen,
                        ')' => TokenKind::RParen,
                        '[' => TokenKind::LBracket,
                        ']' => TokenKind::RBracket,
                        ',' => TokenKind::Comma,
                        ':' => TokenKind::Colon,
                        '=' => TokenKind::Equals,
                        '?' => TokenKind::Question,
                        '.' => TokenKind::Dot,
                        other => {
                            self.diagnostics
                                .push(Diagnostic::unexpected_character(self.span(start), other));
                            continue;
                        }
                    };
                    self.push(kind, start);
                }
            }
        }
        let end = self.pos;
        self.push(TokenKind::Eof, end);
    }
}

/// The tokens of `text`; problems go into `diagnostics` and lexing continues past them.
#[must_use]
pub fn lex(text: &str, file: FileId, diagnostics: &mut Diagnostics) -> Vec<Token> {
    let mut lexer = Lexer {
        text,
        file,
        pos: 0,
        tokens: Vec::new(),
        diagnostics,
    };
    lexer.run();
    lexer.tokens
}

#[cfg(test)]
mod tests {
    use super::{TokenKind as T, lex};
    use crate::diagnostics::Diagnostics;
    use crate::span::FileId;

    fn kinds(text: &str) -> (Vec<T>, Diagnostics) {
        let mut d = Diagnostics::default();
        let tokens = lex(text, FileId(0), &mut d);
        (tokens.into_iter().map(|t| t.kind).collect(), d)
    }

    #[test]
    fn a_field_line() {
        let (k, d) = kinds("email String? @db.VarChar(255) // note\n");
        assert!(d.is_empty());
        assert_eq!(
            k,
            [
                T::Ident("email".into()),
                T::Ident("String".into()),
                T::Question,
                T::At,
                T::Ident("db".into()),
                T::Dot,
                T::Ident("VarChar".into()),
                T::LParen,
                T::Number("255".into()),
                T::RParen,
                T::Comment("note".into()),
                T::Newline,
                T::Eof,
            ]
        );
    }

    #[test]
    fn strings_numbers_and_doc_comments() {
        let (k, d) = kinds("/// The id\n@default(\"a\\\"b\\u{e9}\") -1 2.5 @@map");
        assert!(d.is_empty());
        assert_eq!(k[0], T::DocComment("The id".into()));
        assert_eq!(k[5], T::String("a\"bé".into()));
        assert_eq!(k[7], T::Number("-1".into()));
        assert_eq!(k[8], T::Number("2.5".into()));
        assert_eq!(k[9], T::AtAt);
    }

    #[test]
    fn block_comments_and_unicode_names() {
        let (k, d) = kinds("/**\n * Doc for\n * a model.\n */ model Lööp /* note */\n");
        assert!(d.is_empty());
        assert_eq!(k[0], T::DocComment("Doc for\na model.".into()));
        assert_eq!(k[2], T::Ident("Lööp".into()));
        assert_eq!(k[3], T::Comment("note".into()));
    }

    #[test]
    fn problems_are_reported_and_lexing_goes_on() {
        let (k, d) = kinds("a $ \"open\nb");
        assert_eq!(d.all().len(), 2);
        assert!(d.all()[0].message.contains('$'));
        assert!(d.all()[1].message.contains("closing"));
        assert_eq!(k.last(), Some(&T::Eof));
        assert!(k.contains(&T::Ident("b".into())));
    }
}
