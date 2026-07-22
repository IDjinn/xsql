//! XML text -> arena DOM, built on the `simdxml` structural index instead of
//! quick-xml event streaming. Semantics mirror [`super::parse`]: PIs and
//! DOCTYPE are skipped, text events are trimmed and entity-decoded, CDATA is
//! appended raw, and comments become [`COMMENT_TAG`] nodes when
//! `keep_comments` is set.

use std::collections::HashSet;

use simdxml::XmlIndex;
use simdxml::index::TagType;

use super::dom::{COMMENT_TAG, Document, Element, NodeId};

pub fn parse_document(source: &str) -> Result<Document, String> {
    parse_document_opts(source, false)
}

pub fn parse_document_opts(source: &str, keep_comments: bool) -> Result<Document, String> {
    // simdxml::parse would pick parse_two_stage on attribute-heavy XML, but
    // that path (0.2.1) treats `<` inside comments as real tags. The scalar
    // scanner skips comment bodies correctly, so use it unconditionally.
    let index = simdxml::index::structural::parse_scalar(source.as_bytes())
        .map_err(|e| format!("malformed XML: {e}"))?;

    let mut doc = Document::default();
    let tag_count = index.tag_count();
    // Arena id of the element/comment built for each tag index.
    let mut node_of: Vec<Option<NodeId>> = vec![None; tag_count];
    // Byte offsets where CDATA content begins; those text ranges are appended
    // raw (no trim, no entity decoding), matching the quick-xml path.
    let mut cdata_starts: HashSet<u64> = HashSet::new();

    for i in 0..tag_count {
        match index.tag_type(i) {
            TagType::Open | TagType::SelfClose => {
                let attrs = index
                    .attributes(i)
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), XmlIndex::decode_entities(v).into_owned()))
                    .collect();
                let parent = parent_node(&index, &node_of, i);
                let id = doc.push(Element {
                    tag: index.tag_name(i).to_string(),
                    attrs,
                    children: Vec::new(),
                    text: String::new(),
                    parent,
                });
                attach(&mut doc, parent, id);
                node_of[i] = Some(id);
            }
            TagType::Comment if keep_comments => {
                let start = index.tag_starts[i] as usize;
                let end = index.tag_ends[i] as usize;
                // `<!--` inner `-->`: strip 4 leading and 2 trailing bytes of
                // the tag span (tag_ends points at the closing `>`).
                let text = source
                    .get(start + 4..end - 2)
                    .unwrap_or_default()
                    .to_string();
                let parent = parent_node(&index, &node_of, i);
                let id = doc.push(Element {
                    tag: COMMENT_TAG.to_string(),
                    attrs: Vec::new(),
                    children: Vec::new(),
                    text,
                    parent,
                });
                attach(&mut doc, parent, id);
                node_of[i] = Some(id);
            }
            TagType::CData => {
                cdata_starts.insert(index.tag_starts[i] + 9);
            }
            TagType::PI => {
                if index.tag_name(i).eq_ignore_ascii_case("xml") {
                    doc.had_decl = true;
                }
            }
            TagType::Comment | TagType::Close => {}
        }
    }

    // text_ranges are ordered by start offset, so concatenation per element
    // happens in document order — same result as the streaming parser.
    for range in &index.text_ranges {
        if range.parent_tag == u32::MAX {
            continue;
        }
        let Some(id) = node_of[range.parent_tag as usize] else {
            continue;
        };
        let raw = &source[range.start as usize..range.end as usize];
        if cdata_starts.contains(&range.start) {
            doc.node_mut(id).text.push_str(raw);
        } else {
            // quick-xml's trim_text strips ASCII whitespace only; str::trim
            // would also strip U+00A0 etc. and diverge from the quick path.
            let trimmed = raw.trim_matches(|c: char| c.is_ascii_whitespace());
            if !trimmed.is_empty() {
                doc.node_mut(id)
                    .text
                    .push_str(&XmlIndex::decode_entities(trimmed));
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

fn parent_node(index: &XmlIndex, node_of: &[Option<NodeId>], i: usize) -> Option<NodeId> {
    let p = index.parents[i];
    if p == u32::MAX {
        None
    } else {
        node_of[p as usize]
    }
}

fn attach(doc: &mut Document, parent: Option<NodeId>, id: NodeId) {
    match parent {
        Some(p) => doc.node_mut(p).children.push(id),
        None => doc.roots.push(id),
    }
}
