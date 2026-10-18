//! Source positions and the diagnostics that point at them.

use std::fmt;

/// A half-open byte range of the source.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const fn new(start: u32, end: u32) -> Span {
        Span { start, end }
    }

    /// The smallest span holding both.
    pub fn to(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }

    pub fn len(self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn text(self, source: &str) -> &str {
        &source[self.start as usize..self.end as usize]
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// Maps byte offsets to lines and columns.
pub struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(source: &str) -> LineIndex {
        let mut starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i as u32 + 1);
            }
        }
        LineIndex { starts }
    }

    /// The zero-based line and column (in characters) of a byte offset.
    pub fn locate(&self, source: &str, offset: u32) -> (u32, u32) {
        let line = match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let start = self.starts[line] as usize;
        let end = (offset as usize).min(source.len());
        let col = source[start..end].chars().count() as u32;
        (line as u32, col)
    }

    pub fn line_text<'a>(&self, source: &'a str, line: u32) -> &'a str {
        let start = self.starts[line as usize] as usize;
        let end = self.starts.get(line as usize + 1).map(|e| *e as usize).unwrap_or(source.len());
        source[start..end].trim_end_matches(['\n', '\r'])
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    Warning,
}

/// A problem found in a script, with where it is and, when there is one, what to do.
#[derive(Clone, PartialEq, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Span,
    pub help: Option<String>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, span: Span) -> Diagnostic {
        Diagnostic { severity: Severity::Error, message: message.into(), span, help: None }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Diagnostic {
        self.help = Some(help.into());
        self
    }

    /// The diagnostic as a snippet: `file:line:col: error: message`, the line, and a caret under the span.
    pub fn render(&self, name: &str, source: &str) -> String {
        let index = LineIndex::new(source);
        let (line, col) = index.locate(source, self.span.start);
        let text = index.line_text(source, line);
        let width = if self.span.end > self.span.start {
            let (end_line, end_col) = index.locate(source, self.span.end);
            if end_line == line { (end_col - col).max(1) } else { text.chars().count() as u32 - col.min(text.chars().count() as u32) + 1 }
        } else {
            1
        };
        let kind = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let mut out = format!("{name}:{}:{}: {kind}: {}\n", line + 1, col + 1, self.message);
        out.push_str(&format!("  {text}\n  {}{}\n", " ".repeat(col as usize), "^".repeat(width as usize)));
        if let Some(help) = &self.help {
            out.push_str(&format!("  help: {help}\n"));
        }
        out
    }
}
