//! Arena DOM -> XML text. Pretty-printed by default; compact single-line
//! output when `SET FORMAT = OFF`.

use super::dom::{Document, Element, NodeId};

const INDENT: &str = "    ";

/// Per-element slack on top of the measured strings: brackets, quotes,
/// closing tag punctuation, newline and a couple of indent levels.
const NODE_OVERHEAD: usize = 24;

/// Upper-bound-ish output size for one element (excluding its children):
/// tag written twice, attributes with `="` glue, text, plus overhead.
fn node_own_len(el: &Element) -> usize {
    let attrs: usize = el.attrs.iter().map(|(k, v)| k.len() + v.len() + 4).sum();
    2 * el.tag.len() + el.text.len() + attrs + NODE_OVERHEAD
}

/// Estimated serialized size of the whole document. Iterates the flat arena
/// (detached nodes are counted too, so this over-reserves slightly) — cheap
/// compared to the serialization itself, and avoids repeated regrows of the
/// output string on large documents.
fn estimate_document_len(doc: &Document) -> usize {
    let decl = if doc.had_decl { 48 } else { 0 };
    decl + doc.nodes.iter().map(node_own_len).sum::<usize>()
}

fn estimate_subtree_len(doc: &Document, id: NodeId) -> usize {
    let mut len = 0;
    let mut stack = vec![id];
    while let Some(node) = stack.pop() {
        let el = doc.node(node);
        len += node_own_len(el);
        stack.extend(&el.children);
    }
    len
}

pub fn serialize_document(doc: &Document) -> String {
    serialize_document_opts(doc, true)
}

pub fn serialize_document_opts(doc: &Document, pretty: bool) -> String {
    let mut out = String::with_capacity(estimate_document_len(doc));
    if doc.had_decl {
        out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    }
    for &root in &doc.roots {
        serialize_node(doc, root, 0, pretty, None, &mut out);
        if !pretty {
            out.push('\n');
        }
    }
    out
}

/// Serializes a single subtree (used by SELECT output).
pub fn serialize_subtree(doc: &Document, id: NodeId) -> String {
    serialize_subtree_opts(doc, id, true)
}

pub fn serialize_subtree_opts(doc: &Document, id: NodeId, pretty: bool) -> String {
    let mut out = String::new();
    serialize_subtree_into(doc, id, pretty, &mut out);
    out
}

/// Appends one subtree to an existing buffer (lets callers that render many
/// subtrees reuse a single pre-reserved string).
pub fn serialize_subtree_into(doc: &Document, id: NodeId, pretty: bool, out: &mut String) {
    serialize_subtree_as_into(doc, id, pretty, None, out)
}

/// Like [`serialize_subtree_into`], but `tag` (when given) replaces the
/// outermost element's tag on the way out — a display-only rename (`SELECT
/// ... AS alias`); descendants keep their real tags.
pub fn serialize_subtree_as_into(
    doc: &Document,
    id: NodeId,
    pretty: bool,
    tag: Option<&str>,
    out: &mut String,
) {
    out.reserve(estimate_subtree_len(doc, id));
    serialize_node(doc, id, 0, pretty, tag, out);
    if !pretty {
        out.push('\n');
    }
}

fn serialize_node(
    doc: &Document,
    id: NodeId,
    depth: usize,
    pretty: bool,
    tag_override: Option<&str>,
    out: &mut String,
) {
    let el = doc.node(id);
    let tag = tag_override.unwrap_or(&el.tag);
    if pretty {
        for _ in 0..depth {
            out.push_str(INDENT);
        }
    }
    if el.is_comment() {
        out.push_str("<!--");
        out.push_str(&el.text);
        out.push_str("-->");
        if pretty {
            out.push('\n');
        }
        return;
    }
    out.push('<');
    out.push_str(tag);
    for (k, v) in &el.attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        escape_into(v, true, out);
        out.push('"');
    }

    let text = el.text.trim();
    if el.children.is_empty() && text.is_empty() {
        out.push_str("/>");
        if pretty {
            out.push('\n');
        }
        return;
    }

    out.push('>');
    if el.children.is_empty() {
        // Text-only element stays on one line.
        escape_into(text, false, out);
    } else {
        if pretty {
            out.push('\n');
        }
        if !text.is_empty() {
            if pretty {
                for _ in 0..=depth {
                    out.push_str(INDENT);
                }
            }
            escape_into(text, false, out);
            if pretty {
                out.push('\n');
            }
        }
        for &child in &el.children {
            serialize_node(doc, child, depth + 1, pretty, None, out);
        }
        if pretty {
            for _ in 0..depth {
                out.push_str(INDENT);
            }
        }
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
    if pretty {
        out.push('\n');
    }
}

/// Escapes `value` for use inside a double-quoted attribute (used by the
/// OUTPUT projection, which renders elements outside this serializer).
pub fn escape_attr_into(value: &str, out: &mut String) {
    escape_into(value, true, out);
}

fn escape_into(value: &str, in_attr: bool, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if in_attr => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}
