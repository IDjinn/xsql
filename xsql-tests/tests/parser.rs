use xsql::ast::{Expr, LoopSource, Op, Selector, Setting, Settings, Source, Verb};
use xsql::parser::parse;

/// The four scripts from the original scratch file must parse verbatim.
#[test]
fn scratch_file_parses_verbatim() {
    let script = parse(
        r#"; remove some attributes from all values inside a group
USE database.local.xml
SELECT GROUP arms
FOREACH arm IN arms
    DELETE IGNORE arm.unlock_civi_science
    DELETE IGNORE arm.science
;



; Select xml, delete old values from a group and replace with new values
USE database.local.xml
REPLACE GROUP new_continued_cost
RAW XML `
<ItemSpec id="52034301" level="1" cost="500" rewardMin="0" rewardMax="500" type2="1" item="239301"/>
<ItemSpec id="52034302" level="2" cost="1000" rewardMin="501" rewardMax="1000" type2="1" item="239302"/>
`;

USE database.local.xml
SELECT GROUP goods
FOREACH good IN goods
    WHERE goods.id > 52034301
;

USE database.local.xml
SELECT GROUP office
FOREACH office IN office
    WHERE office.id = 216000
    SET office.name = "New Office Name"
    BREAK;
;
"#,
    )
    .unwrap();

    assert_eq!(script.blocks.len(), 4);

    let Verb::Select { target, foreach, .. } = &script.blocks[0].verb else {
        panic!("block 0 should be SELECT");
    };
    assert_eq!(target, &Selector::Group("arms".into()));
    let ops = &foreach.as_ref().unwrap().ops;
    assert_eq!(ops.len(), 2);
    assert!(matches!(&ops[0], Op::DeleteAttr { attr, ignore: true, .. } if attr == "unlock_civi_science"));

    let Verb::ReplaceGroup { group, xml, .. } = &script.blocks[1].verb else {
        panic!("block 1 should be REPLACE GROUP");
    };
    assert_eq!(group, "new_continued_cost");
    assert!(xml.contains(r#"id="52034301""#));

    let Verb::Select { foreach: Some(f), .. } = &script.blocks[2].verb else {
        panic!("block 2 should be SELECT FOREACH");
    };
    assert!(matches!(&f.ops[0], Op::Where(_)));

    let Verb::Select { foreach: Some(f), .. } = &script.blocks[3].verb else {
        panic!("block 3 should be SELECT FOREACH");
    };
    assert!(matches!(&f.ops[0], Op::Where(_)));
    assert!(matches!(&f.ops[1], Op::Set { attr, .. } if attr == "name"));
    assert!(matches!(&f.ops[2], Op::Break));
}

#[test]
fn sticky_use_applies_to_following_blocks() {
    let script = parse(
        "USE db.xml;\nSELECT GROUP arms;\nFOREACH a IN arms DELETE IGNORE a.x;\nUSE INPUT\nSELECT GROUP goods;",
    )
    .unwrap();
    assert_eq!(script.blocks.len(), 3);
    assert_eq!(script.blocks[0].source, Source::File("db.xml".into()));
    assert_eq!(script.blocks[1].source, Source::File("db.xml".into()));
    assert_eq!(script.blocks[2].source, Source::Input);
}

#[test]
fn missing_use_is_an_error() {
    let err = parse("SELECT GROUP arms;").unwrap_err();
    assert!(err.message.contains("no document in scope"), "{}", err.message);
}

#[test]
fn insert_and_delete_group() {
    let script = parse(
        "USE db.xml\nINSERT INTO GROUP goods RAW XML `<a/>`;\nDELETE IGNORE GROUP legacy;",
    )
    .unwrap();
    assert!(matches!(&script.blocks[0].verb, Verb::InsertInto { group, .. } if group == "goods"));
    assert!(
        matches!(&script.blocks[1].verb, Verb::DeleteGroup { group, ignore: true } if group == "legacy")
    );
}

#[test]
fn merge_into_group_parses() {
    let script =
        parse("USE db.xml\nMERGE INTO GROUP goods RAW XML `<a id=\"1\"/>`;").unwrap();
    assert!(
        matches!(&script.blocks[0].verb, Verb::MergeInto { group, xml, .. } if group == "goods" && xml.contains("<a"))
    );
}

#[test]
fn nested_foreach_and_merge_attr_parse() {
    let script = parse(
        "USE db.xml\nFOREACH office IN offices\n    WHERE office.id = 1\n    FOREACH s IN office\n        MERGE s.tier = 1\n;",
    )
    .unwrap();
    let Verb::Foreach(outer) = &script.blocks[0].verb else { panic!() };
    assert!(matches!(&outer.ops[0], Op::Where(_)));
    let Op::Foreach(inner) = &outer.ops[1] else { panic!("expected nested FOREACH") };
    assert_eq!(inner.var, "s");
    assert_eq!(inner.source, LoopSource::Name("office".into()));
    assert!(matches!(&inner.ops[0], Op::Merge { var, attr, .. } if var == "s" && attr == "tier"));
}

#[test]
fn where_required_parses_attr_and_condition() {
    let script =
        parse("USE db.xml FOREACH a IN g WHERE REQUIRED a.cost > 100;").unwrap();
    let Verb::Foreach(f) = &script.blocks[0].verb else { panic!() };
    let Op::WhereRequired { var, attr, expr, .. } = &f.ops[0] else {
        panic!("expected WHERE REQUIRED")
    };
    assert_eq!(var, "a");
    assert_eq!(attr, "cost");
    // The attribute doubles as the condition's lhs.
    assert!(format!("{expr:?}").contains("Gt"), "{expr:?}");
}

#[test]
fn output_op_parses_star_list_and_alias() {
    let script = parse(
        "USE db.xml\nFOREACH g IN goods OUTPUT g.id, g.cost * 2 AS double, level;\nSELECT GROUP goods OUTPUT *;",
    )
    .unwrap();
    let Verb::Foreach(f) = &script.blocks[0].verb else { panic!() };
    let Op::Output { all: false, items, .. } = &f.ops[0] else { panic!("expected OUTPUT") };
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].1, "id");
    assert_eq!(items[1].1, "double");
    assert_eq!(items[2].1, "level");

    // `SELECT GROUP g OUTPUT ...` synthesizes an implicit loop.
    let Verb::Select { foreach: Some(f), .. } = &script.blocks[1].verb else { panic!() };
    assert_eq!(f.var, "goods");
    assert!(matches!(&f.ops[0], Op::Output { all: true, .. }));
}

#[test]
fn output_expression_without_alias_is_an_error() {
    let err = parse("USE db.xml FOREACH g IN goods OUTPUT g.cost * 2;").unwrap_err();
    assert!(err.message.contains("AS"), "{}", err.message);
}

#[test]
fn numeric_and_quoted_group_names() {
    let script = parse(
        "USE db.xml\nSELECT GROUP 110000 FOREACH item IN 110000 WHERE item.id < 5;\nSELECT GROUP \"Group\";",
    )
    .unwrap();
    assert!(matches!(
        &script.blocks[0].verb,
        Verb::Select { target: Selector::Group(g), .. } if g == "110000"
    ));
    assert!(matches!(
        &script.blocks[1].verb,
        Verb::Select { target: Selector::Group(g), .. } if g == "Group"
    ));
}

#[test]
fn expression_precedence() {
    // AND binds tighter than OR; comparison tighter than AND.
    let script =
        parse("USE db.xml FOREACH a IN g WHERE a.x = 1 OR a.y > 2 AND a.z < 3;").unwrap();
    let Verb::Foreach(f) = &script.blocks[0].verb else { panic!() };
    let Op::Where(expr) = &f.ops[0] else { panic!() };
    let printed = format!("{expr:?}");
    assert!(printed.starts_with("Binary { op: Or"), "{printed}");
}

#[test]
fn parse_error_reports_span() {
    let err = parse("USE db.xml\nSELECT arms;").unwrap_err();
    assert!(err.message.contains("expected GROUP, TAG or ROOT"), "{}", err.message);
    assert_eq!(err.span.unwrap().line, 2);
}

#[test]
fn global_set_and_analyze_statements() {
    let script = parse(
        "SET format = OFF;\nANALYZE;\nSET IGNORE_COMMENTS = off;\nUSE db.xml SELECT GROUP arms;",
    )
    .unwrap();
    assert_eq!(script.blocks.len(), 1);
    assert_eq!(script.settings.len(), 3);
    assert!(matches!(script.settings[0].setting, Setting::Format));
    assert!(!script.settings[0].value);
    assert!(matches!(script.settings[1].setting, Setting::Analyze));
    assert!(script.settings[1].value);
    assert!(matches!(script.settings[2].setting, Setting::IgnoreComments));
    assert!(!script.settings[2].value);

    let settings = Settings::resolve(&script.settings);
    assert!(!settings.format);
    assert!(!settings.ignore_comments);
    assert!(settings.analyze);
}

#[test]
fn set_analyze_off_overrides_analyze_statement() {
    let script = parse("ANALYZE;\nSET ANALYZE = OFF;\nUSE db.xml SELECT GROUP arms;").unwrap();
    assert!(!Settings::resolve(&script.settings).analyze);
}

#[test]
fn unknown_setting_is_an_error() {
    let err = parse("SET bogus = ON;").unwrap_err();
    assert!(err.message.contains("unknown setting `bogus`"), "{}", err.message);
    assert!(err.message.contains("FORMAT"));
}

#[test]
fn setting_value_must_be_on_or_off() {
    let err = parse("SET format = maybe;").unwrap_err();
    assert!(err.message.contains("expected ON or OFF"), "{}", err.message);
}

#[test]
fn set_requires_dotted_target() {
    let err = parse("USE db.xml FOREACH a IN g SET name = \"x\";").unwrap_err();
    assert!(err.message.contains("variable.attribute"), "{}", err.message);
}

#[test]
fn update_desugars_to_foreach() {
    // WHERE guard first, then the writes, then BREAK for LIMIT 1.
    let script = parse(
        "USE db.xml\nUPDATE office SET name = \"New\", office.cost = cost + 1 WHERE office.id = 216000 LIMIT 1;",
    )
    .unwrap();
    let Verb::Foreach(f) = &script.blocks[0].verb else { panic!("expected desugared FOREACH") };
    assert_eq!(f.var, "office");
    assert_eq!(f.source, LoopSource::Name("office".into()));
    assert!(matches!(&f.ops[0], Op::Where(_)));
    assert!(matches!(&f.ops[1], Op::Set { var, attr, .. } if var.is_empty() && attr == "name"));
    assert!(matches!(&f.ops[2], Op::Set { var, attr, .. } if var == "office" && attr == "cost"));
    assert!(matches!(&f.ops[3], Op::Break));
    assert_eq!(f.ops.len(), 4);
}

#[test]
fn update_without_where_or_limit() {
    let script = parse("USE db.xml\nUPDATE GROUP goods SET level = 1;").unwrap();
    let Verb::Foreach(f) = &script.blocks[0].verb else { panic!() };
    assert!(matches!(&f.ops[0], Op::Set { attr, .. } if attr == "level"));
    assert_eq!(f.ops.len(), 1);
}

#[test]
fn merge_shorthand_desugars_to_merge_ops() {
    let script = parse("USE db.xml\nMERGE arms SET tier = 1 WHERE cost > 100;").unwrap();
    let Verb::Foreach(f) = &script.blocks[0].verb else { panic!() };
    assert!(matches!(&f.ops[0], Op::Where(_)));
    assert!(matches!(&f.ops[1], Op::Merge { attr, .. } if attr == "tier"));
}

#[test]
fn delete_from_desugars_to_delete_elem() {
    let script = parse("USE db.xml\nDELETE FROM goods WHERE cost > 500 LIMIT 1;").unwrap();
    let Verb::Foreach(f) = &script.blocks[0].verb else { panic!() };
    assert!(matches!(&f.ops[0], Op::Where(_)));
    assert!(matches!(&f.ops[1], Op::DeleteElem { .. }));
    assert!(matches!(&f.ops[2], Op::Break));
}

#[test]
fn tag_selector_parses_everywhere() {
    let script = parse(
        "USE db.xml\nSELECT TAG ItemSpec;\nFOREACH i IN TAG ItemSpec SET i.seen = 1;\nDELETE IGNORE TAG Obsolete;\nUPDATE TAG ItemSpec SET cost = 0;",
    )
    .unwrap();
    assert!(matches!(
        &script.blocks[0].verb,
        Verb::Select { target: Selector::Tag(t), foreach: None, .. } if t == "ItemSpec"
    ));
    let Verb::Foreach(f) = &script.blocks[1].verb else { panic!() };
    assert_eq!(f.source, LoopSource::Tag("ItemSpec".into()));
    assert!(matches!(
        &script.blocks[2].verb,
        Verb::DeleteTag { tag, ignore: true } if tag == "Obsolete"
    ));
    let Verb::Foreach(f) = &script.blocks[3].verb else { panic!() };
    assert_eq!(f.source, LoopSource::Tag("ItemSpec".into()));
}

#[test]
fn root_selector_parses_everywhere() {
    let script = parse(
        "USE db.xml\nSELECT ROOT;\nFOREACH i IN ROOT SET i.seen = 1;\nUPDATE ROOT SET cost = 0;",
    )
    .unwrap();
    assert!(matches!(
        &script.blocks[0].verb,
        Verb::Select { target: Selector::Root, foreach: None, .. }
    ));
    let Verb::Foreach(f) = &script.blocks[1].verb else { panic!() };
    assert_eq!(f.source, LoopSource::Root);
    let Verb::Foreach(f) = &script.blocks[2].verb else { panic!() };
    assert_eq!(f.source, LoopSource::Root);
}

#[test]
fn aggregate_function_call_parses_as_expr_call() {
    let script = parse("USE db.xml\nFOREACH v IN g OUTPUT COUNT(v.id);").unwrap();
    let Verb::Foreach(f) = &script.blocks[0].verb else { panic!() };
    let Op::Output { items, .. } = &f.ops[0] else { panic!() };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].1, "count");
    assert!(matches!(&items[0].0, Expr::Call { func, .. } if func == "COUNT"));
}

#[test]
fn count_star_parses_as_expr_star_argument() {
    let script = parse("USE db.xml\nFOREACH v IN g OUTPUT COUNT(*);").unwrap();
    let Verb::Foreach(f) = &script.blocks[0].verb else { panic!() };
    let Op::Output { items, .. } = &f.ops[0] else { panic!() };
    let Expr::Call { func, arg, .. } = &items[0].0 else { panic!() };
    assert_eq!(func, "COUNT");
    assert!(matches!(**arg, Expr::Star(_)));
}

#[test]
fn limit_other_than_one_is_an_error() {
    let err = parse("USE db.xml\nUPDATE g SET a = 1 LIMIT 2;").unwrap_err();
    assert!(err.message.contains("only LIMIT 1"), "{}", err.message);
}

#[test]
fn delete_from_rejects_ignore() {
    let err = parse("USE db.xml\nDELETE IGNORE FROM goods;").unwrap_err();
    assert!(err.message.contains("IGNORE is not supported"), "{}", err.message);
}

#[test]
fn select_as_alias_parses_for_group_tag_and_root() {
    let script = parse(
        "USE db.xml\nSELECT GROUP arms AS weapons;\nSELECT TAG ItemSpec AS Item;\nSELECT ROOT AS anything;",
    )
    .unwrap();
    assert!(matches!(
        &script.blocks[0].verb,
        Verb::Select { target: Selector::Group(g), alias: Some(a), foreach: None }
            if g == "arms" && a == "weapons"
    ));
    assert!(matches!(
        &script.blocks[1].verb,
        Verb::Select { target: Selector::Tag(t), alias: Some(a), foreach: None }
            if t == "ItemSpec" && a == "Item"
    ));
    assert!(matches!(
        &script.blocks[2].verb,
        Verb::Select { target: Selector::Root, alias: Some(a), foreach: None } if a == "anything"
    ));
}

#[test]
fn select_as_alias_composes_with_foreach() {
    let script =
        parse("USE db.xml\nSELECT GROUP goods AS stuff FOREACH g IN goods WHERE g.cost > 5;")
            .unwrap();
    let Verb::Select { alias: Some(a), foreach: Some(f), .. } = &script.blocks[0].verb else {
        panic!("expected SELECT ... AS alias FOREACH ...")
    };
    assert_eq!(a, "stuff");
    assert!(matches!(&f.ops[0], Op::Where(_)));
}

#[test]
fn select_without_as_has_no_alias() {
    let script = parse("USE db.xml\nSELECT GROUP arms;").unwrap();
    assert!(matches!(&script.blocks[0].verb, Verb::Select { alias: None, .. }));
}

#[test]
fn rename_group_and_tag_parse() {
    let script = parse(
        "USE db.xml\nRENAME GROUP arms AS weapons;\nRENAME TAG ItemSpec AS Item;\nRENAME IGNORE GROUP missing AS x;",
    )
    .unwrap();
    assert!(matches!(
        &script.blocks[0].verb,
        Verb::RenameGroup { group, new_tag, ignore: false } if group == "arms" && new_tag == "weapons"
    ));
    assert!(matches!(
        &script.blocks[1].verb,
        Verb::RenameTag { tag, new_tag, ignore: false } if tag == "ItemSpec" && new_tag == "Item"
    ));
    assert!(matches!(
        &script.blocks[2].verb,
        Verb::RenameGroup { group, ignore: true, .. } if group == "missing"
    ));
}

#[test]
fn rename_requires_as() {
    let err = parse("USE db.xml\nRENAME GROUP arms weapons;").unwrap_err();
    assert!(err.message.contains("AS"), "{}", err.message);
}
