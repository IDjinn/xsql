//! Interpreter.
//!
//! Execution model:
//! 1. All distinct `USE` sources are loaded and parsed **in parallel**.
//! 2. Blocks are grouped by target document; groups for different documents
//!    run **in parallel** (blocks within one document stay sequential).
//!    SELECT output is buffered per block and flushed in script order.
//! 3. Large FOREACH loops use evaluate-then-apply: expressions are evaluated
//!    read-only **in parallel** over the children (each producing an ordered
//!    mutation plan), then plans are applied sequentially — preserving exact
//!    sequential semantics. Loops with a top-level BREAK stay fully
//!    sequential (a BREAK inside a nested FOREACH only ends that inner loop,
//!    so it doesn't force sequential execution). Nested loops only ever
//!    touch the current top-level child's subtree, which is what keeps the
//!    parallel planning safe. TAG- and ROOT-based loops may match nested
//!    elements; they only run parallel after a disjointness check (no match
//!    is an ancestor of another) — otherwise they stay sequential.
//! 4. Modified documents are serialized in parallel and appended to the
//!    output in first-use order.
//!
//! Global `SET` statements (FORMAT, IGNORE_COMMENTS, ANALYZE) are resolved
//! up front and apply to the whole run; `ANALYZE` prints a per-stage timing
//! report to stderr.

pub mod value;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::ast::{BinOp, Block, Expr, Foreach, LoopSource, Op, Script, Selector, Settings, Source, Verb};
use crate::error::{Result, Span, XsqlError};
use crate::xml::dom::{Document, Element, NodeId};
#[cfg(not(feature = "simd"))]
use crate::xml::parse::{parse_document_opts, parse_fragment_opts};
#[cfg(feature = "simd")]
use crate::xml::parse_simd::{parse_document_opts, parse_fragment_opts};
use crate::xml::serialize::{escape_attr_into, serialize_document_opts, serialize_subtree_as_into};

use value::Value;

/// Children count from which FOREACH switches to parallel evaluation.
const PAR_FOREACH_THRESHOLD: usize = 1024;

/// Timing lines collected for `ANALYZE`. The caller can prepend its own
/// stages (lex, parse, stdin read) and append trailing ones (stdout write)
/// before rendering.
pub struct AnalyzeReport {
    pub lines: Vec<(String, Duration)>,
    /// Total in-memory DOM footprint of every loaded document, in bytes.
    pub memory_bytes: Option<usize>,
}

impl AnalyzeReport {
    pub fn prepend(&mut self, lines: Vec<(String, Duration)>) {
        self.lines.splice(0..0, lines);
    }

    pub fn push(&mut self, label: impl Into<String>, time: Duration) {
        self.lines.push((label.into(), time));
    }

    /// `total` is wall-clock time measured by the caller (stages overlap
    /// under parallelism, so summing lines would overstate it).
    pub fn render(&self, total: Duration) -> String {
        let label_width = self
            .lines
            .iter()
            .map(|(label, _)| label.len())
            .max()
            .unwrap_or(0)
            .max(44);
        let width = label_width + 13;
        let mut out = format!("-- ANALYZE {}\n", "-".repeat(width.saturating_sub(11)));
        for (label, time) in &self.lines {
            out.push_str(&format!("{label:<label_width$}{:>13}\n", fmt_duration(*time)));
        }
        if let Some(bytes) = self.memory_bytes {
            out.push_str(&format!(
                "{:<label_width$}{:>13}\n",
                "memory (documents)",
                fmt_bytes(bytes)
            ));
        }
        out.push_str(&format!("{:<label_width$}{:>13}\n", "total", fmt_duration(total)));
        out.push_str(&format!("{}\n", "-".repeat(width)));
        out
    }
}

pub fn run(script: &Script, stdin_xml: Option<String>) -> Result<String> {
    let start = Instant::now();
    let (out, report) = run_with_report(script, stdin_xml)?;
    if let Some(report) = report {
        eprint!("{}", report.render(start.elapsed()));
    }
    Ok(out)
}

/// Like [`run`], but returns the `ANALYZE` timing report (when the script
/// enables it) instead of printing it.
pub fn run_with_report(
    script: &Script,
    stdin_xml: Option<String>,
) -> Result<(String, Option<AnalyzeReport>)> {
    let settings = Settings::resolve(&script.settings);

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

    // 1. Parallel load, file read and XML parse timed separately.
    let loaded: Vec<(Document, Vec<(String, Duration)>)> = sources
        .par_iter()
        .map(|source| {
            let mut lines = Vec::new();
            let read_start = Instant::now();
            let (name, content) = read_source(source, stdin_xml.as_deref())?;
            if matches!(source, Source::File(_)) {
                lines.push((format!("{:<10} {name}", "read"), read_start.elapsed()));
            }
            let parse_start = Instant::now();
            let doc = parse_document_opts(&content, !settings.ignore_comments)
                .map_err(|e| XsqlError::plain(format!("{name}: {e}")))?;
            lines.push((format!("{:<10} {name}", "parse xml"), parse_start.elapsed()));
            Ok((doc, lines))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut docs = Vec::with_capacity(loaded.len());
    let mut load_lines = Vec::new();
    for (doc, lines) in loaded {
        docs.push(doc);
        load_lines.extend(lines);
    }

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
        timings: Vec<(usize, Duration)>,
    }

    let runs: Vec<DocRun> = docs
        .par_iter_mut()
        .zip(&per_doc)
        .map(|(doc, blocks)| {
            let mut outputs = Vec::new();
            let mut modified = false;
            let mut timings = Vec::new();
            for &(block_idx, block) in blocks {
                let start = Instant::now();
                let result = run_block(doc, block, &settings)?;
                timings.push((block_idx, start.elapsed()));
                modified |= result.modified;
                if let Some(text) = result.output {
                    outputs.push((block_idx, text));
                }
            }
            Ok(DocRun { outputs, modified, timings })
        })
        .collect::<Result<Vec<_>>>()?;

    // 3. Flush SELECT outputs in script order.
    let mut selects: Vec<(usize, String)> =
        runs.iter().flat_map(|r| r.outputs.iter().cloned()).collect();
    selects.sort_by_key(|(idx, _)| *idx);

    // 4. Serialize modified documents in parallel, first-use order.
    let serialize_start = Instant::now();
    let serialized: Vec<String> = docs
        .par_iter()
        .zip(&runs)
        .filter(|(_, run)| run.modified)
        .map(|(doc, _)| serialize_document_opts(doc, settings.format))
        .collect();
    let serialize_time = serialize_start.elapsed();

    let assemble_start = Instant::now();
    let capacity = selects.iter().map(|(_, text)| text.len()).sum::<usize>()
        + serialized.iter().map(String::len).sum::<usize>();
    let mut out = String::with_capacity(capacity);
    for (_, text) in selects {
        out.push_str(&text);
    }
    for text in serialized {
        out.push_str(&text);
    }
    let assemble_time = assemble_start.elapsed();

    let report = settings.analyze.then(|| {
        let mut lines = load_lines;
        let mut block_times: Vec<(usize, Duration)> =
            runs.iter().flat_map(|r| r.timings.iter().copied()).collect();
        block_times.sort_by_key(|(idx, _)| *idx);
        for (idx, time) in block_times {
            let block = &script.blocks[idx];
            lines.push((
                format!(
                    "block #{:<3} {:<8} {}",
                    idx + 1,
                    verb_label(&block.verb),
                    block.source.describe()
                ),
                time,
            ));
        }
        lines.push(("serialize".to_string(), serialize_time));
        lines.push(("assemble output".to_string(), assemble_time));
        AnalyzeReport {
            lines,
            memory_bytes: Some(docs.iter().map(Document::memory_bytes).sum()),
        }
    });

    Ok((out, report))
}

fn verb_label(verb: &Verb) -> &'static str {
    match verb {
        Verb::Select { .. } => "SELECT",
        Verb::ReplaceGroup { .. } => "REPLACE",
        Verb::InsertInto { .. } => "INSERT",
        Verb::MergeInto { .. } => "MERGE",
        Verb::DeleteGroup { .. } | Verb::DeleteTag { .. } => "DELETE",
        Verb::RenameGroup { .. } | Verb::RenameTag { .. } => "RENAME",
        Verb::Foreach(_) => "FOREACH",
    }
}

fn fmt_duration(d: Duration) -> String {
    let us = d.as_micros();
    if us >= 1000 {
        format!("{:.3} ms", us as f64 / 1000.0)
    } else {
        format!("{us} us")
    }
}

fn fmt_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// A named savepoint: the document snapshot taken at CHECKPOINT time.
struct Checkpoint {
    name: String,
    doc: Document,
}

/// Per-document REPL state: the live document plus enough history to support
/// `COMMIT`/`ROLLBACK`/`CHECKPOINT`.
struct DocEntry {
    source: Source,
    doc: Document,
    /// Diverged from `baseline` since the last COMMIT/ROLLBACK.
    dirty: bool,
    /// Snapshot to restore on a bare `ROLLBACK`: `doc` as of the last COMMIT
    /// (or BEGIN, or initial load).
    baseline: Document,
    /// Named savepoints from CHECKPOINT, oldest first.
    checkpoints: Vec<Checkpoint>,
}

/// What happened to one document on `COMMIT`.
pub enum CommitOutcome {
    /// Written to this file path.
    Saved(String),
    /// No file to write (`Source::Input`); here's the serialized text.
    PrintedToStdout(String),
    /// `std::fs::write` failed; the document is left dirty/unchanged.
    Failed(String),
}

pub struct CommitReport {
    pub outcomes: Vec<(Source, CommitOutcome)>,
    /// Concatenated serialized XML for every `Source::Input` doc committed
    /// this call (nothing to write to disk, so it's returned to print).
    pub stdout_text: String,
}

/// Incremental interpreter for the REPL: documents load lazily on first
/// `USE` and stay in memory (with their pending mutations) across submitted
/// statements. SELECT output is returned per `exec`; modified documents are
/// only serialized on demand via [`Session::dump_modified`], or written to
/// disk via [`Session::commit`].
pub struct Session {
    stdin_xml: Option<String>,
    docs: Vec<DocEntry>,
    /// Global settings; `SET`/`ANALYZE` statements persist across execs.
    settings: Settings,
}

impl Session {
    pub fn new(stdin_xml: Option<String>) -> Self {
        Self {
            stdin_xml,
            docs: Vec::new(),
            settings: Settings::default(),
        }
    }

    pub fn exec(&mut self, script: &Script) -> Result<String> {
        for stmt in &script.settings {
            self.settings.apply(stmt);
        }
        let mut snapshotted: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut snapshots: Vec<(usize, Document, bool)> = Vec::new();
        let outcome = self.run_blocks(script, &mut snapshotted, &mut snapshots);
        if outcome.is_err() {
            for (idx, doc, dirty) in snapshots {
                self.docs[idx].doc = doc;
                self.docs[idx].dirty = dirty;
            }
        }
        outcome
    }

    /// Runs every block in `script`, snapshotting each distinct document the
    /// first time this call touches it (into `snapshots`) so `exec` can
    /// restore everything atomically if a later block errors.
    fn run_blocks(
        &mut self,
        script: &Script,
        snapshotted: &mut std::collections::HashSet<usize>,
        snapshots: &mut Vec<(usize, Document, bool)>,
    ) -> Result<String> {
        let settings = self.settings;
        let total_start = Instant::now();
        let mut timings: Vec<(String, Duration)> = Vec::new();
        let mut out = String::new();
        for (block_idx, block) in script.blocks.iter().enumerate() {
            let (idx, load_times) = self.doc_index(&block.source)?;
            if snapshotted.insert(idx) {
                snapshots.push((idx, self.docs[idx].doc.clone(), self.docs[idx].dirty));
            }
            if settings.analyze {
                if let Some((read_time, parse_time)) = load_times {
                    if matches!(block.source, Source::File(_)) {
                        timings.push((
                            format!("{:<10} {}", "read", block.source.describe()),
                            read_time,
                        ));
                    }
                    timings.push((
                        format!("{:<10} {}", "parse xml", block.source.describe()),
                        parse_time,
                    ));
                }
            }
            let entry = &mut self.docs[idx];
            let start = Instant::now();
            let result = run_block(&mut entry.doc, block, &settings)?;
            if settings.analyze {
                timings.push((
                    format!(
                        "block #{:<3} {:<8} {}",
                        block_idx + 1,
                        verb_label(&block.verb),
                        block.source.describe()
                    ),
                    start.elapsed(),
                ));
            }
            entry.dirty |= result.modified;
            if let Some(text) = result.output {
                out.push_str(&text);
            }
        }
        if settings.analyze && !script.blocks.is_empty() {
            let report = AnalyzeReport {
                lines: timings,
                memory_bytes: Some(self.docs.iter().map(|e| e.doc.memory_bytes()).sum()),
            };
            eprint!("{}", report.render(total_start.elapsed()));
        }
        Ok(out)
    }

    /// Serializes every modified document, first-use order.
    pub fn dump_modified(&self) -> String {
        self.docs
            .iter()
            .filter(|e| e.dirty)
            .map(|e| serialize_document_opts(&e.doc, self.settings.format))
            .collect()
    }

    pub fn has_modifications(&self) -> bool {
        self.docs.iter().any(|e| e.dirty)
    }

    /// Writes every dirty document to disk (or, for `Source::Input`
    /// documents with no file to write, appends its text to
    /// [`CommitReport::stdout_text`]). On success a document's baseline is
    /// reset to the just-written state and its checkpoints are cleared.
    pub fn commit(&mut self) -> CommitReport {
        let mut outcomes = Vec::new();
        let mut stdout_text = String::new();
        for entry in &mut self.docs {
            if !entry.dirty {
                continue;
            }
            let text = serialize_document_opts(&entry.doc, self.settings.format);
            match &entry.source {
                Source::File(path) => match std::fs::write(path, &text) {
                    Ok(()) => {
                        entry.baseline = entry.doc.clone();
                        entry.checkpoints.clear();
                        entry.dirty = false;
                        outcomes.push((entry.source.clone(), CommitOutcome::Saved(path.clone())));
                    }
                    Err(e) => outcomes.push((entry.source.clone(), CommitOutcome::Failed(e.to_string()))),
                },
                Source::Input => {
                    stdout_text.push_str(&text);
                    entry.baseline = entry.doc.clone();
                    entry.checkpoints.clear();
                    entry.dirty = false;
                    outcomes.push((entry.source.clone(), CommitOutcome::PrintedToStdout(text)));
                }
            }
        }
        CommitReport { outcomes, stdout_text }
    }

    /// `to = None`: restores every document to its last-COMMIT/load baseline
    /// and drops all checkpoints. `to = Some(name)`: restores every document
    /// that has a checkpoint named `name` to that snapshot, keeping it but
    /// dropping any later checkpoints (SQL `ROLLBACK TO SAVEPOINT`
    /// semantics). Returns the sources touched, or an error if `name` names
    /// a checkpoint that exists on no loaded document.
    pub fn rollback(&mut self, to: Option<&str>) -> std::result::Result<Vec<Source>, String> {
        let mut touched = Vec::new();
        match to {
            None => {
                for entry in &mut self.docs {
                    if entry.dirty || !entry.checkpoints.is_empty() {
                        entry.doc = entry.baseline.clone();
                        entry.checkpoints.clear();
                        entry.dirty = false;
                        touched.push(entry.source.clone());
                    }
                }
                Ok(touched)
            }
            Some(name) => {
                let found_any = self
                    .docs
                    .iter()
                    .any(|e| e.checkpoints.iter().any(|c| c.name == name));
                if !found_any {
                    return Err(format!("no checkpoint named `{name}`"));
                }
                for entry in &mut self.docs {
                    if let Some(pos) = entry.checkpoints.iter().position(|c| c.name == name) {
                        entry.doc = entry.checkpoints[pos].doc.clone();
                        entry.checkpoints.truncate(pos + 1);
                        entry.dirty = true;
                        touched.push(entry.source.clone());
                    }
                }
                Ok(touched)
            }
        }
    }

    /// Creates (or replaces) a named checkpoint on every loaded document.
    /// Returns how many documents got one.
    pub fn checkpoint(&mut self, name: &str) -> usize {
        let mut count = 0;
        for entry in &mut self.docs {
            let snapshot = Checkpoint {
                name: name.to_string(),
                doc: entry.doc.clone(),
            };
            if let Some(existing) = entry.checkpoints.iter_mut().find(|c| c.name == name) {
                *existing = snapshot;
            } else {
                entry.checkpoints.push(snapshot);
            }
            count += 1;
        }
        count
    }

    /// Clears every document's checkpoint stack (the session is always
    /// implicitly "in a transaction", so BEGIN doesn't touch the rollback
    /// baseline — it's just a "forget my savepoints" reset). Returns how
    /// many checkpoints were discarded in total.
    pub fn begin(&mut self) -> usize {
        self.docs.iter_mut().map(|e| std::mem::take(&mut e.checkpoints).len()).sum()
    }

    /// Returns the document's index, plus (read, XML-parse) times when this
    /// call is the one that loaded it.
    fn doc_index(&mut self, source: &Source) -> Result<(usize, Option<(Duration, Duration)>)> {
        if let Some(idx) = self.docs.iter().position(|e| &e.source == source) {
            return Ok((idx, None));
        }
        if *source == Source::Input && self.stdin_xml.is_none() {
            return Err(XsqlError::plain(
                "`USE INPUT` needs an XML document piped on stdin \
                 (not available in interactive mode)",
            ));
        }
        let read_start = Instant::now();
        let (name, content) = read_source(source, self.stdin_xml.as_deref())?;
        let read_time = read_start.elapsed();
        let parse_start = Instant::now();
        let doc = parse_document_opts(&content, !self.settings.ignore_comments)
            .map_err(|e| XsqlError::plain(format!("{name}: {e}")))?;
        let times = (read_time, parse_start.elapsed());
        self.docs.push(DocEntry {
            source: source.clone(),
            baseline: doc.clone(),
            doc,
            dirty: false,
            checkpoints: Vec::new(),
        });
        Ok((self.docs.len() - 1, Some(times)))
    }
}

fn read_source(source: &Source, stdin_xml: Option<&str>) -> Result<(String, String)> {
    match source {
        Source::File(path) => Ok((
            path.clone(),
            std::fs::read_to_string(path)
                .map_err(|e| XsqlError::plain(format!("cannot read `{path}`: {e}")))?,
        )),
        Source::Input => Ok((
            "<stdin>".to_string(),
            stdin_xml.expect("checked by caller").to_string(),
        )),
    }
}

struct BlockResult {
    output: Option<String>,
    modified: bool,
}

fn run_block(doc: &mut Document, block: &Block, settings: &Settings) -> Result<BlockResult> {
    match &block.verb {
        Verb::Select { target, alias, foreach } => {
            let alias = alias.as_deref();
            match foreach {
                // `SELECT GROUP g` prints the group element itself;
                // `SELECT TAG t` prints every matching element.
                None => {
                    let ids = match target {
                        Selector::Group(group) => vec![resolve_group(doc, group, block)?],
                        Selector::Tag(tag) => resolve_tags(doc, tag, block)?,
                        Selector::Root => doc.all_elements(),
                    };
                    let mut text = String::new();
                    for &id in &ids {
                        serialize_subtree_as_into(doc, id, settings.format, alias, &mut text);
                    }
                    Ok(BlockResult { output: Some(text), modified: false })
                }
                Some(foreach) => {
                    let elements = select_elements(doc, target, block)?;
                    let outcome = run_foreach(doc, elements, foreach)?;
                    // An OUTPUT op takes over what gets printed; without one
                    // the elements passing every WHERE print in full.
                    let text = if foreach_has_output(foreach) {
                        render_emits(doc, &outcome.emits, settings, alias)
                    } else {
                        let mut text = String::new();
                        for &id in &outcome.selected {
                            serialize_subtree_as_into(doc, id, settings.format, alias, &mut text);
                        }
                        text
                    };
                    Ok(BlockResult {
                        output: Some(text),
                        modified: outcome.mutations > 0,
                    })
                }
            }
        }
        Verb::Foreach(foreach) => {
            let elements = loop_elements(doc, &foreach.source, block)?;
            let outcome = run_foreach(doc, elements, foreach)?;
            // A mutation loop with OUTPUT also prints its emissions.
            let output = foreach_has_output(foreach)
                .then(|| render_emits(doc, &outcome.emits, settings, None));
            Ok(BlockResult {
                output,
                modified: outcome.mutations > 0,
            })
        }
        Verb::ReplaceGroup { group, xml, xml_span } => {
            let group_id = resolve_group(doc, group, block)?;
            let fragment = parse_fragment_opts(xml, !settings.ignore_comments)
                .map_err(|e| XsqlError::spanned(format!("bad RAW XML: {e}"), *xml_span))?;
            for child in std::mem::take(&mut doc.node_mut(group_id).children) {
                doc.node_mut(child).parent = None;
            }
            doc.graft(fragment, group_id);
            Ok(BlockResult { output: None, modified: true })
        }
        Verb::InsertInto { group, xml, xml_span } => {
            let group_id = resolve_group(doc, group, block)?;
            let fragment = parse_fragment_opts(xml, !settings.ignore_comments)
                .map_err(|e| XsqlError::spanned(format!("bad RAW XML: {e}"), *xml_span))?;
            doc.graft(fragment, group_id);
            Ok(BlockResult { output: None, modified: true })
        }
        Verb::MergeInto { group, xml, xml_span } => {
            let group_id = resolve_group(doc, group, block)?;
            let fragment = parse_fragment_opts(xml, !settings.ignore_comments)
                .map_err(|e| XsqlError::spanned(format!("bad RAW XML: {e}"), *xml_span))?;
            let modified = merge_into(doc, group_id, &fragment);
            Ok(BlockResult { output: None, modified })
        }
        Verb::DeleteGroup { group, ignore } => match doc.find_group(group) {
            Some(group_id) => {
                doc.detach(group_id);
                Ok(BlockResult { output: None, modified: true })
            }
            None if *ignore => Ok(BlockResult { output: None, modified: false }),
            None => Err(group_not_found(group, block)),
        },
        Verb::DeleteTag { tag, ignore } => {
            let matches = doc.find_tags(tag);
            if matches.is_empty() {
                return if *ignore {
                    Ok(BlockResult { output: None, modified: false })
                } else {
                    Err(tag_not_found(tag, block))
                };
            }
            for id in matches {
                doc.detach(id);
            }
            Ok(BlockResult { output: None, modified: true })
        }
        Verb::RenameGroup { group, new_tag, ignore } => match doc.find_group(group) {
            Some(id) => {
                doc.node_mut(id).tag = new_tag.clone();
                Ok(BlockResult { output: None, modified: true })
            }
            None if *ignore => Ok(BlockResult { output: None, modified: false }),
            None => Err(group_not_found(group, block)),
        },
        Verb::RenameTag { tag, new_tag, ignore } => {
            let matches = doc.find_tags(tag);
            if matches.is_empty() {
                return if *ignore {
                    Ok(BlockResult { output: None, modified: false })
                } else {
                    Err(tag_not_found(tag, block))
                };
            }
            for id in matches {
                doc.node_mut(id).tag = new_tag.clone();
            }
            Ok(BlockResult { output: None, modified: true })
        }
    }
}

fn resolve_group(doc: &Document, group: &str, block: &Block) -> Result<NodeId> {
    doc.find_group(group).ok_or_else(|| group_not_found(group, block))
}

fn resolve_tags(doc: &Document, tag: &str, block: &Block) -> Result<Vec<NodeId>> {
    let matches = doc.find_tags(tag);
    if matches.is_empty() {
        Err(tag_not_found(tag, block))
    } else {
        Ok(matches)
    }
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

fn tag_not_found(tag: &str, block: &Block) -> XsqlError {
    XsqlError::spanned(
        format!("no element with tag `{tag}` in {}", block.source.describe()),
        block.span,
    )
}

/// The elements a top-level loop iterates. Group children are siblings, so
/// their subtrees are disjoint and parallel planning is safe; tag matches can
/// nest inside each other, so disjointness is only known after checking.
struct LoopElems {
    elems: Vec<NodeId>,
    /// `true` when the subtrees are known disjoint (group children). Tag
    /// matches are verified lazily in [`run_foreach`] only when the loop is
    /// big enough for parallel evaluation to matter.
    disjoint: bool,
}

fn loop_elements(doc: &Document, source: &LoopSource, block: &Block) -> Result<LoopElems> {
    match source {
        LoopSource::Name(group) => {
            let group_id = resolve_group(doc, group, block)?;
            Ok(LoopElems { elems: non_comment_children(doc, group_id), disjoint: true })
        }
        LoopSource::Tag(tag) => {
            Ok(LoopElems { elems: resolve_tags(doc, tag, block)?, disjoint: false })
        }
        LoopSource::Root => Ok(LoopElems { elems: doc.all_elements(), disjoint: false }),
    }
}

fn select_elements(doc: &Document, target: &Selector, block: &Block) -> Result<LoopElems> {
    match target {
        Selector::Group(group) => {
            let group_id = resolve_group(doc, group, block)?;
            Ok(LoopElems { elems: non_comment_children(doc, group_id), disjoint: true })
        }
        Selector::Tag(tag) => {
            Ok(LoopElems { elems: resolve_tags(doc, tag, block)?, disjoint: false })
        }
        Selector::Root => Ok(LoopElems { elems: doc.all_elements(), disjoint: false }),
    }
}

/// Comment nodes (IGNORE_COMMENTS = OFF) are not loop elements.
fn non_comment_children(doc: &Document, parent: NodeId) -> Vec<NodeId> {
    doc.node(parent)
        .children
        .iter()
        .copied()
        .filter(|&child| !doc.node(child).is_comment())
        .collect()
}

/// Whether no element in `elems` is an ancestor of another — the condition
/// for parallel evaluate-then-apply to preserve sequential semantics (a
/// nested match would otherwise miss its ancestor's pending writes).
fn elems_disjoint(doc: &Document, elems: &[NodeId]) -> bool {
    let set: std::collections::HashSet<NodeId> = elems.iter().copied().collect();
    elems.iter().all(|&id| {
        let mut parent = doc.node(id).parent;
        while let Some(p) = parent {
            if set.contains(&p) {
                return false;
            }
            parent = doc.node(p).parent;
        }
        true
    })
}

// ---------------------------------------------------------------------------
// MERGE INTO (upsert)
// ---------------------------------------------------------------------------

/// `MERGE INTO GROUP`: each fragment element is matched against the group's
/// children; matched elements get the cited attributes written over them
/// (other attributes preserved; the fragment's children, when present,
/// replace the existing ones), unmatched elements are inserted. Returns
/// whether anything actually changed, so idempotent re-runs don't mark the
/// document modified.
fn merge_into(doc: &mut Document, group_id: NodeId, fragment: &Document) -> bool {
    let mut modified = false;
    for &root in &fragment.roots {
        let frag_el = fragment.node(root);
        let target = if frag_el.is_comment() {
            None
        } else {
            find_merge_target(doc, group_id, frag_el)
        };
        match target {
            Some(existing) => {
                for (attr, value) in &frag_el.attrs {
                    if doc.node(existing).attr(attr) != Some(value.as_str()) {
                        doc.node_mut(existing).set_attr(attr, value.clone());
                        modified = true;
                    }
                }
                if !frag_el.children.is_empty() {
                    for child in std::mem::take(&mut doc.node_mut(existing).children) {
                        doc.node_mut(child).parent = None;
                    }
                    for &frag_child in &frag_el.children {
                        doc.copy_subtree(fragment, frag_child, existing);
                    }
                    modified = true;
                }
            }
            None => {
                doc.copy_subtree(fragment, root, group_id);
                modified = true;
            }
        }
    }
    modified
}

/// First non-comment child matching the fragment element: same `id`
/// attribute when the fragment cites one, else same `name`, else same tag.
fn find_merge_target(doc: &Document, group_id: NodeId, frag_el: &Element) -> Option<NodeId> {
    let key = frag_el
        .attr("id")
        .map(|v| ("id", v))
        .or_else(|| frag_el.attr("name").map(|v| ("name", v)));
    doc.node(group_id).children.iter().copied().find(|&child| {
        let el = doc.node(child);
        if el.is_comment() {
            return false;
        }
        match key {
            Some((k, v)) => el.attr(k) == Some(v),
            None => el.tag == frag_el.tag,
        }
    })
}

// ---------------------------------------------------------------------------
// FOREACH engine (evaluate-then-apply)
// ---------------------------------------------------------------------------

/// One planned mutation, kept in execution order (nested loops interleave
/// writes to the current element, its descendants and enclosing elements).
#[derive(Debug)]
enum Action {
    Set(String, String),
    DelAttr(String),
    DeleteElem,
}

/// One OUTPUT emission, in reach order. Projections (`OUTPUT a, b`) are
/// rendered at reach time from the overlay; `OUTPUT *` defers to the
/// serializer after mutations apply — same as a SELECT without OUTPUT.
#[derive(Debug)]
enum Emit {
    Node(NodeId),
    Text(String),
}

/// Planned effects for one top-level child element, computed read-only.
#[derive(Debug)]
struct ChildPlan {
    /// Passed every WHERE guard it reached (still true for deleted elements,
    /// but deleted elements are never selected for output).
    selected: bool,
    broke: bool,
    actions: Vec<(NodeId, Action)>,
    emits: Vec<Emit>,
    /// One entry per reach of an aggregate `OUTPUT` (usually zero or one;
    /// more than one if the aggregate OUTPUT sits inside a nested loop).
    /// Each entry has one slot per aggregate item, `None` when that item's
    /// argument was missing/non-numeric on that reach (skipped, like a null
    /// attribute elsewhere in the language).
    agg_contribs: Vec<Vec<Option<Value>>>,
}

struct ForeachOutcome {
    selected: Vec<NodeId>,
    emits: Vec<Emit>,
    mutations: usize,
    agg_contribs: Vec<Vec<Option<Value>>>,
}

/// Whether the loop (or any nested loop) contains an OUTPUT op — when it
/// does, SELECT prints the emissions instead of the legacy selected list.
fn foreach_has_output(foreach: &Foreach) -> bool {
    foreach.ops.iter().any(|op| match op {
        Op::Output { .. } => true,
        Op::Foreach(inner) => foreach_has_output(inner),
        _ => false,
    })
}

/// Collects every `OUTPUT` op in a foreach, including inside nested loops.
fn collect_outputs<'a>(foreach: &'a Foreach, out: &mut Vec<&'a Op>) {
    for op in &foreach.ops {
        match op {
            Op::Output { .. } => out.push(op),
            Op::Foreach(inner) => collect_outputs(inner, out),
            _ => {}
        }
    }
}

/// Known aggregate function names, valid only as a direct `OUTPUT` item.
const AGGREGATE_FUNCS: &[&str] =
    &["COUNT", "MIN", "MAX", "SUM", "AVG", "FIRST", "LAST", "ANY", "ALL"];

/// One aggregate `OUTPUT` item: just the function name (uppercase) — `*` vs
/// a real expression is a plan-time distinction, not needed once validated.
struct AggItem {
    func: String,
}

/// If this foreach's only `OUTPUT` is a pure-aggregate one (every item a
/// `COUNT`/`MIN`/`MAX`/`SUM`/`AVG`/`FIRST`/`LAST`/`ANY`/`ALL` call), returns
/// its item list. A `None` OUTPUT (absent, or plain) means no aggregate
/// handling is needed. Mixing an aggregate OUTPUT with any other OUTPUT —
/// aggregate or plain — in the same foreach is rejected: there's no single
/// row for a per-element OUTPUT and a whole-loop aggregate to share.
fn aggregate_output(foreach: &Foreach) -> Result<Option<Vec<AggItem>>> {
    let mut outputs = Vec::new();
    collect_outputs(foreach, &mut outputs);
    let is_agg = |items: &[(Expr, String)]| items.iter().any(|(e, _)| matches!(e, Expr::Call { .. }));
    match outputs.as_slice() {
        [Op::Output { items, span, .. }] if is_agg(items) => {
            if !items.iter().all(|(e, _)| matches!(e, Expr::Call { .. })) {
                return Err(XsqlError::spanned(
                    "cannot mix aggregate functions with plain attributes/expressions \
                     in the same OUTPUT",
                    *span,
                ));
            }
            let mut names = Vec::with_capacity(items.len());
            for (expr, _name) in items {
                let Expr::Call { func, arg, span: call_span } = expr else { unreachable!() };
                let func = func.to_ascii_uppercase();
                if !AGGREGATE_FUNCS.contains(&func.as_str()) {
                    return Err(XsqlError::spanned(
                        format!(
                            "unknown aggregate function `{func}` (known: {})",
                            AGGREGATE_FUNCS.join(", ")
                        ),
                        *call_span,
                    ));
                }
                if matches!(**arg, Expr::Star(_)) && func != "COUNT" {
                    return Err(XsqlError::spanned(
                        format!("`{func}(*)` is invalid: only COUNT(*) supports `*`"),
                        *call_span,
                    ));
                }
                names.push(AggItem { func });
            }
            Ok(Some(names))
        }
        outs if outs.iter().any(|op| {
            matches!(op, Op::Output { items, .. } if is_agg(items))
        }) =>
        {
            let span = outs
                .iter()
                .find_map(|op| match op {
                    Op::Output { items, span, .. } if is_agg(items) => Some(*span),
                    _ => None,
                })
                .expect("checked above");
            Err(XsqlError::spanned(
                "an aggregate OUTPUT (COUNT/MIN/MAX/SUM/AVG) must be the only OUTPUT in its FOREACH",
                span,
            ))
        }
        _ => Ok(None),
    }
}

/// Combines every reach's per-item contribution into the loop's final
/// aggregate values, formatted as a comma-joined line (no XML — the whole
/// point of an aggregate OUTPUT is a bare value, not a per-element row).
/// Aliases are not printed: with a single row there is nothing to
/// disambiguate. `COUNT`/`MIN`/`MAX`/`SUM`/`AVG` report `0` when nothing
/// contributed; `FIRST`/`LAST` report an empty value; `ANY` reports `false`;
/// `ALL` reports `true` (vacuously, same as SQL).
fn render_aggregate(items: &[AggItem], contribs: &[Vec<Option<Value>>]) -> String {
    let mut parts = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let col: Vec<&Value> = contribs
            .iter()
            .filter_map(|reach| reach.get(i).and_then(|v| v.as_ref()))
            .collect();
        let text = match item.func.as_str() {
            "COUNT" => Value::Num(col.len() as f64).to_display(),
            "SUM" | "AVG" | "MIN" | "MAX" => {
                let nums: Vec<f64> = col.iter().filter_map(|v| v.as_num()).collect();
                let result = match item.func.as_str() {
                    "SUM" => nums.iter().sum(),
                    "AVG" => {
                        if nums.is_empty() {
                            0.0
                        } else {
                            nums.iter().sum::<f64>() / nums.len() as f64
                        }
                    }
                    "MIN" => nums.iter().copied().fold(f64::INFINITY, f64::min),
                    "MAX" => nums.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                    _ => unreachable!(),
                };
                let result = if result.is_finite() { result } else { 0.0 };
                Value::Num(result).to_display()
            }
            "FIRST" => col.first().map(|v| v.to_display()).unwrap_or_default(),
            "LAST" => col.last().map(|v| v.to_display()).unwrap_or_default(),
            "ANY" => Value::Bool(col.iter().any(|v| v.truthy())).to_display(),
            "ALL" => Value::Bool(col.iter().all(|v| v.truthy())).to_display(),
            _ => unreachable!("validated by aggregate_output"),
        };
        parts.push(text);
    }
    parts.join(",") + "\n"
}

/// Renders emissions in reach order. `Emit::Node` serializes after the
/// mutations were applied, matching what a SELECT without OUTPUT prints.
/// `alias` (from `SELECT ... AS alias`) renames each `Emit::Node`'s outer
/// tag on the way out; aggregate/attribute-row `Emit::Text` lines are
/// unaffected (they carry no reusable tag).
fn render_emits(doc: &Document, emits: &[Emit], settings: &Settings, alias: Option<&str>) -> String {
    let mut text = String::new();
    for emit in emits {
        match emit {
            Emit::Text(line) => text.push_str(line),
            Emit::Node(id) => serialize_subtree_as_into(doc, *id, settings.format, alias, &mut text),
        }
    }
    text
}

fn run_foreach(
    doc: &mut Document,
    elements: LoopElems,
    foreach: &Foreach,
) -> Result<ForeachOutcome> {
    let LoopElems { elems: children, disjoint } = elements;
    let has_break = foreach.ops.iter().any(|op| matches!(op, Op::Break));
    let agg = aggregate_output(foreach)?;

    let mut outcome = ForeachOutcome {
        selected: Vec::new(),
        emits: Vec::new(),
        mutations: 0,
        agg_contribs: Vec::new(),
    };

    // Parallel planning requires disjoint subtrees; tag matches only get the
    // (linear) ancestor check once the loop is big enough to parallelize.
    let parallel = !has_break
        && children.len() >= PAR_FOREACH_THRESHOLD
        && (disjoint || elems_disjoint(doc, &children));
    if parallel {
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

    if let Some(items) = agg {
        outcome.emits.push(Emit::Text(render_aggregate(&items, &outcome.agg_contribs)));
    }

    Ok(outcome)
}

fn apply_plan(doc: &mut Document, child: NodeId, plan: ChildPlan, outcome: &mut ForeachOutcome) {
    outcome.mutations += plan.actions.len();
    let mut child_deleted = false;
    for (node, action) in plan.actions {
        match action {
            Action::Set(attr, value) => doc.node_mut(node).set_attr(&attr, value),
            Action::DelAttr(attr) => {
                doc.node_mut(node).remove_attr(&attr);
            }
            Action::DeleteElem => {
                doc.detach(node);
                child_deleted |= node == child;
            }
        }
    }
    if plan.selected && !child_deleted {
        outcome.selected.push(child);
    }
    outcome.emits.extend(plan.emits);
    outcome.agg_contribs.extend(plan.agg_contribs);
}

/// One enclosing FOREACH binding during planning: the names resolving to
/// `node`, plus the overlay of pending attribute writes so later expressions
/// observe earlier ones.
struct Scope<'a> {
    var: &'a str,
    /// Group-name alias for the element (the top-level loop's group, or a
    /// nested loop's sub-group name); empty when the nested loop iterates an
    /// enclosing variable's children — that name must keep resolving to the
    /// enclosing scope.
    alias: &'a str,
    node: NodeId,
    changes: Vec<(String, Option<String>)>,
}

impl Scope<'_> {
    fn get(&self, doc: &Document, attr: &str) -> Option<String> {
        for (name, value) in self.changes.iter().rev() {
            if name == attr {
                return value.clone();
            }
        }
        doc.node(self.node).attr(attr).map(str::to_string)
    }
}

/// The loop variable, the group name and a bare (empty) prefix all refer to
/// the current element; enclosing loops' names stay visible (innermost wins).
fn lookup_scope(scopes: &[Scope], var: &str) -> Option<usize> {
    if var.is_empty() {
        return scopes.len().checked_sub(1);
    }
    scopes
        .iter()
        .rposition(|s| s.var == var || (!s.alias.is_empty() && s.alias == var))
}

fn resolve_scope(scopes: &[Scope], var: &str, span: Span) -> Result<usize> {
    lookup_scope(scopes, var).ok_or_else(|| {
        let known: Vec<String> = scopes
            .iter()
            .flat_map(|s| [s.var, s.alias])
            .filter(|n| !n.is_empty())
            .map(|n| format!("`{n}`"))
            .collect();
        XsqlError::spanned(
            format!("unknown variable `{var}` (in scope: {})", known.join(", ")),
            span,
        )
    })
}

fn plan_child(doc: &Document, child: NodeId, foreach: &Foreach) -> Result<ChildPlan> {
    let mut scopes = Vec::new();
    let mut actions = Vec::new();
    let mut emits = Vec::new();
    let mut agg_contribs = Vec::new();
    let (selected, broke) = plan_element(
        doc,
        child,
        foreach,
        foreach.source.name(),
        &mut scopes,
        &mut actions,
        &mut emits,
        &mut agg_contribs,
    )?;
    Ok(ChildPlan { selected, broke, actions, emits, agg_contribs })
}

/// Plans one element of a loop (recursing into nested FOREACH), read-only.
/// Returns (passed every WHERE it reached, hit BREAK).
#[allow(clippy::too_many_arguments)]
fn plan_element<'a>(
    doc: &Document,
    node: NodeId,
    foreach: &'a Foreach,
    alias: &'a str,
    scopes: &mut Vec<Scope<'a>>,
    actions: &mut Vec<(NodeId, Action)>,
    emits: &mut Vec<Emit>,
    agg_contribs: &mut Vec<Vec<Option<Value>>>,
) -> Result<(bool, bool)> {
    scopes.push(Scope { var: &foreach.var, alias, node, changes: Vec::new() });
    let result = plan_ops(doc, foreach, scopes, actions, emits, agg_contribs);
    scopes.pop();
    result
}

#[allow(clippy::too_many_arguments)]
fn plan_ops<'a>(
    doc: &Document,
    foreach: &'a Foreach,
    scopes: &mut Vec<Scope<'a>>,
    actions: &mut Vec<(NodeId, Action)>,
    emits: &mut Vec<Emit>,
    agg_contribs: &mut Vec<Vec<Option<Value>>>,
) -> Result<(bool, bool)> {
    let mut selected = true;
    for (op_idx, op) in foreach.ops.iter().enumerate() {
        match op {
            Op::Where(expr) => {
                if !eval_expr(doc, expr, scopes)?.truthy() {
                    selected = false;
                    break;
                }
            }
            Op::WhereRequired { var, attr, expr, span } => {
                let idx = resolve_scope(scopes, var, *span)?;
                if scopes[idx].get(doc, attr).is_none() {
                    return Err(XsqlError::spanned(
                        format!(
                            "attribute `{attr}` is REQUIRED but missing on <{}> \
                             (use plain WHERE to skip elements without it silently)",
                            doc.node(scopes[idx].node).tag
                        ),
                        *span,
                    ));
                }
                if !eval_expr(doc, expr, scopes)?.truthy() {
                    selected = false;
                    break;
                }
            }
            Op::Set { var, attr, value, span } => {
                let idx = resolve_scope(scopes, var, *span)?;
                let value = eval_expr(doc, value, scopes)?.to_display();
                let target = scopes[idx].node;
                scopes[idx].changes.push((attr.clone(), Some(value.clone())));
                actions.push((target, Action::Set(attr.clone(), value)));
            }
            Op::Merge { var, attr, value, span } => {
                let idx = resolve_scope(scopes, var, *span)?;
                if scopes[idx].get(doc, attr).is_none() {
                    let value = eval_expr(doc, value, scopes)?.to_display();
                    let target = scopes[idx].node;
                    scopes[idx].changes.push((attr.clone(), Some(value.clone())));
                    actions.push((target, Action::Set(attr.clone(), value)));
                }
            }
            Op::DeleteAttr { var, attr, ignore, span } => {
                let idx = resolve_scope(scopes, var, *span)?;
                if scopes[idx].get(doc, attr).is_some() {
                    scopes[idx].changes.push((attr.clone(), None));
                    actions.push((scopes[idx].node, Action::DelAttr(attr.clone())));
                } else if !ignore {
                    return Err(XsqlError::spanned(
                        format!(
                            "attribute `{attr}` not found on <{}> (use DELETE IGNORE to skip silently)",
                            doc.node(scopes[idx].node).tag
                        ),
                        *span,
                    ));
                }
            }
            Op::DeleteElem { var, ignore: _, span } => {
                let idx = resolve_scope(scopes, var, *span)?;
                actions.push((scopes[idx].node, Action::DeleteElem));
                // Deleting the current element ends its planning (later ops
                // would target a deleted element) — but a BREAK further down
                // still stops the loop (`DELETE FROM ... LIMIT 1`). Deleting
                // an enclosing element leaves this loop running over the
                // (already snapshotted) children.
                if idx == scopes.len() - 1 {
                    let breaks = foreach.ops[op_idx + 1..]
                        .iter()
                        .any(|op| matches!(op, Op::Break));
                    return Ok((selected, breaks));
                }
            }
            Op::Break => return Ok((selected, true)),
            Op::Foreach(inner) => {
                let (kids, inner_alias) = resolve_loop_elems(doc, scopes, inner)?;
                for kid in kids {
                    let (_, broke) = plan_element(
                        doc,
                        kid,
                        inner,
                        inner_alias,
                        scopes,
                        actions,
                        emits,
                        agg_contribs,
                    )?;
                    if broke {
                        break;
                    }
                }
            }
            Op::Output { all, items, span } => {
                let node = scopes.last().expect("OUTPUT has an enclosing scope").node;
                if *all {
                    emits.push(Emit::Node(node));
                } else if items.iter().any(|(e, _)| matches!(e, Expr::Call { .. })) {
                    if !items.iter().all(|(e, _)| matches!(e, Expr::Call { .. })) {
                        return Err(XsqlError::spanned(
                            "cannot mix aggregate functions with plain attributes/expressions \
                             in the same OUTPUT",
                            *span,
                        ));
                    }
                    let mut reach = Vec::with_capacity(items.len());
                    for (expr, _name) in items {
                        let Expr::Call { func, arg, span } = expr else { unreachable!() };
                        let func = func.to_ascii_uppercase();
                        if matches!(**arg, Expr::Star(_)) {
                            // `COUNT(*)`: counts the row unconditionally, no
                            // expression to evaluate.
                            reach.push(Some(Value::Bool(true)));
                            continue;
                        }
                        let value = eval_expr(doc, arg, scopes)?;
                        reach.push(match func.as_str() {
                            "COUNT" | "FIRST" | "LAST" => {
                                (!matches!(value, Value::Null)).then_some(value)
                            }
                            "MIN" | "MAX" | "SUM" | "AVG" => value.as_num().map(Value::Num),
                            "ANY" | "ALL" => Some(Value::Bool(value.truthy())),
                            other => {
                                return Err(XsqlError::spanned(
                                    format!(
                                        "unknown aggregate function `{other}` (known: {})",
                                        AGGREGATE_FUNCS.join(", ")
                                    ),
                                    *span,
                                ));
                            }
                        });
                    }
                    agg_contribs.push(reach);
                } else {
                    // Rendered at reach time from the overlay: SETs before
                    // the OUTPUT are visible, later ones are not. Missing
                    // attributes are omitted.
                    let mut line = format!("<{}", doc.node(node).tag);
                    for (expr, name) in items {
                        let value = eval_expr(doc, expr, scopes)?;
                        if !matches!(value, Value::Null) {
                            line.push(' ');
                            line.push_str(name);
                            line.push_str("=\"");
                            escape_attr_into(&value.to_display(), &mut line);
                            line.push('"');
                        }
                    }
                    line.push_str("/>\n");
                    emits.push(Emit::Text(line));
                }
            }
        }
    }
    Ok((selected, false))
}

/// Resolves what a nested `FOREACH v IN name` iterates: an enclosing loop
/// element's children when `name` is a variable in scope, otherwise a group
/// found inside the current element's subtree (staying inside the subtree is
/// what keeps parallel planning of sibling elements safe). `IN TAG t`
/// iterates every element with tag `t` inside the current element's subtree
/// (zero matches simply iterate nothing — heterogeneous documents are the
/// whole point of TAG). `IN ROOT` is the same, but with no tag filter at
/// all — for subtrees whose tag names aren't known ahead of time.
fn resolve_loop_elems<'a>(
    doc: &Document,
    scopes: &[Scope],
    inner: &'a Foreach,
) -> Result<(Vec<NodeId>, &'a str)> {
    let current = scopes.last().expect("nested loop has an enclosing scope").node;
    match &inner.source {
        LoopSource::Name(name) => {
            if let Some(idx) = lookup_scope(scopes, name) {
                return Ok((non_comment_children(doc, scopes[idx].node), ""));
            }
            match doc.find_group_within(current, name) {
                Some(id) => Ok((non_comment_children(doc, id), name.as_str())),
                None => Err(XsqlError::spanned(
                    format!(
                        "cannot iterate `{name}`: not a variable in scope nor a group inside <{}>",
                        doc.node(current).tag
                    ),
                    inner.span,
                )),
            }
        }
        LoopSource::Tag(tag) => Ok((doc.find_tags_within(current, tag), tag.as_str())),
        LoopSource::Root => Ok((doc.all_elements_within(current), "root")),
    }
}

fn eval_expr(doc: &Document, expr: &Expr, scopes: &[Scope]) -> Result<Value> {
    match expr {
        Expr::Str(s) => Ok(Value::Str(s.clone())),
        Expr::Num(n) => Ok(Value::Num(*n)),
        Expr::Attr { var, attr, span } => {
            let idx = resolve_scope(scopes, var, *span)?;
            Ok(scopes[idx].get(doc, attr).map_or(Value::Null, Value::Str))
        }
        Expr::Not(inner, _) => Ok(Value::Bool(!eval_expr(doc, inner, scopes)?.truthy())),
        Expr::Neg(inner, span) => {
            let value = eval_expr(doc, inner, scopes)?;
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
                    let l = eval_expr(doc, lhs, scopes)?;
                    return Ok(Value::Bool(
                        l.truthy() && eval_expr(doc, rhs, scopes)?.truthy(),
                    ));
                }
                BinOp::Or => {
                    let l = eval_expr(doc, lhs, scopes)?;
                    return Ok(Value::Bool(
                        l.truthy() || eval_expr(doc, rhs, scopes)?.truthy(),
                    ));
                }
                _ => {}
            }

            let l = eval_expr(doc, lhs, scopes)?;
            let r = eval_expr(doc, rhs, scopes)?;
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
        Expr::Call { func, span, .. } => Err(XsqlError::spanned(
            format!("`{func}(...)` is an aggregate function: it can only be used as a direct OUTPUT item"),
            *span,
        )),
        Expr::Star(span) => Err(XsqlError::spanned(
            "`*` is only valid as `COUNT(*)`'s argument",
            *span,
        )),
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
