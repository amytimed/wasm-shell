use std::fmt;

/// Byte offset + length into the source string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub len: usize,
}

impl Span {
    pub fn new(start: usize, len: usize) -> Self {
        Self { start, len }
    }

    pub fn single(pos: usize) -> Self {
        Self { start: pos, len: 1 }
    }

    pub fn eof(pos: usize) -> Self {
        Self { start: pos, len: 0 }
    }

    pub fn end(&self) -> usize {
        self.start + self.len
    }

    pub fn merge(self, other: Span) -> Span {
        let start = self.start.min(other.start);
        let end = self.end().max(other.end());
        Span { start, len: end - start }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub span: Span,
    pub message: String,
}

impl ParseError {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self { span, message: message.into() }
    }

    /// Render a human-friendly error with source context.
    pub fn display_with_source<'a>(&'a self, source: &'a str) -> ErrorDisplay<'a> {
        ErrorDisplay { error: self, source }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "parse error at {}..{}: {}", self.span.start, self.span.end(), self.message)
    }
}

impl std::error::Error for ParseError {}

pub struct ErrorDisplay<'a> {
    error: &'a ParseError,
    source: &'a str,
}

impl<'a> fmt::Display for ErrorDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let span = self.error.span;
        // Find line number and column
        let before = &self.source[..span.start.min(self.source.len())];
        let line_num = before.chars().filter(|&c| c == '\n').count() + 1;
        let col = before.rfind('\n').map(|i| span.start - i - 1).unwrap_or(span.start);

        // Extract the offending line
        let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = self.source[line_start..]
            .find('\n')
            .map(|i| line_start + i)
            .unwrap_or(self.source.len());
        let line_text = &self.source[line_start..line_end];

        writeln!(f, "parse error (line {line_num}, col {col}): {}", self.error.message)?;
        writeln!(f, "  {line_text}")?;
        write!(f, "  {}{}", " ".repeat(col), "^".repeat(span.len.max(1)))
    }
}
