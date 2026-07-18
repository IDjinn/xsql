use xsql::eval::{Session, run, run_with_report};
use xsql::parser::{parse, parse_session};

fn fixture_path() -> String {
    let path = format!(
        "{}/fixtures/database.local.xml",
        env!("CARGO_MANIFEST_DIR").replace('\\', "/")
    );
    path
}

fn run_script(script: &str) -> String {
    let script = parse(script).unwrap();
    run(&script, None).unwrap()
}

fn run_on_fixture(body: &str) -> String {
    run_script(&format!("USE {}\n{body}", fixture_path()))
}

#[test]
fn select_group_prints_it() {
    let out = run_on_fixture("SELECT GROUP arms;");
    assert!(out.starts_with("<arms>"));
    assert!(out.contains(r#"<ItemSpec id="101""#));
    assert!(out.contains(r#"<ItemSpec id="103""#));
    // Query only: the document itself is not re-emitted.
    assert!(!out.contains("<database>"));
}

#[test]
fn delete_attrs_with_ignore() {
    let out = run_on_fixture(
        "FOREACH arm IN arms\n    DELETE IGNORE arm.unlock_civi_science\n    DELETE IGNORE arm.science\n;",
    );
    // Mutated document is printed in full.
    assert!(out.contains("<database>"));
    assert!(!out.contains("unlock_civi_science"));
    assert!(!out.contains("science"));
    assert!(out.contains(r#"<ItemSpec id="101" cost="500"/>"#));
}

#[test]
fn delete_attr_without_ignore_errors_on_missing() {
    let script = parse(&format!(
        "USE {}\nFOREACH arm IN arms DELETE arm.science;",
        fixture_path()
    ))
    .unwrap();
    let err = run(&script, None).unwrap_err();
    assert!(err.message.contains("`science` not found"), "{}", err.message);
    assert!(err.message.contains("DELETE IGNORE"));
}

#[test]
fn where_filters_select() {
    let out = run_on_fixture("SELECT GROUP goods FOREACH good IN goods WHERE goods.id > 52034301;");
    assert!(!out.contains(r#"id="52034301""#));
    assert!(out.contains(r#"id="52034302""#));
    assert!(out.contains(r#"id="52034307""#));
}

#[test]
fn set_with_break_edits_only_first_match() {
    let out = run_on_fixture(
        "SELECT GROUP office\nFOREACH office IN office\n    WHERE office.id = 216000\n    SET office.name = \"New Office Name\"\n    BREAK;\n;",
    );
    assert!(out.contains(r#"name="New Office Name""#));
    assert!(out.contains(r#"name="Other Office""#));
    // Selected element printed once, then the whole modified document.
    assert_eq!(out.matches("New Office Name").count(), 2);
}

#[test]
fn replace_group_swaps_children() {
    let out = run_on_fixture(
        "REPLACE GROUP new_continued_cost\nRAW XML `\n<ItemSpec id=\"9001\" level=\"1\" cost=\"500\"/>\n<ItemSpec id=\"9002\" level=\"2\" cost=\"1000\"/>\n`;",
    );
    assert!(!out.contains(r#"<ItemSpec id="1" level="1" cost="1"/>"#));
    assert!(out.contains(r#"id="9001""#));
    assert!(out.contains(r#"id="9002""#));
}

#[test]
fn insert_into_group_appends() {
    let out = run_on_fixture("INSERT INTO GROUP goods RAW XML `<ItemSpec id=\"999\"/>`;");
    assert!(out.contains(r#"id="52034307""#));
    assert!(out.contains(r#"id="999""#));
}

#[test]
fn delete_group_removes_subtree() {
    let out = run_on_fixture("DELETE GROUP new_continued_cost;");
    assert!(!out.contains("new_continued_cost"));
    assert!(out.contains("<arms>"));
}

#[test]
fn delete_element_via_foreach() {
    let out = run_on_fixture("FOREACH good IN goods WHERE good.id = 52034302 DELETE good;");
    assert!(!out.contains(r#"id="52034302""#));
    assert!(out.contains(r#"id="52034301""#));
}

#[test]
fn arithmetic_set() {
    let out = run_on_fixture("FOREACH good IN goods SET good.cost = good.cost * 2 + 1;");
    assert!(out.contains(r#"cost="1001""#));
    assert!(out.contains(r#"cost="2001""#));
    assert!(out.contains(r#"cost="8001""#));
}

#[test]
fn numeric_group_matched_by_id_attribute() {
    let script = parse(
        "USE INPUT\nSELECT GROUP 110000\nFOREACH item IN 110000 WHERE item.id < 120001;",
    )
    .unwrap();
    let xml = r#"<db><Group id="110000"><ItemSpec id="120000"/><ItemSpec id="120001"/></Group></db>"#;
    let out = run(&script, Some(xml.to_string())).unwrap();
    assert!(out.contains(r#"id="120000""#));
    assert!(!out.contains(r#"id="120001""#));
}

#[test]
fn use_input_reads_stdin_document() {
    let script = parse("USE INPUT SELECT GROUP arms;").unwrap();
    let xml = r#"<db><arms><ItemSpec id="7"/></arms></db>"#;
    let out = run(&script, Some(xml.to_string())).unwrap();
    assert!(out.contains(r#"<ItemSpec id="7"/>"#));
}

#[test]
fn use_input_without_stdin_is_an_error() {
    let script = parse("USE INPUT SELECT GROUP arms;").unwrap();
    let err = run(&script, None).unwrap_err();
    assert!(err.message.contains("USE INPUT"), "{}", err.message);
}

#[test]
fn missing_group_reports_source() {
    let script = parse(&format!("USE {} SELECT GROUP nope;", fixture_path())).unwrap();
    let err = run(&script, None).unwrap_err();
    assert!(err.message.contains("`nope` not found"), "{}", err.message);
}

#[test]
fn format_off_serializes_compact() {
    let xml = r#"<db><arms><ItemSpec id="1" cost="5"/><ItemSpec id="2" cost="6"/></arms></db>"#;
    let script = parse("SET FORMAT = OFF;\nUSE INPUT SELECT GROUP arms;\nFOREACH a IN arms SET a.cost = 9;").unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    // SELECT subtree and the modified document each occupy one line: no indentation.
    assert!(!out.contains("    <"), "{out}");
    assert!(out.contains(r#"<arms><ItemSpec id="1" cost="5"/><ItemSpec id="2" cost="6"/></arms>"#));
    assert!(out.contains(r#"<db><arms><ItemSpec id="1" cost="9"/><ItemSpec id="2" cost="9"/></arms></db>"#));
}

#[test]
fn comments_dropped_by_default() {
    let xml = r#"<db><arms><!-- keep me --><ItemSpec id="1"/></arms></db>"#;
    let script = parse("USE INPUT FOREACH a IN arms SET a.cost = 1;").unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    assert!(!out.contains("keep me"));
}

#[test]
fn ignore_comments_off_preserves_comments() {
    let xml = r#"<db><arms><!-- keep me --><ItemSpec id="1"/></arms></db>"#;
    let script = parse(
        "SET IGNORE_COMMENTS = OFF;\nUSE INPUT FOREACH a IN arms SET a.cost = 1;",
    )
    .unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    assert!(out.contains("<!-- keep me -->"), "{out}");
    // The comment is not a loop element: only the real child got the attribute.
    assert_eq!(out.matches(r#"cost="1""#).count(), 1, "{out}");
}

#[test]
fn analyze_returns_timing_report() {
    let xml = r#"<db><arms><ItemSpec id="1"/></arms></db>"#;
    let script = parse("ANALYZE;\nUSE INPUT SELECT GROUP arms;").unwrap();
    let (out, report) = run_with_report(&script, Some(xml.to_string())).unwrap();
    assert!(out.contains(r#"<ItemSpec id="1"/>"#));
    let mut report = report.expect("ANALYZE should produce a report");
    report.prepend(vec![("lex".to_string(), std::time::Duration::from_micros(42))]);
    let rendered = report.render(std::time::Duration::from_micros(999));
    assert!(rendered.contains("-- ANALYZE"), "{rendered}");
    assert!(rendered.contains("lex"), "{rendered}");
    assert!(rendered.contains("parse xml  <stdin>"), "{rendered}");
    assert!(rendered.contains("block #1   SELECT   <stdin>"), "{rendered}");
    assert!(rendered.contains("serialize"), "{rendered}");
    assert!(rendered.contains("assemble output"), "{rendered}");
    assert!(rendered.contains("memory (documents)"), "{rendered}");
    assert!(rendered.contains("total"), "{rendered}");
    assert!(report.memory_bytes.unwrap() > 0);
}

#[test]
fn analyze_report_times_file_read_separately() {
    let script = parse(&format!("ANALYZE;\nUSE {} SELECT GROUP arms;", fixture_path())).unwrap();
    let (_, report) = run_with_report(&script, None).unwrap();
    let rendered = report.unwrap().render(std::time::Duration::ZERO);
    assert!(rendered.contains("read       "), "{rendered}");
    assert!(rendered.contains("parse xml  "), "{rendered}");
}

#[test]
fn no_analyze_means_no_report() {
    let xml = r#"<db><arms><ItemSpec id="1"/></arms></db>"#;
    let script = parse("USE INPUT SELECT GROUP arms;").unwrap();
    let (_, report) = run_with_report(&script, Some(xml.to_string())).unwrap();
    assert!(report.is_none());
}

/// REPL-style session: sticky USE and in-memory mutations persist across
/// separately submitted statements; modified docs serialize on demand.
#[test]
fn session_state_persists_across_execs() {
    let xml = r#"<db><arms><ItemSpec id="1" cost="5"/></arms></db>"#;
    let mut session = Session::new(Some(xml.to_string()));

    let (s1, current) = parse_session("USE INPUT FOREACH a IN arms SET a.cost = 99;", None).unwrap();
    assert_eq!(session.exec(&s1).unwrap(), "");

    // No USE here: inherits the sticky source from the previous statement.
    let (s2, _) = parse_session("SELECT GROUP arms;", current).unwrap();
    let out = session.exec(&s2).unwrap();
    assert!(out.contains(r#"cost="99""#), "{out}");

    assert!(session.has_modifications());
    assert!(session.dump_modified().contains(r#"cost="99""#));
}

#[test]
fn session_without_stdin_rejects_use_input() {
    let mut session = Session::new(None);
    let (script, _) = parse_session("USE INPUT SELECT GROUP arms;", None).unwrap();
    let err = session.exec(&script).unwrap_err();
    assert!(err.message.contains("USE INPUT"), "{}", err.message);
}

/// Large group exercises the parallel evaluate-then-apply FOREACH path
/// (threshold is 1024 children); result must match sequential semantics.
#[test]
fn parallel_foreach_matches_sequential_semantics() {
    let n = 100_000;
    let mut xml = String::from("<db><big>");
    for i in 0..n {
        xml.push_str(&format!(r#"<Item id="{i}" cost="{}"/>"#, i * 10));
    }
    xml.push_str("</big></db>");

    let script = parse(
        "USE INPUT\nFOREACH item IN big\n    WHERE item.id >= 50000\n    SET item.cost = item.cost + 5\n    DELETE IGNORE item.tmp\n;",
    )
    .unwrap();
    let out = run(&script, Some(xml)).unwrap();

    // Untouched below the threshold, shifted above it.
    assert!(out.contains(r#"<Item id="49999" cost="499990"/>"#));
    assert!(out.contains(r#"<Item id="50000" cost="500005"/>"#));
    assert!(out.contains(&format!(r#"<Item id="{}" cost="{}"/>"#, n - 1, (n - 1) * 10 + 5)));
}
