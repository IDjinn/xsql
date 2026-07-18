//! Recursive-descent parser: tokens -> `ast::Script`.

use std::time::{Duration, Instant};

use crate::ast::*;
use crate::error::{Result, Span, XsqlError};
use crate::lexer::{Tok, Token, lex};

pub fn parse(source: &str) -> Result<Script> {
    parse_session(source, None).map(|(script, _)| script)
}

pub struct ParseTimes {
    pub lex: Duration,
    pub parse: Duration,
}

/// Like [`parse`], but reports how long lexing and parsing each took
/// (feeds the `ANALYZE` report).
pub fn parse_with_times(source: &str) -> Result<(Script, ParseTimes)> {
    let lex_start = Instant::now();
    let tokens = lex(source)?;
    let lex_time = lex_start.elapsed();
    let parse_start = Instant::now();
    let (script, _) = Parser { tokens, pos: 0 }.script(None)?;
    let times = ParseTimes { lex: lex_time, parse: parse_start.elapsed() };
    Ok((script, times))
}

/// Parses with an inherited sticky `USE` source (REPL mode: the current
/// document carries over between submitted statements). Returns the script
/// and the sticky source left active at the end.
pub fn parse_session(
    source: &str,
    current: Option<Source>,
) -> Result<(Script, Option<Source>)> {
    let tokens = lex(source)?;
    Parser { tokens, pos: 0 }.script(current)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn bump(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == tok {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: Tok, context: &str) -> Result<Token> {
        if self.peek() == &tok {
            Ok(self.bump())
        } else {
            Err(XsqlError::spanned(
                format!(
                    "expected {} {context}, found {}",
                    tok.describe(),
                    self.peek().describe()
                ),
                self.span(),
            ))
        }
    }

    fn ident(&mut self, context: &str) -> Result<(String, Span)> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Ident(name) => {
                self.bump();
                Ok((name, span))
            }
            other => Err(XsqlError::spanned(
                format!("expected identifier {context}, found {}", other.describe()),
                span,
            )),
        }
    }

    fn script(mut self, mut current: Option<Source>) -> Result<(Script, Option<Source>)> {
        let mut blocks = Vec::new();
        let mut settings = Vec::new();
        // `USE` is sticky: once declared it applies to every following block
        // until another `USE` appears.
        loop {
            while self.eat(&Tok::Semi) {}
            if self.peek() == &Tok::Eof {
                return Ok((Script { blocks, settings }, current));
            }
            // Global statements: no document required.
            if self.peek() == &Tok::Analyze {
                let span = self.span();
                self.bump();
                settings.push(SettingStmt { setting: Setting::Analyze, value: true, span });
                self.statement_end()?;
                continue;
            }
            if self.peek() == &Tok::Set {
                settings.push(self.global_set()?);
                self.statement_end()?;
                continue;
            }
            let span = self.span();
            if self.eat(&Tok::Use) {
                current = Some(self.use_source()?);
                // `USE x;` alone just switches the current document.
                if matches!(self.peek(), Tok::Semi | Tok::Eof) {
                    continue;
                }
            }
            let Some(source) = current.clone() else {
                return Err(XsqlError::spanned(
                    "no document in scope: start the script with `USE <file>` or `USE INPUT`",
                    span,
                ));
            };
            let verb_span = self.span();
            let verb = self.verb()?;
            blocks.push(Block { source, verb, span: verb_span });
            self.statement_end()?;
        }
    }

    fn statement_end(&mut self) -> Result<()> {
        if self.peek() != &Tok::Eof {
            self.expect(Tok::Semi, "to terminate the statement block")?;
        }
        Ok(())
    }

    /// `SET <name> = ON|OFF` at script scope (distinct from the FOREACH-level
    /// `SET var.attr = expr`, which is handled inside `foreach`).
    fn global_set(&mut self) -> Result<SettingStmt> {
        let span = self.span();
        self.bump(); // SET
        let name_span = self.span();
        let name = match self.peek().clone() {
            Tok::Ident(name) => {
                self.bump();
                name
            }
            // `ANALYZE` lexes as a keyword but is also a setting name.
            Tok::Analyze => {
                self.bump();
                "ANALYZE".to_string()
            }
            other => {
                return Err(XsqlError::spanned(
                    format!("expected setting name after SET, found {}", other.describe()),
                    name_span,
                ));
            }
        };
        let setting = match name.to_ascii_uppercase().as_str() {
            "FORMAT" => Setting::Format,
            "IGNORE_COMMENTS" => Setting::IgnoreComments,
            "ANALYZE" => Setting::Analyze,
            _ => {
                return Err(XsqlError::spanned(
                    format!("unknown setting `{name}` (known settings: FORMAT, IGNORE_COMMENTS, ANALYZE)"),
                    name_span,
                ));
            }
        };
        self.expect(Tok::Eq, "after the setting name")?;
        let value_span = self.span();
        let value = match self.peek().clone() {
            Tok::Ident(word) => match word.to_ascii_uppercase().as_str() {
                "ON" | "TRUE" => {
                    self.bump();
                    true
                }
                "OFF" | "FALSE" => {
                    self.bump();
                    false
                }
                _ => {
                    return Err(XsqlError::spanned(
                        format!("expected ON or OFF as the setting value, found `{word}`"),
                        value_span,
                    ));
                }
            },
            Tok::Num(n) if n == 1.0 => {
                self.bump();
                true
            }
            Tok::Num(n) if n == 0.0 => {
                self.bump();
                false
            }
            other => {
                return Err(XsqlError::spanned(
                    format!("expected ON or OFF as the setting value, found {}", other.describe()),
                    value_span,
                ));
            }
        };
        Ok(SettingStmt { setting, value, span })
    }

    fn use_source(&mut self) -> Result<Source> {
        match self.peek().clone() {
            Tok::Path(path) => {
                self.bump();
                Ok(Source::File(path))
            }
            Tok::Str(path) => {
                self.bump();
                Ok(Source::File(path))
            }
            Tok::Input => {
                self.bump();
                Ok(Source::Input)
            }
            other => Err(XsqlError::spanned(
                format!("expected file path or INPUT after USE, found {}", other.describe()),
                self.span(),
            )),
        }
    }

    fn verb(&mut self) -> Result<Verb> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Select => {
                self.bump();
                self.expect(Tok::Group, "after SELECT")?;
                let group = self.group_name()?;
                let foreach = if self.peek() == &Tok::Foreach {
                    Some(self.foreach()?)
                } else if self.peek() == &Tok::Output {
                    // `SELECT GROUP g OUTPUT ...` — implicit loop over the
                    // group's children.
                    let output_span = self.span();
                    let op = self.output_op()?;
                    Some(Foreach {
                        var: group.clone(),
                        group: group.clone(),
                        ops: vec![op],
                        span: output_span,
                    })
                } else {
                    None
                };
                Ok(Verb::Select { group, foreach })
            }
            Tok::Replace => {
                self.bump();
                self.expect(Tok::Group, "after REPLACE")?;
                let group = self.group_name()?;
                let (xml, xml_span) = self.raw_xml()?;
                Ok(Verb::ReplaceGroup { group, xml, xml_span })
            }
            Tok::Insert => {
                self.bump();
                self.expect(Tok::Into, "after INSERT")?;
                self.expect(Tok::Group, "after INSERT INTO")?;
                let group = self.group_name()?;
                let (xml, xml_span) = self.raw_xml()?;
                Ok(Verb::InsertInto { group, xml, xml_span })
            }
            Tok::Merge => {
                self.bump();
                self.expect(Tok::Into, "after MERGE")?;
                self.expect(Tok::Group, "after MERGE INTO")?;
                let group = self.group_name()?;
                let (xml, xml_span) = self.raw_xml()?;
                Ok(Verb::MergeInto { group, xml, xml_span })
            }
            Tok::Delete => {
                self.bump();
                let ignore = self.eat(&Tok::Ignore);
                self.expect(Tok::Group, "after DELETE")?;
                let group = self.group_name()?;
                Ok(Verb::DeleteGroup { group, ignore })
            }
            Tok::Foreach => Ok(Verb::Foreach(self.foreach()?)),
            other => Err(XsqlError::spanned(
                format!(
                    "expected SELECT, REPLACE, INSERT, MERGE, DELETE or FOREACH after USE, found {}",
                    other.describe()
                ),
                span,
            )),
        }
    }

    /// Group names may be identifiers (`arms`), numbers (`110000` — real-world
    /// files key groups by numeric `id`), or quoted strings (`"Group"` — for
    /// names that collide with keywords or contain special characters).
    fn group_name(&mut self) -> Result<String> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Ident(name) => {
                self.bump();
                Ok(name)
            }
            Tok::Num(n) if n.fract() == 0.0 && n >= 0.0 => {
                self.bump();
                Ok(format!("{}", n as u64))
            }
            Tok::Str(name) => {
                self.bump();
                Ok(name)
            }
            other => Err(XsqlError::spanned(
                format!("expected group name (identifier, number or quoted string), found {}", other.describe()),
                span,
            )),
        }
    }

    fn raw_xml(&mut self) -> Result<(String, Span)> {
        self.expect(Tok::Raw, "(expected RAW XML `...`)")?;
        self.expect(Tok::Xml, "after RAW")?;
        let span = self.span();
        match self.peek().clone() {
            Tok::RawXml(xml) => {
                self.bump();
                Ok((xml, span))
            }
            other => Err(XsqlError::spanned(
                format!("expected raw XML literal in backticks, found {}", other.describe()),
                span,
            )),
        }
    }

    fn foreach(&mut self) -> Result<Foreach> {
        let span = self.span();
        self.expect(Tok::Foreach, "to start a loop")?;
        let (var, var_span) = self.ident("as loop variable")?;
        if var.contains('.') {
            return Err(XsqlError::spanned(
                format!("loop variable `{var}` must not contain dots"),
                var_span,
            ));
        }
        self.expect(Tok::In, "after the loop variable")?;
        let group = self.group_name()?;

        let mut ops = Vec::new();
        loop {
            match self.peek().clone() {
                Tok::Where => {
                    self.bump();
                    if self.eat(&Tok::Required) {
                        let span = self.span();
                        // Peek (don't consume) the attribute reference: the
                        // same token then re-parses as the condition's lhs,
                        // so `WHERE REQUIRED cost > 100` needs `cost` once.
                        let (var, attr) = match self.peek().clone() {
                            Tok::Ident(name) => match name.split_once('.') {
                                Some((v, a)) => (v.to_string(), a.to_string()),
                                None => (String::new(), name),
                            },
                            other => {
                                return Err(XsqlError::spanned(
                                    format!(
                                        "expected attribute reference after WHERE REQUIRED, found {}",
                                        other.describe()
                                    ),
                                    span,
                                ));
                            }
                        };
                        let expr = self.expr()?;
                        ops.push(Op::WhereRequired { var, attr, expr, span });
                    } else {
                        ops.push(Op::Where(self.expr()?));
                    }
                }
                Tok::Set => {
                    let span = self.span();
                    self.bump();
                    let (target, target_span) = self.ident("after SET")?;
                    let (var, attr) = split_attr_ref(&target, target_span)?;
                    self.expect(Tok::Eq, "after the SET target")?;
                    let value = self.expr()?;
                    ops.push(Op::Set { var, attr, value, span });
                }
                Tok::Merge => {
                    let span = self.span();
                    self.bump();
                    let (target, target_span) = self.ident("after MERGE")?;
                    let (var, attr) = split_attr_ref(&target, target_span)?;
                    self.expect(Tok::Eq, "after the MERGE target")?;
                    let value = self.expr()?;
                    ops.push(Op::Merge { var, attr, value, span });
                }
                Tok::Foreach => {
                    ops.push(Op::Foreach(Box::new(self.foreach()?)));
                }
                Tok::Output => {
                    ops.push(self.output_op()?);
                }
                Tok::Delete => {
                    let span = self.span();
                    self.bump();
                    let ignore = self.eat(&Tok::Ignore);
                    let (target, _) = self.ident("after DELETE")?;
                    match target.split_once('.') {
                        Some((var, attr)) => ops.push(Op::DeleteAttr {
                            var: var.to_string(),
                            attr: attr.to_string(),
                            ignore,
                            span,
                        }),
                        None => ops.push(Op::DeleteElem { var: target, ignore, span }),
                    }
                }
                Tok::Break => {
                    self.bump();
                    ops.push(Op::Break);
                }
                _ => break,
            }
        }

        Ok(Foreach { var, group, ops, span })
    }

    /// `OUTPUT *` | `OUTPUT expr [AS name] (, expr [AS name])*` — the caller
    /// has peeked the OUTPUT token.
    fn output_op(&mut self) -> Result<Op> {
        let span = self.span();
        self.expect(Tok::Output, "to start the projection")?;
        if self.eat(&Tok::Star) {
            return Ok(Op::Output { all: true, items: Vec::new(), span });
        }
        let mut items = Vec::new();
        loop {
            let item_span = self.span();
            let expr = self.expr()?;
            let alias = if self.eat(&Tok::As) {
                Some(self.ident("after AS")?.0)
            } else {
                None
            };
            let name = match (alias, &expr) {
                (Some(name), _) => name,
                (None, Expr::Attr { attr, .. }) => attr.clone(),
                (None, _) => {
                    return Err(XsqlError::spanned(
                        "an OUTPUT expression needs a name: add `AS <name>`",
                        item_span,
                    ));
                }
            };
            items.push((expr, name));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(Op::Output { all: false, items, span })
    }

    fn expr(&mut self) -> Result<Expr> {
        self.expr_or()
    }

    fn expr_or(&mut self) -> Result<Expr> {
        let mut lhs = self.expr_and()?;
        while self.peek() == &Tok::Or {
            let span = self.span();
            self.bump();
            let rhs = self.expr_and()?;
            lhs = binary(BinOp::Or, lhs, rhs, span);
        }
        Ok(lhs)
    }

    fn expr_and(&mut self) -> Result<Expr> {
        let mut lhs = self.expr_not()?;
        while self.peek() == &Tok::And {
            let span = self.span();
            self.bump();
            let rhs = self.expr_not()?;
            lhs = binary(BinOp::And, lhs, rhs, span);
        }
        Ok(lhs)
    }

    fn expr_not(&mut self) -> Result<Expr> {
        if self.peek() == &Tok::Not {
            let span = self.span();
            self.bump();
            let inner = self.expr_not()?;
            return Ok(Expr::Not(Box::new(inner), span));
        }
        self.expr_cmp()
    }

    fn expr_cmp(&mut self) -> Result<Expr> {
        let lhs = self.expr_add()?;
        let op = match self.peek() {
            Tok::Eq => BinOp::Eq,
            Tok::NotEq => BinOp::NotEq,
            Tok::Lt => BinOp::Lt,
            Tok::Gt => BinOp::Gt,
            Tok::Le => BinOp::Le,
            Tok::Ge => BinOp::Ge,
            _ => return Ok(lhs),
        };
        let span = self.span();
        self.bump();
        let rhs = self.expr_add()?;
        Ok(binary(op, lhs, rhs, span))
    }

    fn expr_add(&mut self) -> Result<Expr> {
        let mut lhs = self.expr_mul()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => return Ok(lhs),
            };
            let span = self.span();
            self.bump();
            let rhs = self.expr_mul()?;
            lhs = binary(op, lhs, rhs, span);
        }
    }

    fn expr_mul(&mut self) -> Result<Expr> {
        let mut lhs = self.expr_unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                _ => return Ok(lhs),
            };
            let span = self.span();
            self.bump();
            let rhs = self.expr_unary()?;
            lhs = binary(op, lhs, rhs, span);
        }
    }

    fn expr_unary(&mut self) -> Result<Expr> {
        if self.peek() == &Tok::Minus {
            let span = self.span();
            self.bump();
            let inner = self.expr_unary()?;
            return Ok(Expr::Neg(Box::new(inner), span));
        }
        self.expr_primary()
    }

    fn expr_primary(&mut self) -> Result<Expr> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Num(n) => {
                self.bump();
                Ok(Expr::Num(n))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Expr::Str(s))
            }
            Tok::Ident(name) => {
                self.bump();
                match name.split_once('.') {
                    Some((var, attr)) => Ok(Expr::Attr {
                        var: var.to_string(),
                        attr: attr.to_string(),
                        span,
                    }),
                    // Bare name: attribute of the loop element.
                    None => Ok(Expr::Attr {
                        var: String::new(),
                        attr: name,
                        span,
                    }),
                }
            }
            Tok::LParen => {
                self.bump();
                let inner = self.expr()?;
                self.expect(Tok::RParen, "to close the parenthesized expression")?;
                Ok(inner)
            }
            other => Err(XsqlError::spanned(
                format!("expected an expression, found {}", other.describe()),
                span,
            )),
        }
    }
}

fn binary(op: BinOp, lhs: Expr, rhs: Expr, span: Span) -> Expr {
    Expr::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
        span,
    }
}

fn split_attr_ref(target: &str, span: Span) -> Result<(String, String)> {
    match target.split_once('.') {
        Some((var, attr)) if !attr.is_empty() => Ok((var.to_string(), attr.to_string())),
        _ => Err(XsqlError::spanned(
            format!("expected `variable.attribute`, found `{target}`"),
            span,
        )),
    }
}
