//! AST for the xsql language.

use crate::error::Span;

#[derive(Debug, Clone)]
pub struct Script {
    pub blocks: Vec<Block>,
    /// Global `SET <name> = ON|OFF` statements (and `ANALYZE;`, which is
    /// sugar for `SET ANALYZE = ON`), in script order.
    pub settings: Vec<SettingStmt>,
}

#[derive(Debug, Clone)]
pub struct SettingStmt {
    pub setting: Setting,
    pub value: bool,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Pretty-printed (ON, default) vs compact single-line XML output.
    Format,
    /// Drop XML comments while parsing (ON, default) vs preserve them.
    IgnoreComments,
    /// Print per-stage timings to stderr after the run.
    Analyze,
}

/// Resolved global settings a script runs under.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub format: bool,
    pub ignore_comments: bool,
    pub analyze: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: true,
            ignore_comments: true,
            analyze: false,
        }
    }
}

impl Settings {
    pub fn apply(&mut self, stmt: &SettingStmt) {
        match stmt.setting {
            Setting::Format => self.format = stmt.value,
            Setting::IgnoreComments => self.ignore_comments = stmt.value,
            Setting::Analyze => self.analyze = stmt.value,
        }
    }

    pub fn resolve(stmts: &[SettingStmt]) -> Self {
        let mut settings = Self::default();
        for stmt in stmts {
            settings.apply(stmt);
        }
        settings
    }
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

/// What a verb operates on: a *group* (first element matched by tag, `name`
/// or `id` attribute — a container whose children are iterated), a *tag*
/// (every element with that tag name, wherever it sits — for documents
/// without a regular group structure), or the document *root* (every
/// element regardless of tag — for documents whose tag names aren't known
/// ahead of time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Group(String),
    Tag(String),
    Root,
}

impl Selector {
    pub fn name(&self) -> &str {
        match self {
            Selector::Group(name) | Selector::Tag(name) => name,
            Selector::Root => "root",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Verb {
    /// `SELECT GROUP name [FOREACH ...]` — prints the group, or the matching
    /// elements when a FOREACH filter is present. `SELECT TAG name` prints
    /// every element with that tag instead.
    Select {
        target: Selector,
        foreach: Option<Foreach>,
    },
    /// `REPLACE GROUP name RAW XML `...`` — replaces the group's children.
    ReplaceGroup { group: String, xml: String, xml_span: Span },
    /// `INSERT INTO GROUP name RAW XML `...`` — appends children.
    InsertInto { group: String, xml: String, xml_span: Span },
    /// `MERGE INTO GROUP name RAW XML `...`` — upsert: each fragment element
    /// is matched against the group's children (by `id`, then `name`, then
    /// tag); matched elements get the cited attributes written over them
    /// (other attributes preserved), unmatched elements are inserted.
    MergeInto { group: String, xml: String, xml_span: Span },
    /// `DELETE [IGNORE] GROUP name` — removes the whole group element.
    DeleteGroup { group: String, ignore: bool },
    /// `DELETE [IGNORE] TAG name` — removes every element with that tag.
    DeleteTag { tag: String, ignore: bool },
    /// Bare mutation loop. Also the desugaring of the MySQL-style shorthands
    /// (`UPDATE`, `MERGE ... SET`, `DELETE FROM`).
    Foreach(Foreach),
}

/// What a `FOREACH` iterates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopSource {
    /// Group name at top level; in a nested loop, an enclosing loop variable
    /// (iterates that element's children) or a group inside the current
    /// element.
    Name(String),
    /// `IN TAG t` — every element with tag `t`: document-wide at top level,
    /// within the current element's subtree when nested.
    Tag(String),
    /// `IN ROOT` — every element regardless of tag: document-wide at top
    /// level, within the current element's subtree when nested. For
    /// documents/subtrees whose tag names aren't known ahead of time.
    Root,
}

impl LoopSource {
    pub fn name(&self) -> &str {
        match self {
            LoopSource::Name(name) | LoopSource::Tag(name) => name,
            LoopSource::Root => "root",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Foreach {
    pub var: String,
    pub source: LoopSource,
    pub ops: Vec<Op>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Op {
    /// Guard: when false, remaining ops are skipped for this element.
    Where(Expr),
    /// `WHERE REQUIRED attr <cond>` — every element must carry `attr`: a
    /// missing attribute is a hard error (plain WHERE silently skips such
    /// elements, since missing attributes evaluate as null); present
    /// attributes evaluate `expr` as a normal guard.
    WhereRequired {
        var: String,
        attr: String,
        expr: Expr,
        span: Span,
    },
    Set {
        var: String,
        attr: String,
        value: Expr,
        span: Span,
    },
    /// `MERGE var.attr = expr` — writes the attribute only when it is
    /// missing; an existing value wins, even when different (idempotent).
    Merge {
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
    /// Nested loop: `IN` names an enclosing loop variable (iterates that
    /// element's children) or a group found inside the current element.
    Foreach(Box<Foreach>),
    /// `OUTPUT *` or `OUTPUT expr [AS name], ...` — emission point: when
    /// execution reaches it, prints the current element in full (`*`, which
    /// is also what a SELECT without OUTPUT does) or a flat element carrying
    /// only the cited attributes/expressions. Each item's name defaults to
    /// the attribute name; non-attribute expressions require `AS`.
    Output {
        all: bool,
        items: Vec<(Expr, String)>,
        span: Span,
    },
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
    /// `FUNC(expr)` — an aggregate function call (`COUNT`, `MIN`, `MAX`,
    /// `SUM`, `AVG`). Only valid as an `OUTPUT` item; evaluated across every
    /// element the loop reaches, not per element.
    Call {
        func: String,
        arg: Box<Expr>,
        span: Span,
    },
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
