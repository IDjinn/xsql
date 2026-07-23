//! Real-world-shaped XML fixtures (RTDPJ protesto title records, no
//! personally identifying data — see fixtures/Valid3.xml, Valid_full.xml,
//! empty.xml). Exercise parsing + basic queries/mutations against documents
//! with realistic depth and repeated tag structure, not hand-crafted for tests.

use xsql::eval::run;
use xsql::parser::parse;

fn fixture_path(name: &str) -> String {
    format!(
        "{}/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR").replace('\\', "/")
    )
}

fn run_on(name: &str, body: &str) -> String {
    let script = parse(&format!("USE {}\n{body}", fixture_path(name))).unwrap();
    run(&script, None).unwrap()
}

const VALID3: &str = "Valid3.xml";
const VALID_FULL: &str = "Valid_full.xml";
const EMPTY: &str = "empty.xml";

#[test]
fn valid3_select_group_estado() {
    let out = run_on(VALID3, "SELECT GROUP estado;");
    assert!(out.contains(r#"uf="ac""#));
}

#[test]
fn valid3_select_tag_titulo_any_depth() {
    let out = run_on(VALID3, "SELECT TAG titulo;");
    assert!(out.contains(r#"id="0000000000""#));
    assert!(out.contains("<especie>DRI</especie>"));
}

#[test]
fn valid3_foreach_tag_mutates_attribute() {
    let out = run_on(
        VALID3,
        "FOREACH d IN TAG devedor\n    WHERE d.sequencia = 1\n    SET d.sequencia = 2\n;",
    );
    assert!(out.contains(r#"sequencia="2""#));
    // Rest of the document is untouched.
    assert!(out.contains("<nome>JOSE WITHOUT MONEY</nome>"));
}

#[test]
fn valid3_aggregate_count_matches_where() {
    let out = run_on(
        VALID3,
        "FOREACH a IN TAG apresentante WHERE a.codigo = 123 OUTPUT COUNT(*);",
    );
    assert_eq!(out.trim(), "1");
}

#[test]
fn valid_full_aggregate_counts_all_apresentantes() {
    let out = run_on(VALID_FULL, "FOREACH a IN TAG apresentante OUTPUT COUNT(*);");
    assert_eq!(out.trim(), "3");
}

#[test]
fn valid_full_where_filters_by_codigo() {
    let out = run_on(
        VALID_FULL,
        "FOREACH a IN TAG apresentante WHERE a.codigo = 341 OUTPUT COUNT(*);",
    );
    assert_eq!(out.trim(), "1");
}

#[test]
fn valid_full_update_sugar_mutates_one_match() {
    let out = run_on(
        VALID_FULL,
        r#"UPDATE TAG apresentante SET codigo = 999 WHERE codigo = 341 LIMIT 1;"#,
    );
    assert!(out.contains(r#"codigo="999""#));
    // Other apresentante elements are untouched.
    assert!(out.contains(r#"codigo="237""#));
    assert!(out.contains(r#"codigo="033""#));
}

#[test]
fn empty_document_has_no_root_to_select() {
    // Just an XML declaration, no root element: parses fine, SELECT ROOT
    // simply matches nothing.
    let out = run_on(EMPTY, "SELECT ROOT;");
    assert_eq!(out, "");
}
