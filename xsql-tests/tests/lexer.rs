use xsql::lexer::{Tok, lex};

fn kinds(src: &str) -> Vec<Tok> {
    lex(src).unwrap().into_iter().map(|t| t.tok).collect()
}

#[test]
fn semicolon_eats_rest_of_line() {
    let toks = kinds("; this whole line is a comment\nSELECT GROUP arms;trailing junk\n");
    assert_eq!(
        toks,
        vec![
            Tok::Semi,
            Tok::Select,
            Tok::Group,
            Tok::Ident("arms".into()),
            Tok::Semi,
            Tok::Eof
        ]
    );
}

#[test]
fn use_lexes_bare_path_with_dots() {
    let toks = kinds("USE database.local.xml\nSELECT");
    assert_eq!(
        toks,
        vec![
            Tok::Use,
            Tok::Path("database.local.xml".into()),
            Tok::Select,
            Tok::Eof
        ]
    );
}

#[test]
fn use_input_keyword_case_insensitive() {
    assert_eq!(kinds("USE INPUT"), vec![Tok::Use, Tok::Input, Tok::Eof]);
    assert_eq!(kinds("use input"), vec![Tok::Use, Tok::Input, Tok::Eof]);
}

#[test]
fn use_quoted_path_with_spaces() {
    let toks = kinds(r#"USE "my dir/data file.xml""#);
    assert_eq!(
        toks,
        vec![Tok::Use, Tok::Str("my dir/data file.xml".into()), Tok::Eof]
    );
}

#[test]
fn single_quoted_strings() {
    let toks = kinds("USE 'my dir/db.xml' SELECT GROUP 'new_continued_cost'");
    assert_eq!(
        toks,
        vec![
            Tok::Use,
            Tok::Str("my dir/db.xml".into()),
            Tok::Select,
            Tok::Group,
            Tok::Str("new_continued_cost".into()),
            Tok::Eof
        ]
    );
}

#[test]
fn dotted_ident_and_comparison() {
    let toks = kinds("WHERE goods.id > 52034301");
    assert_eq!(
        toks,
        vec![
            Tok::Where,
            Tok::Ident("goods.id".into()),
            Tok::Gt,
            Tok::Num(52034301.0),
            Tok::Eof
        ]
    );
}

#[test]
fn raw_xml_multiline() {
    let toks = kinds("RAW XML `\n<a id=\"1\"/>\n<b/>\n`");
    assert_eq!(
        toks,
        vec![
            Tok::Raw,
            Tok::Xml,
            Tok::RawXml("\n<a id=\"1\"/>\n<b/>\n".into()),
            Tok::Eof
        ]
    );
}

#[test]
fn string_and_set() {
    let toks = kinds(r#"SET office.name = "New Office Name""#);
    assert_eq!(
        toks,
        vec![
            Tok::Set,
            Tok::Ident("office.name".into()),
            Tok::Eq,
            Tok::Str("New Office Name".into()),
            Tok::Eof
        ]
    );
}

#[test]
fn operators() {
    let toks = kinds("a != b <> c <= d >= e + f - g * h / i");
    assert!(toks.contains(&Tok::NotEq));
    assert!(toks.contains(&Tok::Le));
    assert!(toks.contains(&Tok::Ge));
    assert!(toks.contains(&Tok::Plus));
    assert!(toks.contains(&Tok::Star));
    assert!(toks.contains(&Tok::Slash));
}

#[test]
fn unterminated_string_errors_with_span() {
    let err = lex("SET a.b = \"oops").unwrap_err();
    assert!(err.message.contains("unterminated string"));
    assert!(err.span.is_some());
}

#[test]
fn unterminated_raw_xml_errors() {
    let err = lex("RAW XML `<a/>").unwrap_err();
    assert!(err.message.contains("unterminated raw XML"));
}
