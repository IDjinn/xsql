//! Interpreter.
//!
//! Execution model:
//! 1. All distinct `USE` sources are loaded and parsed **in parallel**.
//! 2. Blocks are grouped by target document; groups for different documents
//!    run **in parallel** (blocks within one document stay sequential).
//!    SELECT output is buffered per block and flushed in script order.
//! 3. Large FOREACH loops use evaluate-then-apply: expressions are evaluated
//!    read-only **in parallel** over the children (each producing a mutation
//!    plan), then plans are applied sequentially — preserving exact
//!    sequential semantics. Loops containing BREAK stay fully sequential.
//! 4. Modified documents are serialized in parallel and appended to the
//!    output in first-use order.

pub mod value;

use std::collections::HashMap;

use rayon::prelude::*;

use crate::ast::{BinOp, Block, Expr, Foreach, Op, Script, Source, Verb};
use crate::error::{Result, XsqlError};
use crate::xml::dom::{Document, NodeId};
use crate::xml::parse::{parse_document, parse_fragment};
use crate::xml::serialize::{serialize_document, serialize_subtree};

use value::Value;

/// Children count from which FOREACH switches to parallel evaluation.
const PAR_FOREACH_THRESHOLD: usize = 1024;

pub fn run(script: &Script, stdin_xml: Option<String>) -> Result<String> {
    // Distinct sources in first-use order.
    let mut sources: Vec<Source> = Vec::new();
    for block in &script.blocks {
        if !sources.contains(&block.source) {
            sources.push(block.source.clone());
        }
    }

    if sources.contains(&Source::Input) && stdin_xml.is_none() {
        return Err(XsqlError::plain(
            "script uses `USE INPUT` but no XML document was piped on stdin \
             (note: the script itself cannot also come from stdin)",
        ));
    }

    // 1. Parallel load.
    let mut docs: Vec<Document> = sources
        .par_iter()
        .map(|source| load_source(source, stdin_xml.as_deref()))
        .collect::<Result<Vec<_>>>()?;

    // 2. Group blocks by document, run groups in parallel.
    let source_index: HashMap<&Source, usize> =
        sources.iter().enumerate().map(|(i, s)| (s, i)).collect();
    let mut per_doc: Vec<Vec<(usize, &Block)>> = vec![Vec::new(); sources.len()];
    for (block_idx, block) in script.blocks.iter().enumerate() {
        per_doc[source_index[&block.source]].push((block_idx, block));
    }

    struct DocRun {
        outputs: Vec<(usize, String)>,
        modified: bool,
    }

    let runs: Vec<DocRun> = docs
        .par_iter_mut()
        .zip(&per_doc)
        .map(|(doc, blocks)| {
            let mut outputs = Vec::new();
            let mut modified = false;
            for &(block_idx, block) in blocks {
                let result = run_block(doc, block)?;
                modified |= result.modified;
                if let Some(text) = result.output {
                    outputs.push((block_idx, text));
                }
            }
            Ok(DocRun { outputs, modified })
        })
        .collect::<Result<Vec<_>>>()?;

    // 3. Flush SELECT outputs in script order.
    let mut selects: Vec<(usize, String)> =
        runs.iter().flat_map(|r| r.outputs.iter().cloned()).collect();
    selects.sort_by_key(|(idx, _)| *idx);

    // 4. Serialize modified documents in parallel, first-use order.
    let serialized: Vec<String> = docs
        .par_iter()
        .zip(&runs)
        .filter(|(_, run)| run.modified)
        .map(|(doc, _)| serialize_document(doc))
        .collect();

    let mut out = String::new();
    for (_, text) in selects {
        out.push_str(&text);
    }
    for text in serialized {
        out.push_str(&text);
    }
    Ok(out)
}

/// Incremental interpreter for the REPL: documents load lazily on first
/// `USE` and stay in memory (with their pending mutations) across submitted
/// statements. SELECT output is returned per `exec`; modified documents are
/// only serialized on demand via [`Session::dump_modified`].
pub struct Session {
    stdin_xml: Option<String>,
    docs: Vec<(Source, Document, bool)>,
}

impl Session {
    pub fn new(stdin_xml: Option<String>) -> Self {
        Self { stdin_xml, docs: Vec::new() }
    }

    pub fn exec(&mut self, script: &Script) -> Result<String> {
        let mut out = String::new();
        for block in &script.blocks {
            let idx = self.doc_index(&block.source)?;
            let (_, doc, modified) = &mut self.docs[idx];
            let result = run_block(doc, block)?;
            *modified |= result.modified;
            if let Some(text) = result.output {
                out.push_str(&text);
            }
        }
        Ok(out)
    }

    /// Serializes every modified document, first-use order.
    pub fn dump_modified(&self) -> String {
        self.docs
            .iter()
            .filter(|(_, _, modified)| *modified)
            .map(|(_, doc, _)| serialize_document(doc))
            .collect()
    }

    pub fn has_modifications(&self) -> bool {
        self.docs.iter().any(|(_, _, modified)| *modified)
    }

    fn doc_index(&mut self, source: &Source) -> Result<usize> {
        if let Some(idx) = self.docs.iter().position(|(s, _, _)| s == source) {
            return Ok(idx);
        }
        if *source == Source::Input && self.stdin_xml.is_none() {
            return Err(XsqlError::plain(
                "`USE INPUT` needs an XML document piped on stdin \
                 (not available in interactive mode)",
            ));
        }
        let doc = load_source(source, self.stdin_xml.as_deref())?;
        self.docs.push((source.clone(), doc, false));
        Ok(self.docs.len() - 1)
    }
}

fn load_source(source: &Source, stdin_xml: Option<&str>) -> Result<Document> {
    let (name, content) = match source {
        Source::File(path) => (
            path.as_str(),
            std::fs::read_to_string(path)
                .map_err(|e| XsqlError::plain(format!("cannot read `{path}`: {e}")))?,
        ),
        Source::Input => ("<stdin>", stdin_xml.expect("checked by caller").to_string()),
    };
    parse_document(&content).map_err(|e| XsqlError::plain(format!("{name}: {e}")))
}

struct BlockResult {
    output: Option<String>,
    modified: bool,
}

fn run_block(doc: &mut Document, block: &Block) -> Result<BlockResult> {
    match &block.verb {
        Verb::Select { group, foreach } => {
            let group_id = resolve_group(doc, group, block)?;
            match foreach {
                None => Ok(BlockResult {
                    output: Some(serialize_subtree(doc, group_id)),
                    modified: false,
                }),
                Some(foreach) => {
                    let outcome = run_foreach(doc, group_id, foreach)?;
                    let mut text = String::new();
                    for id in outcome.selected {
                        text.push_str(&serialize_subtree(doc, id));
                    }
                    Ok(BlockResult {
                        output: Some(text),
                        modified: outcome.mutations > 0,
                    })
                }
            }
        }
        Verb::Foreach(foreach) => {
            let group_id = resolve_group(doc, &foreach.group, block)?;
            let outcome = run_foreach(doc, group_id, foreach)?;
            Ok(BlockResult {
                output: None,
                modified: outcome.mutations > 0,
            })
        }
        Verb::ReplaceGroup { group, xml, xml_span } => {
            let group_id = resolve_group(doc, group, block)?;
            let fragment = parse_fragment(xml)
                .map_err(|e| XsqlError::spanned(format!("bad RAW XML: {e}"), *xml_span))?;
            for child in std::mem::take(&mut doc.node_mut(group_id).children) {
                doc.node_mut(child).parent = None;
            }
            doc.graft(fragment, group_id);
            Ok(BlockResult { output: None, modified: true })
        }
        Verb::InsertInto { group, xml, xml_span } => {
            let group_id = resolve_group(doc, group, block)?;
            let fragment = parse_fragment(xml)
                .map_err(|e| XsqlError::spanned(format!("bad RAW XML: {e}"), *xml_span))?;
            doc.graft(fragment, group_id);
            Ok(BlockResult { output: None, modified: true })
        }
        Verb::DeleteGroup { group, ignore } => match doc.find_group(group) {
            Some(group_id) => {
                doc.detach(group_id);
                Ok(BlockResult { output: None, modified: true })
            }
            None if *ignore => Ok(BlockResult { output: None, modified: false }),
            None => Err(group_not_found(group, block)),
        },
    }
}

fn resolve_group(doc: &Document, group: &str, block: &Block) -> Result<NodeId> {
    doc.find_group(group).ok_or_else(|| group_not_found(group, block))
}

fn group_not_found(group: &str, block: &Block) -> XsqlError {
    XsqlError::spanned(
        format!(
            "group `{group}` not found in {} (matched by tag, `name` or `id` attribute)",
            block.source.describe()
        ),
        block.span,
    )
}

// ---------------------------------------------------------------------------
// FOREACH engine (evaluate-then-apply)
// ---------------------------------------------------------------------------

/// Planned effects for one child element, computed read-only.
#[derive(Debug, Default)]
struct ChildPlan {
    /// Passed every WHERE guard it reached (still true for deleted elements,
    /// but deleted elements are never selected for output).
    selected: bool,
    broke: bool,
    sets: Vec<(String, String)>,
    del_attrs: Vec<String>,
    delete_elem: bool,
}

struct ForeachOutcome {
    selected: Vec<NodeId>,
    mutations: usize,
}

fn run_foreach(doc: &mut Document, group_id: NodeId, foreach: &Foreach) -> Result<ForeachOutcome> {
    let children: Vec<NodeId> = doc.node(group_id).children.clone();
    let has_break = foreach.ops.iter().any(|op| matches!(op, Op::Break));

    let mut outcome = ForeachOutcome { selected: Vec::new(), mutations: 0 };

    if !has_break && children.len() >= PAR_FOREACH_THRESHOLD {
        // Parallel read-only evaluation, then sequential apply.
        let doc_ref: &Document = doc;
        let plans: Vec<ChildPlan> = children
            .par_iter()
            .map(|&child| plan_child(doc_ref, child, foreach))
            .collect::<Result<Vec<_>>>()?;
        for (&child, plan) in children.iter().zip(plans) {
            apply_plan(doc, child, plan, &mut outcome);
        }
    } else {
        for &child in &children {
            let plan = plan_child(doc, child, foreach)?;
            let broke = plan.broke;
            apply_plan(doc, child, plan, &mut outcome);
            if broke {
                break;
            }
        }
    }

    Ok(outcome)
}

fn apply_plan(doc: &mut Document, child: NodeId, plan: ChildPlan, outcome: &mut ForeachOutcome) {
    outcome.mutations += plan.sets.len() + plan.del_attrs.len() + plan.delete_elem as usize;
    let el = doc.node_mut(child);
    for (attr, val) in plan.sets {
        el.set_attr(&attr, val);
    }
    for attr in plan.del_attrs {
        el.remove_attr(&attr);
    }
    if plan.delete_elem {
        doc.detach(child);
    } else if plan.selected {
        outcome.selected.push(child);
    }
}

/// Attribute view of one child during planning: pending SET/DELETE ops are
/// layered over the element so later expressions observe earlier writes.
struct Overlay<'a> {
    doc: &'a Document,
    child: NodeId,
    changes: Vec<(String, Option<String>)>,
}

impl<'a> Overlay<'a> {
    fn get(&self, attr: &str) -> Option<String> {
        for (name, value) in self.changes.iter().rev() {
            if name == attr {
                return value.clone();
            }
        }
        self.doc.node(self.child).attr(attr).map(str::to_string)
    }
}

fn plan_child(doc: &Document, child: NodeId, foreach: &Foreach) -> Result<ChildPlan> {
    let mut plan = ChildPlan { selected: true, ..ChildPlan::default() };
    let mut overlay = Overlay { doc, child, changes: Vec::new() };

    for op in &foreach.ops {
        match op {
            Op::Where(expr) => {
                if !eval_expr(expr, foreach, &overlay)?.truthy() {
                    plan.selected = false;
                    break;
                }
            }
            Op::Set { var, attr, value, span } => {
                check_var(var, foreach, *span)?;
                let value = eval_expr(value, foreach, &overlay)?.to_display();
                overlay.changes.push((attr.clone(), Some(value.clone())));
                plan.sets.push((attr.clone(), value));
            }
            Op::DeleteAttr { var, attr, ignore, span } => {
                check_var(var, foreach, *span)?;
                if overlay.get(attr).is_some() {
                    overlay.changes.push((attr.clone(), None));
                    plan.del_attrs.push(attr.clone());
                } else if !ignore {
                    return Err(XsqlError::spanned(
                        format!(
                            "attribute `{attr}` not found on <{}> (use DELETE IGNORE to skip silently)",
                            doc.node(child).tag
                        ),
                        *span,
                    ));
                }
            }
            Op::DeleteElem { var, ignore: _, span } => {
                check_var(var, foreach, *span)?;
                plan.delete_elem = true;
                break;
            }
            Op::Break => {
                plan.broke = true;
                break;
            }
        }
    }

    Ok(plan)
}

/// The loop variable, the group name and a bare (empty) prefix all refer to
/// the current element — the scratch-file scripts use them interchangeably.
fn check_var(var: &str, foreach: &Foreach, span: crate::error::Span) -> Result<()> {
    if var.is_empty() || var == foreach.var || var == foreach.group {
        Ok(())
    } else {
        Err(XsqlError::spanned(
            format!(
                "unknown variable `{var}` (loop variable is `{}`, group is `{}`)",
                foreach.var, foreach.group
            ),
            span,
        ))
    }
}

fn eval_expr(expr: &Expr, foreach: &Foreach, overlay: &Overlay) -> Result<Value> {
    match expr {
        Expr::Str(s) => Ok(Value::Str(s.clone())),
        Expr::Num(n) => Ok(Value::Num(*n)),
        Expr::Attr { var, attr, span } => {
            check_var(var, foreach, *span)?;
            Ok(overlay.get(attr).map_or(Value::Null, Value::Str))
        }
        Expr::Not(inner, _) => Ok(Value::Bool(!eval_expr(inner, foreach, overlay)?.truthy())),
        Expr::Neg(inner, span) => {
            let value = eval_expr(inner, foreach, overlay)?;
            match value.as_num() {
                Some(n) => Ok(Value::Num(-n)),
                None => Err(XsqlError::spanned(
                    format!("cannot negate non-numeric value `{}`", value.to_display()),
                    *span,
                )),
            }
        }
        Expr::Binary { op, lhs, rhs, span } => {
            match op {
                BinOp::And => {
                    let l = eval_expr(lhs, foreach, overlay)?;
                    return Ok(Value::Bool(
                        l.truthy() && eval_expr(rhs, foreach, overlay)?.truthy(),
                    ));
                }
                BinOp::Or => {
                    let l = eval_expr(lhs, foreach, overlay)?;
                    return Ok(Value::Bool(
                        l.truthy() || eval_expr(rhs, foreach, overlay)?.truthy(),
                    ));
                }
                _ => {}
            }

            let l = eval_expr(lhs, foreach, overlay)?;
            let r = eval_expr(rhs, foreach, overlay)?;
            let value = match op {
                BinOp::Eq => Value::Bool(l.loose_eq(&r)),
                BinOp::NotEq => Value::Bool(!l.loose_eq(&r)),
                BinOp::Lt => Value::Bool(l.compare(&r) == Some(std::cmp::Ordering::Less)),
                BinOp::Gt => Value::Bool(l.compare(&r) == Some(std::cmp::Ordering::Greater)),
                BinOp::Le => Value::Bool(matches!(
                    l.compare(&r),
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                )),
                BinOp::Ge => Value::Bool(matches!(
                    l.compare(&r),
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                )),
                BinOp::Add => match (l.as_num(), r.as_num()) {
                    (Some(a), Some(b)) => Value::Num(a + b),
                    // Non-numeric `+` is string concatenation.
                    _ => {
                        require_present(&l, "left", span)?;
                        require_present(&r, "right", span)?;
                        Value::Str(l.to_display() + &r.to_display())
                    }
                },
                BinOp::Sub | BinOp::Mul | BinOp::Div => {
                    let (a, b) = numeric_operands(&l, &r, span)?;
                    match op {
                        BinOp::Sub => Value::Num(a - b),
                        BinOp::Mul => Value::Num(a * b),
                        _ => {
                            if b == 0.0 {
                                return Err(XsqlError::spanned("division by zero", *span));
                            }
                            Value::Num(a / b)
                        }
                    }
                }
                BinOp::And | BinOp::Or => unreachable!("handled above"),
            };
            Ok(value)
        }
    }
}

fn require_present(value: &Value, side: &str, span: &crate::error::Span) -> Result<()> {
    if matches!(value, Value::Null) {
        Err(XsqlError::spanned(
            format!("{side}-hand attribute is missing (value is null)"),
            *span,
        ))
    } else {
        Ok(())
    }
}

fn numeric_operands(l: &Value, r: &Value, span: &crate::error::Span) -> Result<(f64, f64)> {
    match (l.as_num(), r.as_num()) {
        (Some(a), Some(b)) => Ok((a, b)),
        _ => Err(XsqlError::spanned(
            format!(
                "arithmetic needs numeric operands, got `{}` and `{}`",
                l.to_display(),
                r.to_display()
            ),
            *span,
        )),
    }
}
