use xsql::ast::{Op, Source, Verb};
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

    let Verb::Select { group, foreach } = &script.blocks[0].verb else {
        panic!("block 0 should be SELECT");
    };
    assert_eq!(group, "arms");
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
fn numeric_and_quoted_group_names() {
    let script = parse(
        "USE db.xml\nSELECT GROUP 110000 FOREACH item IN 110000 WHERE item.id < 5;\nSELECT GROUP \"Group\";",
    )
    .unwrap();
    assert!(matches!(&script.blocks[0].verb, Verb::Select { group, .. } if group == "110000"));
    assert!(matches!(&script.blocks[1].verb, Verb::Select { group, .. } if group == "Group"));
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
    assert!(err.message.contains("expected `GROUP`"), "{}", err.message);
    assert_eq!(err.span.unwrap().line, 2);
}

#[test]
fn set_requires_dotted_target() {
    let err = parse("USE db.xml FOREACH a IN g SET name = \"x\";").unwrap_err();
    assert!(err.message.contains("variable.attribute"), "{}", err.message);
}
