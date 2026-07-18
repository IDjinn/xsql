//! XML text -> arena DOM, built on quick-xml. Processing instructions and
//! DOCTYPE are skipped; comments are skipped too unless `keep_comments` is
//! set (`SET IGNORE_COMMENTS = OFF`), in which case they become nodes with
//! the reserved [`COMMENT_TAG`] tag.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::dom::{COMMENT_TAG, Document, Element, NodeId};

pub fn parse_document(source: &str) -> Result<Document, String> {
    parse_document_opts(source, false)
}

pub fn parse_document_opts(source: &str, keep_comments: bool) -> Result<Document, String> {
    let mut reader = Reader::from_str(source);
    reader.config_mut().trim_text(true);

    let mut doc = Document::default();
    let mut stack: Vec<NodeId> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Decl(_)) => doc.had_decl = true,
            Ok(Event::Start(start)) => {
                let id = open_element(&mut doc, &stack, &start)?;
                stack.push(id);
            }
            Ok(Event::Empty(start)) => {
                open_element(&mut doc, &stack, &start)?;
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Text(text)) => {
                if let Some(&id) = stack.last() {
                    let value = text.unescape().map_err(|e| e.to_string())?;
                    doc.node_mut(id).text.push_str(&value);
                }
            }
            Ok(Event::CData(cdata)) => {
                if let Some(&id) = stack.last() {
                    let value = String::from_utf8_lossy(&cdata).into_owned();
                    doc.node_mut(id).text.push_str(&value);
                }
            }
            Ok(Event::Comment(comment)) if keep_comments => {
                let text = String::from_utf8_lossy(comment.as_ref()).into_owned();
                let parent = stack.last().copied();
                let id = doc.push(Element {
                    tag: COMMENT_TAG.to_string(),
                    attrs: Vec::new(),
                    children: Vec::new(),
                    text,
                    parent,
                });
                match parent {
                    Some(p) => doc.node_mut(p).children.push(id),
                    None => doc.roots.push(id),
                }
            }
            Ok(Event::Comment(_) | Event::PI(_) | Event::DocType(_)) => {}
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(format!(
                    "malformed XML at byte {}: {e}",
                    reader.buffer_position()
                ));
            }
        }
    }

    Ok(doc)
}

/// Parses an XML fragment (zero or more sibling elements) into a standalone
/// document whose `roots` are the fragment's top-level elements.
pub fn parse_fragment(source: &str) -> Result<Document, String> {
    parse_document_opts(source, false)
}

pub fn parse_fragment_opts(source: &str, keep_comments: bool) -> Result<Document, String> {
    parse_document_opts(source, keep_comments)
}

fn open_element(
    doc: &mut Document,
    stack: &[NodeId],
    start: &BytesStart,
) -> Result<NodeId, String> {
    let tag = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut attrs = Vec::new();
    for attr in start.attributes() {
        let attr = attr.map_err(|e| format!("bad attribute in <{tag}>: {e}"))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let value = attr
            .unescape_value()
            .map_err(|e| format!("bad attribute value in <{tag}>: {e}"))?
            .into_owned();
        attrs.push((key, value));
    }

    let parent = stack.last().copied();
    let id = doc.push(Element {
        tag,
        attrs,
        children: Vec::new(),
        text: String::new(),
        parent,
    });
    match parent {
        Some(p) => doc.node_mut(p).children.push(id),
        None => doc.roots.push(id),
    }
    Ok(id)
}
