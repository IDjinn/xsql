//! End-to-end tests spawning the real `xsql` binary.
//! Run via `cargo test` from the workspace root so the binary is built.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn xsql_bin() -> PathBuf {
    // target/debug/deps/cli-<hash>.exe -> target/debug/xsql[.exe]
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push(format!("xsql{}", std::env::consts::EXE_SUFFIX));
    assert!(path.exists(), "binary not built at {path:?}; run `cargo test` from the workspace root");
    path
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/database.local.xml")
}

/// Fixture path with forward slashes, safe inside quoted xsql strings
/// (backslash starts an escape sequence there).
fn fixture_str() -> String {
    fixture().display().to_string().replace('\\', "/")
}

struct Output {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run_xsql(args: &[&str], stdin: Option<&str>, cwd: Option<&PathBuf>) -> Output {
    let mut cmd = Command::new(xsql_bin());
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(text) = stdin {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    Output {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

#[test]
fn eval_flag_selects_group() {
    let query = format!("USE \"{}\" SELECT GROUP arms;", fixture_str());
    let out = run_xsql(&["-e", &query], None, None);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains(r#"<ItemSpec id="101""#));
}

#[test]
fn script_from_stdin() {
    let script = format!("USE \"{}\" SELECT GROUP goods;", fixture_str());
    let out = run_xsql(&[], Some(&script), None);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains(r#"id="52034301""#));
}

#[test]
fn xml_piped_to_use_input() {
    let xml = r#"<db><arms><ItemSpec id="7" cost="1"/></arms></db>"#;
    let out = run_xsql(
        &["-e", "USE INPUT FOREACH a IN arms SET a.cost = a.cost + 10;"],
        Some(xml),
        None,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains(r#"cost="11""#));
}

#[test]
fn use_input_with_script_from_stdin_is_rejected() {
    let out = run_xsql(&[], Some("USE INPUT SELECT GROUP arms;"), None);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("USE INPUT"));
}

#[test]
fn runtime_error_sets_exit_code_and_span() {
    let query = format!("USE \"{}\"\nSELECT GROUP nope;", fixture_str());
    let out = run_xsql(&["-e", &query], None, None);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("`nope` not found"));
    assert!(out.stderr.contains("--> <eval>:2:"));
}

/// ANALYZE report lands on stderr; stdout stays a clean XML stream.
#[test]
fn analyze_report_goes_to_stderr() {
    let xml = r#"<db><arms><ItemSpec id="7"/></arms></db>"#;
    let out = run_xsql(
        &["-e", "ANALYZE;\nUSE INPUT SELECT GROUP arms;"],
        Some(xml),
        None,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains(r#"<ItemSpec id="7"/>"#));
    assert!(!out.stdout.contains("ANALYZE"), "{}", out.stdout);
    assert!(out.stderr.contains("-- ANALYZE"), "{}", out.stderr);
    assert!(out.stderr.contains("lex"), "{}", out.stderr);
    assert!(out.stderr.contains("parse"), "{}", out.stderr);
    assert!(out.stderr.contains("read stdin"), "{}", out.stderr);
    assert!(out.stderr.contains("write stdout"), "{}", out.stderr);
    assert!(out.stderr.contains("memory (documents)"), "{}", out.stderr);
    assert!(out.stderr.contains("total"), "{}", out.stderr);
}

#[test]
fn usage_error_exit_code() {
    let out = run_xsql(&["--bogus"], None, None);
    assert_eq!(out.code, 2);
}

/// The original scratch-file script, run verbatim from a temp working dir
/// containing `database.local.xml`.
#[test]
fn scratch_script_runs_verbatim() {
    let dir = std::env::temp_dir().join(format!("xsql-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fixture(), dir.join("database.local.xml")).unwrap();
    let script = r#"; remove some attributes from all values inside a group
USE database.local.xml
FOREACH arm IN arms
    DELETE IGNORE arm.unlock_civi_science
    DELETE IGNORE arm.science
;

USE database.local.xml
SELECT GROUP office
FOREACH office IN office
    WHERE office.id = 216000
    SET office.name = "New Office Name"
    BREAK;
;
"#;
    std::fs::write(dir.join("script.xsql"), script).unwrap();

    let out = run_xsql(&["script.xsql"], None, Some(&dir));
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains(r#"name="New Office Name""#));
    assert!(!out.stdout.contains("unlock_civi_science"));

    std::fs::remove_dir_all(&dir).ok();
}
