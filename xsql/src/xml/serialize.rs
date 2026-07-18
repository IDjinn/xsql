//! Arena DOM -> pretty-printed XML text.

use super::dom::{Document, NodeId};

const INDENT: &str = "    ";

pub fn serialize_document(doc: &Document) -> String {
    let mut out = String::new();
    if doc.had_decl {
        out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    }
    for &root in &doc.roots {
        serialize_node(doc, root, 0, &mut out);
    }
    out
}

/// Serializes a single subtree (used by SELECT output).
pub fn serialize_subtree(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    serialize_node(doc, id, 0, &mut out);
    out
}

fn serialize_node(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
    let el = doc.node(id);
    for _ in 0..depth {
        out.push_str(INDENT);
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
        out.push_str("/>\n");
        return;
    }

    out.push('>');
    if el.children.is_empty() {
        // Text-only element stays on one line.
        escape_into(text, false, out);
    } else {
        out.push('\n');
        if !text.is_empty() {
            for _ in 0..=depth {
                out.push_str(INDENT);
            }
            escape_into(text, false, out);
            out.push('\n');
        }
        for &child in &el.children {
            serialize_node(doc, child, depth + 1, out);
        }
        for _ in 0..depth {
            out.push_str(INDENT);
        }
    }
    out.push_str("</");
    out.push_str(&el.tag);
    out.push_str(">\n");
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
