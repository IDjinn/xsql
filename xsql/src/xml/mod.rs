pub mod dom;
pub mod encoding;
pub mod parse;
#[cfg(feature = "simd")]
pub mod parse_simd;
pub mod serialize;

/// XML parse failure carrying an optional byte offset into the source, so
/// callers (e.g. `xsql --check`) can render line/column context. `message`
/// has no position baked in; [`XmlParseError::describe`] gives the plain
/// one-line form.
#[derive(Debug, Clone)]
pub struct XmlParseError {
    pub message: String,
    pub byte: Option<usize>,
}

impl XmlParseError {
    /// One-line description with line/column when a position is known.
    pub fn describe(&self, source: &str) -> String {
        match self.line_col(source) {
            Some((line, col)) => {
                format!("malformed XML at line {line}, column {col}: {}", self.message)
            }
            None => format!("malformed XML: {}", self.message),
        }
    }

    /// 1-based line/column of the failure, if a position is known.
    pub fn line_col(&self, source: &str) -> Option<(usize, usize)> {
        self.byte.map(|byte| line_col(source, byte))
    }
}

/// 1-based line/column for a byte offset, for diagnostics. Columns count
/// bytes, which matches the offsets quick-xml reports.
pub fn line_col(source: &str, byte: usize) -> (usize, usize) {
    let (mut line, mut col) = (1usize, 1usize);
    for &b in &source.as_bytes()[..byte.min(source.len())] {
        if b == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
