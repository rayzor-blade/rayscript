//! Source positions and the diagnostics that point at them.

use std::fmt;
use std::io;
use std::ops::Range;

use ariadne::{CharSet, Color, Config, IndexType, Label, Report, ReportKind, Source};

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

impl Severity {
    pub fn kind(self) -> ReportKind<'static> {
        match self {
            Severity::Error => ReportKind::Error,
            Severity::Warning => ReportKind::Warning,
        }
    }

    pub fn color(self) -> Color {
        match self {
            Severity::Error => Color::Red,
            Severity::Warning => Color::Yellow,
        }
    }
}

/// A problem found in a script, with where it is and, when there is one, what to do.
#[derive(Clone, PartialEq, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Span,
    pub help: Option<String>,
    /// What is written under the span; `here` when there is none, since a bare underline is only colour.
    pub caption: Option<String>,
    /// Other places that explain this one, such as where an unclosed bracket was opened.
    pub labels: Vec<(Span, String)>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, span: Span) -> Diagnostic {
        Diagnostic { severity: Severity::Error, message: message.into(), span, help: None, caption: None, labels: Vec::new() }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Diagnostic {
        self.help = Some(help.into());
        self
    }

    pub fn with_caption(mut self, caption: impl Into<String>) -> Diagnostic {
        self.caption = Some(caption.into());
        self
    }

    pub fn with_label(mut self, span: Span, text: impl Into<String>) -> Diagnostic {
        self.labels.push((span, text.into()));
        self
    }

    /// The diagnostic as text for a log or an error message: no colour, and ASCII line drawing.
    pub fn render(&self, name: &str, source: &str) -> String {
        self.render_with(name, source, false)
    }

    /// The diagnostic with its source line, underlines and labels. `fancy` adds colour and Unicode line
    /// drawing, for a terminal.
    pub fn render_with(&self, name: &str, source: &str, fancy: bool) -> String {
        let mut out = Vec::new();
        let _ = self.write(name, source, fancy, &mut out);
        String::from_utf8_lossy(&out).into_owned()
    }

    pub fn write(&self, name: &str, source: &str, fancy: bool, out: &mut dyn io::Write) -> io::Result<()> {
        let id = name.to_string();
        let range = byte_range(self.span, source);
        let color = self.severity.color();
        let caption = self.caption.as_deref().unwrap_or("here");
        let mut report = Report::build(self.severity.kind(), (id.clone(), range.clone()))
            .with_config(config(fancy))
            .with_message(&self.message)
            .with_label(Label::new((id.clone(), range)).with_message(caption).with_color(color));
        for (span, text) in &self.labels {
            report = report.with_label(Label::new((id.clone(), byte_range(*span, source))).with_message(text).with_color(NOTE_COLOR));
        }
        if let Some(help) = &self.help {
            report = report.with_help(help);
        }
        report.finish().write((id, Source::from(source)), out)
    }
}

/// Every diagnostic, one after another.
pub fn render_all(diagnostics: &[Diagnostic], name: &str, source: &str, fancy: bool) -> String {
    diagnostics.iter().map(|d| d.render_with(name, source, fancy)).collect()
}

/// The colour of labels that explain a diagnostic rather than mark it.
pub const NOTE_COLOR: Color = Color::Blue;

/// The report settings every diagnostic shares: byte offsets, and `fancy` for colour and Unicode lines.
pub fn config(fancy: bool) -> Config {
    Config::default().with_index_type(IndexType::Byte).with_color(fancy).with_char_set(if fancy { CharSet::Unicode } else { CharSet::Ascii })
}

/// A span as a range of `source` that starts and ends on characters and covers at least one, so that an
/// empty span, such as the end of the file, still gets an underline.
pub fn byte_range(span: Span, source: &str) -> Range<usize> {
    let len = source.len();
    let mut start = (span.start as usize).min(len);
    while !source.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (span.end as usize).min(len).max(start);
    if end == start {
        end = source[start..].chars().next().map_or(start, |c| start + c.len_utf8());
    }
    while !source.is_char_boundary(end) {
        end += 1;
    }
    start..end
}
