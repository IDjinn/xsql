//! Diagnostics with source spans, rendered `file:line:col` style with the
//! offending source line underneath.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    /// 1-based line.
    pub line: u32,
    /// 1-based column (in chars).
    pub col: u32,
}

impl Span {
    pub fn new(line: u32, col: u32) -> Self {
        Self { line, col }
    }
}

#[derive(Debug, Clone)]
pub struct XsqlError {
    pub message: String,
    pub span: Option<Span>,
}

impl XsqlError {
    pub fn spanned(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span: Some(span),
        }
    }

    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
        }
    }

    /// Renders the diagnostic against the script source it came from.
    pub fn render(&self, source_name: &str, source: &str) -> String {
        let mut out = format!("error: {}", self.message);
        if let Some(span) = self.span {
            out.push_str(&format!(
                "\n  --> {source_name}:{}:{}",
                span.line, span.col
            ));
            if let Some(line) = source.lines().nth(span.line as usize - 1) {
                let gutter = span.line.to_string();
                out.push_str(&format!("\n {gutter} | {line}"));
                let pad = " ".repeat(gutter.len() + 3 + span.col as usize - 1);
                out.push_str(&format!("\n{pad}^"));
            }
        }
        out
    }
}

impl fmt::Display for XsqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span {
            Some(span) => write!(f, "{}:{}: {}", span.line, span.col, self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

pub type Result<T> = std::result::Result<T, XsqlError>;
