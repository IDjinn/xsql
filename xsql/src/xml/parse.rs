//! XML text -> arena DOM, built on quick-xml. Processing instructions and
//! DOCTYPE are skipped; comments are skipped too unless `keep_comments` is
//! set (`SET IGNORE_COMMENTS = OFF`), in which case they become nodes with
//! the reserved [`COMMENT_TAG`] tag.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::XmlParseError;
use super::dom::{COMMENT_TAG, Document, Element, NodeId};

pub fn parse_document(source: &str) -> Result<Document, String> {
    parse_document_opts(source, false)
}

pub fn parse_document_opts(source: &str, keep_comments: bool) -> Result<Document, String> {
    parse_document_diag(source, keep_comments).map_err(|e| e.describe(source))
}

/// Like [`parse_document_opts`] but keeps the failure position structured
/// instead of formatting it into the message (used by `xsql --check` to
/// render source context).
pub fn parse_document_diag(source: &str, keep_comments: bool) -> Result<Document, XmlParseError> {
    let mut reader = Reader::from_str(source);
    reader.config_mut().trim_text(true);
    // Reject `<a></b>`: without this quick-xml pairs any end tag with the
    // innermost open element, silently producing a garbage DOM.
    reader.config_mut().check_end_names = true;

    let mut doc = Document::default();
    let mut stack: Vec<NodeId> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Decl(_)) => doc.had_decl = true,
            Ok(Event::Start(start)) => {
                let id = open_element(&mut doc, &stack, &start, false)
                    .map_err(|message| at(&reader, message))?;
                stack.push(id);
            }
            Ok(Event::Empty(start)) => {
                open_element(&mut doc, &stack, &start, true)
                    .map_err(|message| at(&reader, message))?;
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Text(text)) => {
                if let Some(&id) = stack.last() {
                    let value = text
                        .unescape()
                        .map_err(|e| at(&reader, e.to_string()))?;
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
                    self_closing: false,
                });
                match parent {
                    Some(p) => doc.node_mut(p).children.push(id),
                    None => doc.roots.push(id),
                }
            }
            Ok(Event::Comment(_) | Event::PI(_) | Event::DocType(_)) => {}
            Ok(Event::Eof) => {
                if let Some(&id) = stack.last() {
                    return Err(XmlParseError {
                        message: format!(
                            "unexpected end of file, `<{}>` is never closed",
                            doc.node(id).tag
                        ),
                        byte: Some(source.len()),
                    });
                }
                break;
            }
            Err(e) => {
                return Err(XmlParseError {
                    message: e.to_string(),
                    byte: Some(reader.error_position() as usize),
                });
            }
        }
    }

    Ok(doc)
}

fn at(reader: &Reader<&[u8]>, message: String) -> XmlParseError {
    XmlParseError {
        message,
        byte: Some(reader.buffer_position() as usize),
    }
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
    self_closing: bool,
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
        self_closing,
    });
    match parent {
        Some(p) => doc.node_mut(p).children.push(id),
        None => doc.roots.push(id),
    }
    Ok(id)
}
