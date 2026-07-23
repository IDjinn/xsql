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

#[test]
fn nested_foreach_iterates_children_of_outer_element() {
    let xml = r#"<db><offices><Office id="1"><Staff id="a" cost="1"/><Staff id="b" cost="2"/></Office><Office id="2"><Staff id="c" cost="3"/></Office></offices></db>"#;
    let script = parse(
        "USE INPUT\nFOREACH office IN offices\n    WHERE office.id = 1\n    FOREACH s IN office\n        SET s.cost = s.cost * 10\n;",
    )
    .unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    assert!(out.contains(r#"<Staff id="a" cost="10"/>"#), "{out}");
    assert!(out.contains(r#"<Staff id="b" cost="20"/>"#), "{out}");
    // The other office's children are untouched.
    assert!(out.contains(r#"<Staff id="c" cost="3"/>"#), "{out}");
}

#[test]
fn nested_foreach_reads_and_writes_outer_scope() {
    let xml = r#"<db><offices><Office id="1" total="0"><Staff cost="1"/><Staff cost="2"/></Office><Office id="2" total="0"><Staff cost="5"/></Office></offices></db>"#;
    let script = parse(
        "USE INPUT\nFOREACH office IN offices\n    FOREACH s IN office\n        SET office.total = office.total + s.cost\n        SET s.from = office.id\n;",
    )
    .unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    // Inner iterations accumulate into the outer element's attribute.
    assert!(out.contains(r#"<Office id="1" total="3">"#), "{out}");
    assert!(out.contains(r#"<Office id="2" total="5">"#), "{out}");
    // Inner elements can read outer attributes.
    assert!(out.contains(r#"<Staff cost="1" from="1"/>"#), "{out}");
    assert!(out.contains(r#"<Staff cost="5" from="2"/>"#), "{out}");
}

#[test]
fn nested_foreach_over_subgroup_inside_current_element() {
    let xml = r#"<db><offices><Office id="1"><staff><P id="x" c="1"/></staff></Office></offices></db>"#;
    let script = parse(
        "USE INPUT\nFOREACH office IN offices\n    FOREACH p IN staff\n        SET p.c = 9\n;",
    )
    .unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    assert!(out.contains(r#"<P id="x" c="9"/>"#), "{out}");
}

#[test]
fn nested_break_only_stops_inner_loop() {
    let xml = r#"<db><offices><Office id="1"><S n="1"/><S n="2"/></Office><Office id="2"><S n="3"/><S n="4"/></Office></offices></db>"#;
    let script = parse(
        "USE INPUT\nFOREACH office IN offices\n    FOREACH s IN office\n        SET s.hit = 1\n        BREAK;\n;",
    )
    .unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    // First child of EACH office got hit: the outer loop kept going.
    assert!(out.contains(r#"<S n="1" hit="1"/>"#), "{out}");
    assert!(out.contains(r#"<S n="2"/>"#), "{out}");
    assert!(out.contains(r#"<S n="3" hit="1"/>"#), "{out}");
    assert!(out.contains(r#"<S n="4"/>"#), "{out}");
}

#[test]
fn nested_foreach_unknown_source_errors() {
    let xml = r#"<db><offices><Office id="1"/></offices></db>"#;
    let script =
        parse("USE INPUT\nFOREACH office IN offices\n    FOREACH x IN nope\n        SET x.a = 1\n;")
            .unwrap();
    let err = run(&script, Some(xml.to_string())).unwrap_err();
    assert!(err.message.contains("cannot iterate `nope`"), "{}", err.message);
}

#[test]
fn merge_into_updates_cited_attrs_and_inserts_new() {
    let out = run_on_fixture(
        "MERGE INTO GROUP goods RAW XML `<ItemSpec id=\"52034301\" cost=\"777\" extra=\"1\"/><ItemSpec id=\"999\" cost=\"5\"/>`;",
    );
    // Matched by id: cited attrs updated, the rest (level) preserved.
    assert!(out.contains(r#"<ItemSpec id="52034301" level="1" cost="777" extra="1"/>"#), "{out}");
    // No match: inserted.
    assert!(out.contains(r#"<ItemSpec id="999" cost="5"/>"#), "{out}");
    // Untouched sibling survives.
    assert!(out.contains(r#"id="52034302""#), "{out}");
}

#[test]
fn merge_into_is_idempotent() {
    // Fragment matches the existing element exactly: nothing changes, so the
    // document is not re-emitted.
    let out = run_on_fixture(
        "MERGE INTO GROUP goods RAW XML `<ItemSpec id=\"52034301\" level=\"1\" cost=\"500\"/>`;",
    );
    assert_eq!(out, "", "{out}");
}

#[test]
fn merge_into_matches_by_name_then_tag() {
    let xml = r#"<db><cfg><opt name="speed" v="1"/><misc v="2"/></cfg></db>"#;
    let script = parse(
        "USE INPUT\nMERGE INTO GROUP cfg RAW XML `<opt name=\"speed\" v=\"9\"/><misc v=\"3\"/><opt name=\"new\" v=\"0\"/>`;",
    )
    .unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    assert!(out.contains(r#"<opt name="speed" v="9"/>"#), "{out}");
    assert!(out.contains(r#"<misc v="3"/>"#), "{out}");
    assert!(out.contains(r#"<opt name="new" v="0"/>"#), "{out}");
}

#[test]
fn merge_attr_only_writes_missing() {
    // 101/102 already have science (kept, even though the value differs);
    // 103 lacks it and gets the merged value.
    let out = run_on_fixture("FOREACH arm IN arms MERGE arm.science = 99;");
    assert!(out.contains(r#"id="101" cost="500" science="1""#), "{out}");
    assert!(out.contains(r#"id="102" cost="900" science="3""#), "{out}");
    assert!(out.contains(r#"id="103" cost="1200" unlock_civi_science="5" science="99""#), "{out}");
}

#[test]
fn where_required_errors_when_attr_missing() {
    // 103 has no science attribute: REQUIRED makes that a hard error
    // (plain WHERE would just treat it as null and skip the element).
    let script = parse(&format!(
        "USE {}\nSELECT GROUP arms FOREACH arm IN arms WHERE REQUIRED arm.science > 0;",
        fixture_path()
    ))
    .unwrap();
    let err = run(&script, None).unwrap_err();
    assert!(err.message.contains("`science` is REQUIRED"), "{}", err.message);
    assert!(err.message.contains("<ItemSpec>"), "{}", err.message);
}

#[test]
fn where_required_filters_normally_when_attr_present() {
    // Every good has `level`, so REQUIRED behaves like a plain WHERE.
    let out = run_on_fixture(
        "SELECT GROUP goods FOREACH good IN goods WHERE REQUIRED good.level >= 2;",
    );
    assert!(!out.contains(r#"id="52034301""#), "{out}");
    assert!(out.contains(r#"id="52034302""#), "{out}");
    assert!(out.contains(r#"id="52034307""#), "{out}");
}

#[test]
fn attribute_ops_apply_in_script_order() {
    // DELETE then SET must leave the attribute present (script order), not
    // batch all SETs before all DELETEs.
    let out = run_on_fixture(
        "FOREACH good IN goods WHERE good.id = 52034301 DELETE good.cost SET good.cost = 42;",
    );
    assert!(out.contains(r#"<ItemSpec id="52034301" level="1" cost="42"/>"#), "{out}");
}

#[test]
fn output_projects_cited_attrs_only() {
    let out = run_on_fixture(
        "SELECT GROUP goods\nFOREACH good IN goods\n    WHERE good.id > 52034301\n    OUTPUT good.id, good.cost\n;",
    );
    assert!(out.contains(r#"<ItemSpec id="52034302" cost="1000"/>"#), "{out}");
    assert!(out.contains(r#"<ItemSpec id="52034307" cost="4000"/>"#), "{out}");
    // Uncited attribute and unmatched element are absent; query only.
    assert!(!out.contains("level="), "{out}");
    assert!(!out.contains(r#"id="52034301""#), "{out}");
    assert!(!out.contains("<database>"), "{out}");
}

#[test]
fn output_star_matches_select_default() {
    let plain = run_on_fixture("SELECT GROUP goods FOREACH good IN goods WHERE good.id > 52034301;");
    let star = run_on_fixture(
        "SELECT GROUP goods FOREACH good IN goods WHERE good.id > 52034301 OUTPUT *;",
    );
    assert_eq!(plain, star);
}

#[test]
fn output_without_foreach_loops_implicitly() {
    let out = run_on_fixture("SELECT GROUP goods OUTPUT id, level;");
    assert!(out.contains(r#"<ItemSpec id="52034301" level="1"/>"#), "{out}");
    assert!(out.contains(r#"<ItemSpec id="52034302" level="2"/>"#), "{out}");
    assert!(out.contains(r#"<ItemSpec id="52034307" level="7"/>"#), "{out}");
}

#[test]
fn output_expression_with_alias() {
    let out = run_on_fixture(
        "SELECT GROUP goods\nFOREACH good IN goods\n    WHERE good.id = 52034301\n    OUTPUT good.id, good.cost * 2 AS double_cost\n;",
    );
    assert!(out.contains(r#"<ItemSpec id="52034301" double_cost="1000"/>"#), "{out}");
}

#[test]
fn output_omits_missing_attrs() {
    // 103 has no science attribute: the projection just leaves it out.
    let out = run_on_fixture("SELECT GROUP arms OUTPUT id, science;");
    assert!(out.contains(r#"<ItemSpec id="101" science="1"/>"#), "{out}");
    assert!(out.contains(r#"<ItemSpec id="103"/>"#), "{out}");
}

#[test]
fn output_sees_prior_sets_not_later_ones() {
    let out = run_on_fixture(
        "FOREACH good IN goods\n    WHERE good.id = 52034301\n    SET good.cost = 111\n    OUTPUT good.id, good.cost\n    SET good.cost = 222\n;",
    );
    // Emission snapshots the overlay at reach time.
    assert!(out.contains(r#"<ItemSpec id="52034301" cost="111"/>"#), "{out}");
    // The document itself ends up with the later SET.
    assert!(out.contains(r#"<ItemSpec id="52034301" level="1" cost="222"/>"#), "{out}");
}

#[test]
fn output_in_nested_loop_joins_scopes() {
    let xml = r#"<db><offices><Office id="1"><S name="a" cost="9"/><S name="b" cost="1"/></Office><Office id="2"><S name="c" cost="7"/></Office></offices></db>"#;
    let script = parse(
        "USE INPUT\nSELECT GROUP offices\nFOREACH office IN offices\n    FOREACH s IN office\n        WHERE s.cost > 5\n        OUTPUT office.id AS office, s.name, s.cost\n;",
    )
    .unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    assert!(out.contains(r#"<S office="1" name="a" cost="9"/>"#), "{out}");
    assert!(out.contains(r#"<S office="2" name="c" cost="7"/>"#), "{out}");
    assert!(!out.contains(r#"name="b""#), "{out}");
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

// ---------------------------------------------------------------------------
// MySQL-style shorthands (UPDATE / MERGE ... SET / DELETE FROM)
// ---------------------------------------------------------------------------

#[test]
fn update_edits_matching_element() {
    let out = run_on_fixture(
        "UPDATE office SET name = \"New Office Name\" WHERE office.id = 216000 LIMIT 1;",
    );
    assert!(out.contains(r#"name="New Office Name""#), "{out}");
    assert!(out.contains(r#"name="Other Office""#), "{out}");
}

#[test]
fn update_without_where_edits_all() {
    let out = run_on_fixture("UPDATE goods SET level = 9;");
    assert_eq!(out.matches(r#"level="9""#).count(), 3, "{out}");
}

#[test]
fn update_multiple_assignments_and_expressions() {
    let out = run_on_fixture("UPDATE goods SET cost = cost * 2, tag2 = \"x\" WHERE id = 52034301;");
    assert!(out.contains(r#"id="52034301" level="1" cost="1000" tag2="x""#), "{out}");
    assert!(out.contains(r#"id="52034302" level="2" cost="1000"/>"#), "{out}");
}

#[test]
fn merge_shorthand_writes_only_missing() {
    // 101 and 102 already carry science; only 103 gets the merged value.
    let out = run_on_fixture("MERGE arms SET science = 99;");
    assert!(out.contains(r#"id="101" cost="500" science="1""#), "{out}");
    assert!(out.contains(r#"id="103" cost="1200" unlock_civi_science="5" science="99""#), "{out}");
}

#[test]
fn delete_from_removes_matching_elements() {
    let out = run_on_fixture("DELETE FROM goods WHERE cost > 500;");
    assert!(out.contains(r#"id="52034301""#), "{out}");
    assert!(!out.contains(r#"id="52034302""#), "{out}");
    assert!(!out.contains(r#"id="52034307""#), "{out}");
    // The container survives.
    assert!(out.contains("<goods>"), "{out}");
}

#[test]
fn delete_from_limit_one_stops_after_first_match() {
    let out = run_on_fixture("DELETE FROM goods WHERE cost >= 1000 LIMIT 1;");
    assert!(!out.contains(r#"id="52034302""#), "{out}");
    assert!(out.contains(r#"id="52034307""#), "{out}");
}

// ---------------------------------------------------------------------------
// TAG selectors
// ---------------------------------------------------------------------------

#[test]
fn select_tag_prints_all_matches_across_groups() {
    let out = run_on_fixture("SELECT TAG ItemSpec;");
    // arms(3) + goods(3) + new_continued_cost(1), no container tags.
    assert_eq!(out.matches("<ItemSpec").count(), 7, "{out}");
    assert!(!out.contains("<arms"), "{out}");
    assert!(!out.contains("<OfficeSpec"), "{out}");
}

#[test]
fn foreach_in_tag_iterates_document_wide() {
    let out = run_on_fixture("FOREACH i IN TAG ItemSpec SET i.seen = 1;");
    assert_eq!(out.matches(r#"seen="1""#).count(), 7, "{out}");
    assert!(!out.contains(r#"<OfficeSpec id="216000" name="Old Office" seen"#), "{out}");
}

#[test]
fn update_tag_with_where() {
    let out = run_on_fixture("UPDATE TAG ItemSpec SET cheap = 1 WHERE cost < 600;");
    // cost 500 in arms, cost 500 in goods, cost 1 in new_continued_cost.
    assert_eq!(out.matches(r#"cheap="1""#).count(), 3, "{out}");
}

#[test]
fn delete_tag_removes_every_match() {
    let out = run_on_fixture("DELETE TAG ItemSpec;");
    assert!(!out.contains("<ItemSpec"), "{out}");
    assert!(out.contains("<arms/>"), "{out}");
    assert!(out.contains("<OfficeSpec"), "{out}");
}

#[test]
fn delete_missing_tag_errors_without_ignore() {
    let script = parse(&format!("USE {}\nDELETE TAG Nope;", fixture_path())).unwrap();
    let err = run(&script, None).unwrap_err();
    assert!(err.message.contains("no element with tag `Nope`"), "{}", err.message);
    // With IGNORE it is a no-op.
    let out = run_on_fixture("DELETE IGNORE TAG Nope;");
    assert_eq!(out, "");
}

#[test]
fn select_missing_tag_errors() {
    let script = parse(&format!("USE {}\nSELECT TAG Nope;", fixture_path())).unwrap();
    let err = run(&script, None).unwrap_err();
    assert!(err.message.contains("no element with tag `Nope`"), "{}", err.message);
}

#[test]
fn nested_foreach_in_tag_stays_within_subtree() {
    let xml = r#"<db><a><wrap><Item v="1"/></wrap><Item v="2"/></a><b><Item v="3"/></b></db>"#;
    let script = parse(
        "USE INPUT\nFOREACH outer IN TAG a\n    FOREACH i IN TAG Item\n        SET i.hit = 1\n;",
    )
    .unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    // Both Items under <a> (nested at different depths) are hit; <b>'s is not.
    assert_eq!(out.matches(r#"hit="1""#).count(), 2, "{out}");
    assert!(out.contains(r#"<Item v="3"/>"#), "{out}");
}

#[test]
fn select_tag_with_output_projection() {
    let out = run_on_fixture("SELECT TAG OfficeSpec OUTPUT id, name;");
    assert!(out.contains(r#"<OfficeSpec id="216000" name="Old Office"/>"#), "{out}");
    assert!(out.contains(r#"<OfficeSpec id="216001" name="Other Office"/>"#), "{out}");
}

/// Nested tag matches (an element whose tag also appears in its descendants)
/// must keep sequential semantics: the outer element's write is visible when
/// the inner element is planned... and the loop stays correct either way.
#[test]
fn foreach_tag_handles_nested_matches() {
    let xml = r#"<db><Item id="1"><Item id="2"/></Item></db>"#;
    let script =
        parse("USE INPUT\nFOREACH i IN TAG Item SET i.n = i.id + 10;").unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    assert!(out.contains(r#"<Item id="1" n="11">"#), "{out}");
    assert!(out.contains(r#"<Item id="2" n="12"/>"#), "{out}");
}

// ---------------------------------------------------------------------------
// ROOT
// ---------------------------------------------------------------------------

#[test]
fn foreach_in_root_iterates_every_element_regardless_of_tag() {
    let xml = r#"<db>
        <a><ItemSpec id="1" type="0"/></a>
        <b><ItemSpec id="2" type="1"/></b>
        <c><ItemSpec id="3" type="0"/></c>
    </db>"#;
    let script = parse("USE INPUT\nFOREACH v IN ROOT WHERE v.type = 0 OUTPUT v.id;").unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    assert_eq!(out, "<ItemSpec id=\"1\"/>\n<ItemSpec id=\"3\"/>\n");
}

#[test]
fn select_root_prints_every_element() {
    let xml = r#"<db><a><Item id="1"/></a></db>"#;
    let script = parse("USE INPUT\nSELECT ROOT OUTPUT id;").unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    // db, a and Item all show up (db/a have no `id`, so their tag prints bare).
    assert!(out.contains("<db/>"), "{out}");
    assert!(out.contains("<a/>"), "{out}");
    assert!(out.contains(r#"<Item id="1"/>"#), "{out}");
}

#[test]
fn nested_foreach_in_root_stays_within_subtree() {
    let xml = r#"<db><a><wrap><Item v="1"/></wrap><Item v="2"/></a><b><Item v="3"/></b></db>"#;
    let script = parse(
        "USE INPUT\nFOREACH outer IN TAG a\n    FOREACH i IN ROOT\n        SET i.hit = 1\n;",
    )
    .unwrap();
    let out = run(&script, Some(xml.to_string())).unwrap();
    // Every element under <a> is hit (wrap and both Items), nothing under <b>.
    assert_eq!(out.matches(r#"hit="1""#).count(), 3, "{out}");
    assert!(out.contains(r#"<Item v="3"/>"#), "{out}");
}

// ---------------------------------------------------------------------------
// Aggregate OUTPUT functions
// ---------------------------------------------------------------------------

fn aggregate_fixture() -> &'static str {
    r#"<db>
        <ItemSpec id="1" type="0" cost="10"/>
        <ItemSpec id="2" type="1" cost="20"/>
        <ItemSpec id="3" type="0" cost="30"/>
    </db>"#
}

#[test]
fn output_count_returns_plain_number() {
    let script = parse(
        "USE INPUT\nFOREACH v IN TAG ItemSpec WHERE v.type = 0 OUTPUT COUNT(v.id);",
    )
    .unwrap();
    let out = run(&script, Some(aggregate_fixture().to_string())).unwrap();
    assert_eq!(out, "2\n");
}

#[test]
fn output_min_max_sum_avg_in_one_row() {
    let script = parse(
        "USE INPUT\nFOREACH v IN TAG ItemSpec WHERE v.type = 0 \
         OUTPUT MIN(v.cost), MAX(v.cost), SUM(v.cost), AVG(v.cost);",
    )
    .unwrap();
    let out = run(&script, Some(aggregate_fixture().to_string())).unwrap();
    assert_eq!(out, "10,30,40,20\n");
}

#[test]
fn output_count_with_zero_matches_is_zero() {
    let script = parse(
        "USE INPUT\nFOREACH v IN TAG ItemSpec WHERE v.type = 99 OUTPUT COUNT(v.id);",
    )
    .unwrap();
    let out = run(&script, Some(aggregate_fixture().to_string())).unwrap();
    assert_eq!(out, "0\n");
}

#[test]
fn output_cannot_mix_aggregate_and_plain_items() {
    let script = parse("USE INPUT\nFOREACH v IN TAG ItemSpec OUTPUT v.id, COUNT(v.id);").unwrap();
    let err = run(&script, Some(aggregate_fixture().to_string())).unwrap_err();
    assert!(err.message.contains("cannot mix aggregate functions"), "{}", err.message);
}

#[test]
fn output_unknown_aggregate_function_errors() {
    let script = parse("USE INPUT\nFOREACH v IN TAG ItemSpec OUTPUT NOPE(v.id);").unwrap();
    let err = run(&script, Some(aggregate_fixture().to_string())).unwrap_err();
    assert!(err.message.contains("unknown aggregate function `NOPE`"), "{}", err.message);
}
