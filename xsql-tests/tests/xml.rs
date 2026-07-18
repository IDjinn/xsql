use xsql::xml::parse::parse_document;
use xsql::xml::serialize::serialize_document;

#[test]
fn round_trip_basic() {
    let src = r#"<?xml version="1.0" encoding="utf-8"?>
<database>
    <arms>
        <ItemSpec id="1" cost="500"/>
        <ItemSpec id="2" cost="1000"/>
    </arms>
</database>
"#;
    let doc = parse_document(src).unwrap();
    assert_eq!(serialize_document(&doc), src);
}

#[test]
fn parses_text_and_escapes() {
    let doc = parse_document(r#"<a note="x &amp; y"><b>1 &lt; 2</b></a>"#).unwrap();
    let root = doc.node(doc.roots[0]);
    assert_eq!(root.attr("note"), Some("x & y"));
    let b = doc.node(root.children[0]);
    assert_eq!(b.text, "1 < 2");
    assert_eq!(
        serialize_document(&doc),
        "<a note=\"x &amp; y\">\n    <b>1 &lt; 2</b>\n</a>\n"
    );
}

#[test]
fn find_group_by_tag_and_attrs() {
    let doc = parse_document(r#"<db><arms/><Group name="goods"><g/></Group></db>"#).unwrap();
    assert!(doc.find_group("arms").is_some());
    assert!(doc.find_group("goods").is_some());
    assert!(doc.find_group("missing").is_none());
}

#[test]
fn detach_and_graft() {
    let mut doc = parse_document(r#"<db><goods><a id="1"/></goods></db>"#).unwrap();
    let goods = doc.find_group("goods").unwrap();
    let old_child = doc.node(goods).children[0];
    doc.detach(old_child);
    assert!(doc.node(goods).children.is_empty());

    let frag = parse_document(r#"<b id="2"/><b id="3"/>"#).unwrap();
    let added = doc.graft(frag, goods);
    assert_eq!(added.len(), 2);
    assert_eq!(doc.node(goods).children.len(), 2);
    assert_eq!(doc.node(doc.node(goods).children[1]).attr("id"), Some("3"));
}

#[test]
fn malformed_xml_is_an_error() {
    assert!(parse_document("<a><b></a>").is_err());
}

#[test]
fn comments_and_doctype_are_skipped() {
    let doc = parse_document("<!DOCTYPE db><db><!-- note --><a/></db>").unwrap();
    assert_eq!(doc.node(doc.roots[0]).children.len(), 1);
}
