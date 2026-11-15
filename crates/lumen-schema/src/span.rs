//! Where things are: a byte range inside one source file.

/// Which source file a span points into. Schemas may later be split over several files, so every
/// span carries one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct FileId(pub u32);

/// A byte range `start..end` in one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    /// The file.
    pub file: FileId,
    /// First byte.
    pub start: usize,
    /// One past the last byte.
    pub end: usize,
}

impl Span {
    /// A span in `file` from `start` to `end`.
    #[must_use]
    pub fn new(file: FileId, start: usize, end: usize) -> Self {
        Self { file, start, end }
    }

    /// The smallest span covering both.
    #[must_use]
    pub fn to(self, other: Span) -> Span {
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// One schema file and its text.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// The path or name shown in messages, for example `schema.prisma`.
    pub name: String,
    /// The full text.
    pub text: String,
    line_starts: Vec<usize>,
}

impl SourceFile {
    /// A file named `name` with `text`.
    #[must_use]
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        let text = text.into();
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        Self {
            name: name.into(),
            text,
            line_starts,
        }
    }

    /// The 1-based line and column (in characters) of byte `offset`.
    #[must_use]
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.text.len());
        let line = self
            .line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        let start = self.line_starts[line];
        let col = self.text[start..offset].chars().count();
        (line + 1, col + 1)
    }

    /// The text of 1-based line `line`, without its line break.
    #[must_use]
    pub fn line(&self, line: usize) -> &str {
        let Some(&start) = self.line_starts.get(line.saturating_sub(1)) else {
            return "";
        };
        let end = self
            .line_starts
            .get(line)
            .map_or(self.text.len(), |&next| next.saturating_sub(1));
        self.text[start..end.max(start)].trim_end_matches('\r')
    }

    /// The text a span covers.
    #[must_use]
    pub fn slice(&self, span: Span) -> &str {
        self.text
            .get(span.start..span.end.min(self.text.len()))
            .unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::SourceFile;

    #[test]
    fn lines_and_columns() {
        let f = SourceFile::new("s.prisma", "model A {\n  id Int\r\n}\n");
        assert_eq!(f.line_col(0), (1, 1));
        assert_eq!(f.line_col(12), (2, 3));
        assert_eq!(f.line(2), "  id Int");
        assert_eq!(f.line(3), "}");
        assert_eq!(f.line(9), "");
    }
}
