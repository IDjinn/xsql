//! Hand-rolled single-pass lexer.
//!
//! Notable rules:
//! - `;` terminates a statement block AND discards the rest of its line, so
//!   lines like `; free-form comment text` act as comments.
//! - After `USE`, the next bare token is lexed as a raw file path (dots and
//!   slashes allowed), or the keyword `INPUT`, or a quoted string.
//! - Backtick strings are raw multi-line XML payloads.

use crate::error::{Result, Span, XsqlError};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Path(String),
    Str(String),
    RawXml(String),
    Num(f64),

    Semi,
    Eq,
    NotEq,
    Lt,
    Gt,
    Le,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,

    Use,
    Input,
    Select,
    Replace,
    Insert,
    Delete,
    Set,
    Foreach,
    In,
    Where,
    Group,
    Raw,
    Xml,
    Ignore,
    Break,
    Into,
    And,
    Or,
    Not,

    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(name) => format!("identifier `{name}`"),
            Tok::Path(path) => format!("path `{path}`"),
            Tok::Str(_) => "string literal".into(),
            Tok::RawXml(_) => "raw XML literal".into(),
            Tok::Num(n) => format!("number `{n}`"),
            Tok::Semi => "`;`".into(),
            Tok::Eof => "end of input".into(),
            other => format!("`{other:?}`").to_uppercase(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub fn lex(source: &str) -> Result<Vec<Token>> {
    Lexer::new(source).run()
}

struct Lexer<'a> {
    chars: Vec<char>,
    pos: usize,
    line: u32,
    col: u32,
    /// Set right after a `USE` keyword: the next bare token is a file path.
    path_mode: bool,
    _source: &'a str,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
            path_mode: false,
            _source: source,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += 1;
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(ch)
    }

    fn span(&self) -> Span {
        Span::new(self.line, self.col)
    }

    fn run(mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        loop {
            while matches!(self.peek(), Some(c) if c.is_whitespace()) {
                self.bump();
            }
            let span = self.span();
            let Some(ch) = self.peek() else {
                tokens.push(Token { tok: Tok::Eof, span });
                return Ok(tokens);
            };

            let tok = if ch == ';' {
                // Terminator; the rest of the line is comment text.
                while matches!(self.peek(), Some(c) if c != '\n') {
                    self.bump();
                }
                Tok::Semi
            } else if self.path_mode && ch != '"' && ch != '\'' {
                self.lex_path()
            } else if ch == '"' || ch == '\'' {
                self.lex_string(ch, span)?
            } else if ch == '`' {
                self.lex_raw_xml(span)?
            } else if ch.is_ascii_digit() {
                self.lex_number(span)?
            } else if ch.is_alphabetic() || ch == '_' {
                self.lex_word()
            } else {
                self.lex_symbol(span)?
            };

            self.path_mode = matches!(tok, Tok::Use);
            tokens.push(Token { tok, span });
        }
    }

    fn lex_path(&mut self) -> Tok {
        let mut word = String::new();
        while matches!(self.peek(), Some(c) if !c.is_whitespace() && c != ';') {
            word.push(self.bump().unwrap());
        }
        if word.eq_ignore_ascii_case("input") {
            Tok::Input
        } else {
            Tok::Path(word)
        }
    }

    /// Strings accept both `"double"` and `'single'` quotes — single quotes
    /// survive shells that consume double quotes (PowerShell, cmd).
    fn lex_string(&mut self, quote: char, span: Span) -> Result<Tok> {
        self.bump(); // opening quote
        let mut value = String::new();
        loop {
            match self.bump() {
                Some(ch) if ch == quote => return Ok(Tok::Str(value)),
                Some('\\') => match self.bump() {
                    Some('n') => value.push('\n'),
                    Some('t') => value.push('\t'),
                    Some(other) => value.push(other),
                    None => break,
                },
                Some(ch) => value.push(ch),
                None => break,
            }
        }
        Err(XsqlError::spanned("unterminated string literal", span))
    }

    fn lex_raw_xml(&mut self, span: Span) -> Result<Tok> {
        self.bump(); // opening backtick
        let mut value = String::new();
        loop {
            match self.bump() {
                Some('`') => return Ok(Tok::RawXml(value)),
                Some(ch) => value.push(ch),
                None => break,
            }
        }
        Err(XsqlError::spanned("unterminated raw XML literal (missing closing `)", span))
    }

    fn lex_number(&mut self, span: Span) -> Result<Tok> {
        let mut text = String::new();
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            text.push(self.bump().unwrap());
        }
        if self.peek() == Some('.')
            && matches!(self.chars.get(self.pos + 1), Some(c) if c.is_ascii_digit())
        {
            text.push(self.bump().unwrap());
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                text.push(self.bump().unwrap());
            }
        }
        text.parse::<f64>()
            .map(Tok::Num)
            .map_err(|_| XsqlError::spanned(format!("invalid number `{text}`"), span))
    }

    fn lex_word(&mut self) -> Tok {
        let mut word = String::new();
        while matches!(self.peek(), Some(c) if c.is_alphanumeric() || c == '_' || c == '.') {
            word.push(self.bump().unwrap());
        }
        match word.to_ascii_uppercase().as_str() {
            "USE" => Tok::Use,
            "INPUT" => Tok::Input,
            "SELECT" => Tok::Select,
            "REPLACE" => Tok::Replace,
            "INSERT" => Tok::Insert,
            "DELETE" => Tok::Delete,
            "SET" => Tok::Set,
            "FOREACH" => Tok::Foreach,
            "IN" => Tok::In,
            "WHERE" => Tok::Where,
            "GROUP" => Tok::Group,
            "RAW" => Tok::Raw,
            "XML" => Tok::Xml,
            "IGNORE" => Tok::Ignore,
            "BREAK" => Tok::Break,
            "INTO" => Tok::Into,
            "AND" => Tok::And,
            "OR" => Tok::Or,
            "NOT" => Tok::Not,
            _ => Tok::Ident(word),
        }
    }

    fn lex_symbol(&mut self, span: Span) -> Result<Tok> {
        let ch = self.bump().unwrap();
        let tok = match ch {
            '=' => {
                if self.peek() == Some('=') {
                    self.bump();
                }
                Tok::Eq
            }
            '!' if self.peek() == Some('=') => {
                self.bump();
                Tok::NotEq
            }
            '<' => match self.peek() {
                Some('=') => {
                    self.bump();
                    Tok::Le
                }
                Some('>') => {
                    self.bump();
                    Tok::NotEq
                }
                _ => Tok::Lt,
            },
            '>' => {
                if self.peek() == Some('=') {
                    self.bump();
                    Tok::Ge
                } else {
                    Tok::Gt
                }
            }
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            other => {
                return Err(XsqlError::spanned(
                    format!("unexpected character `{other}`"),
                    span,
                ));
            }
        };
        Ok(tok)
    }
}
