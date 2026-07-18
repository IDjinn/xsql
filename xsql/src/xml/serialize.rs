//! Arena DOM -> XML text. Pretty-printed by default; compact single-line
//! output when `SET FORMAT = OFF`.

use super::dom::{Document, NodeId};

const INDENT: &str = "    ";

pub fn serialize_document(doc: &Document) -> String {
    serialize_document_opts(doc, true)
}

pub fn serialize_document_opts(doc: &Document, pretty: bool) -> String {
    let mut out = String::new();
    if doc.had_decl {
        out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    }
    for &root in &doc.roots {
        serialize_node(doc, root, 0, pretty, &mut out);
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
    serialize_node(doc, id, 0, pretty, &mut out);
    if !pretty {
        out.push('\n');
    }
    out
}

fn serialize_node(doc: &Document, id: NodeId, depth: usize, pretty: bool, out: &mut String) {
    let el = doc.node(id);
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
    out.push_str(&el.tag);
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
            serialize_node(doc, child, depth + 1, pretty, out);
        }
        if pretty {
            for _ in 0..depth {
                out.push_str(INDENT);
            }
        }
    }
    out.push_str("</");
    out.push_str(&el.tag);
    out.push('>');
    if pretty {
        out.push('\n');
    }
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
