//! AST for the xsql language.

use crate::error::Span;

#[derive(Debug, Clone)]
pub struct Script {
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub source: Source,
    pub verb: Verb,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    File(String),
    /// `USE INPUT` — the XML document arrives on stdin.
    Input,
}

impl Source {
    pub fn describe(&self) -> &str {
        match self {
            Source::File(path) => path,
            Source::Input => "<stdin>",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Verb {
    /// `SELECT GROUP name [FOREACH ...]` — prints the group, or the matching
    /// elements when a FOREACH filter is present.
    Select {
        group: String,
        foreach: Option<Foreach>,
    },
    /// `REPLACE GROUP name RAW XML `...`` — replaces the group's children.
    ReplaceGroup { group: String, xml: String, xml_span: Span },
    /// `INSERT INTO GROUP name RAW XML `...`` — appends children.
    InsertInto { group: String, xml: String, xml_span: Span },
    /// `DELETE [IGNORE] GROUP name` — removes the whole group element.
    DeleteGroup { group: String, ignore: bool },
    /// Bare mutation loop.
    Foreach(Foreach),
}

#[derive(Debug, Clone)]
pub struct Foreach {
    pub var: String,
    pub group: String,
    pub ops: Vec<Op>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Op {
    /// Guard: when false, remaining ops are skipped for this element.
    Where(Expr),
    Set {
        var: String,
        attr: String,
        value: Expr,
        span: Span,
    },
    DeleteAttr {
        var: String,
        attr: String,
        ignore: bool,
        span: Span,
    },
    DeleteElem {
        var: String,
        ignore: bool,
        span: Span,
    },
    Break,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Str(String),
    Num(f64),
    /// `var.attr` reference; `var` may be empty for a bare attribute name,
    /// and may name either the loop variable or the group.
    Attr {
        var: String,
        attr: String,
        span: Span,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    Not(Box<Expr>, Span),
    Neg(Box<Expr>, Span),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Eq,
    NotEq,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Add,
    Sub,
    Mul,
    Div,
}
