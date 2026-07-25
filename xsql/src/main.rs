use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use xsql::ast::Source;
use xsql::eval;
use xsql::lexer::{Tok, lex};
use xsql::parser;
use xsql::xml::XmlParseError;
#[cfg(not(feature = "simd"))]
use xsql::xml::parse::parse_document_diag;
#[cfg(feature = "simd")]
use xsql::xml::parse_simd::parse_document_diag;

const USAGE: &str = "\
xsql - SQL-like language for querying and mutating XML files

Usage:
  xsql                          interactive mode (REPL)
  xsql <script.xsql>            run a script file
  xsql -e \"<query>\"             run an inline query
  xsql -c <file.xml>...         validate that XML files are well-formed
  <producer> | xsql             read the script from stdin
  <xml> | xsql script.xsql      pipe an XML document to `USE INPUT`
  <xml> | xsql -c               validate XML piped on stdin

Options:
  -e, --eval <QUERY>       inline query
  -c, --check              validate XML well-formedness instead of running a script
  -i, --interactive        force interactive mode even with piped stdin
  -E, --encoding <ENC>     output encoding for emitted/saved documents
                           (default: preserve each file's source encoding;
                           e.g. utf-8, utf-8-bom, utf-16, windows-1252, latin1)
  -o, --output <FILE>      write modified documents to FILE instead of stdout
                           (exact bytes — immune to shell redirection
                           re-encoding); SELECT results still print to stdout
  -h, --help               show this help
  -V, --version            show version

Input encoding is detected automatically (BOM, UTF-16, or the XML
declaration's encoding attribute).
";

const REPL_HELP: &str = "\
REPL commands (bare or dot-prefixed, e.g. `COMMIT` or `.commit`):
  .help                  show this help
  .dump                  preview every modified document (stdout only)
  BEGIN                  clear checkpoints (mutations are already implicit)
  COMMIT                 write every modified document back to disk
  ROLLBACK               discard changes back to the last COMMIT/load
  ROLLBACK TO <name>     discard changes back to a named CHECKPOINT
  CHECKPOINT <name>      snapshot every loaded document under <name>
  SAVEPOINT <name>       alias for CHECKPOINT
  exit | quit | .exit    leave the REPL (Ctrl+D also works)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut eval_query: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut force_repl = false;
    let mut check_mode = false;
    let mut out_encoding: Option<xsql::xml::encoding::XmlEncoding> = None;
    let mut output_path: Option<String> = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("xsql {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "-i" | "--interactive" => force_repl = true,
            "-c" | "--check" => check_mode = true,
            "-e" | "--eval" => match iter.next() {
                Some(query) => eval_query = Some(query),
                None => return usage_error("missing query after -e"),
            },
            "-o" | "--output" => match iter.next() {
                Some(path) => output_path = Some(path),
                None => return usage_error("missing file after -o"),
            },
            "-E" | "--encoding" => match iter.next() {
                Some(label) => match xsql::xml::encoding::from_label(&label) {
                    Some(enc) => out_encoding = Some(enc),
                    None => {
                        return usage_error(&format!(
                            "unknown encoding `{label}` (try utf-8, utf-8-bom, utf-16, utf-16be, windows-1252, latin1, ...)"
                        ));
                    }
                },
                None => return usage_error("missing encoding name after -E"),
            },
            _ if arg.starts_with('-') => {
                return usage_error(&format!("unknown option `{arg}`"));
            }
            _ => paths.push(arg),
        }
    }

    if check_mode {
        if eval_query.is_some() {
            return usage_error("-c and -e are mutually exclusive");
        }
        if output_path.is_some() {
            return usage_error("-c and -o are mutually exclusive");
        }
        return check_xml(&paths, out_encoding);
    }

    let script_path = match paths.len() {
        0 => None,
        1 => Some(paths.remove(0)),
        _ => return usage_error("only one script file may be given"),
    };

    if eval_query.is_some() && script_path.is_some() {
        return usage_error("-e and a script file are mutually exclusive");
    }

    let mut script_from_stdin = false;
    let (source_name, source) = if let Some(query) = eval_query {
        ("<eval>".to_string(), query)
    } else if let Some(path) = script_path {
        // Scripts also go through encoding detection, so a BOM'd or UTF-16
        // file (e.g. created by PowerShell redirection) still lexes.
        match std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| {
            xsql::xml::encoding::decode(&bytes).map(|(text, _)| text)
        }) {
            Ok(text) => (path, text),
            Err(e) => {
                eprintln!("error: cannot read script `{path}`: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else if force_repl || std::io::stdin().is_terminal() {
        // No args and nothing piped in (or -i forced it): interactive mode,
        // like node/python.
        if output_path.is_some() {
            return usage_error("-o needs a script, an -e query, or a piped script");
        }
        return repl(out_encoding);
    } else {
        script_from_stdin = true;
        match read_stdin() {
            Ok(text) => ("<stdin>".to_string(), text),
            Err(e) => {
                eprintln!("error: cannot read script from stdin: {e}");
                return ExitCode::FAILURE;
            }
        }
    };

    let total_start = std::time::Instant::now();
    let (script, parse_times) = match parser::parse_with_times(&source) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("{}", e.render(&source_name, &source));
            return ExitCode::FAILURE;
        }
    };

    let uses_input = script.blocks.iter().any(|b| b.source == Source::Input);
    let stdin_start = std::time::Instant::now();
    let stdin_xml = if uses_input && !script_from_stdin {
        match read_stdin() {
            Ok(xml) => Some(xml),
            Err(e) => {
                eprintln!("error: cannot read XML from stdin: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let stdin_time = stdin_start.elapsed();
    let read_stdin_xml = stdin_xml.is_some();

    match eval::run_with_options(&script, stdin_xml, out_encoding) {
        Ok((output, report)) => {
            let write_output = |out: &eval::RunOutput| -> Result<&'static str, String> {
                // SELECT results are query output for the terminal; `-o`
                // receives only the modified documents, so SELECT text can
                // never corrupt the written XML.
                if !out.selects.is_empty() {
                    let _ = std::io::stdout().write_all(&out.selects);
                    let _ = std::io::stdout().flush();
                }
                match &output_path {
                    Some(path) => std::fs::write(path, &out.documents)
                        .map(|()| "write output file")
                        .map_err(|e| format!("cannot write `{path}`: {e}")),
                    None => {
                        let _ = std::io::stdout().write_all(&out.documents);
                        let _ = std::io::stdout().flush();
                        Ok("write stdout")
                    }
                }
            };
            match report {
                Some(mut report) => {
                    let mut pre = vec![
                        ("lex".to_string(), parse_times.lex),
                        ("parse".to_string(), parse_times.parse),
                    ];
                    if read_stdin_xml {
                        pre.push(("read stdin".to_string(), stdin_time));
                    }
                    report.prepend(pre);
                    let write_start = std::time::Instant::now();
                    let label = match write_output(&output) {
                        Ok(label) => label,
                        Err(e) => {
                            eprintln!("error: {e}");
                            return ExitCode::FAILURE;
                        }
                    };
                    report.push(label, write_start.elapsed());
                    eprint!("{}", report.render(total_start.elapsed()));
                }
                None => {
                    if let Err(e) = write_output(&output) {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", e.render(&source_name, &source));
            ExitCode::FAILURE
        }
    }
}

/// REPL-only transaction/meta commands, recognized as plain text (not
/// through the lexer/parser) so they never collide with real xsql grammar —
/// every real statement starts with `USE`.
enum Meta {
    Begin,
    Commit,
    Rollback(Option<String>),
    Checkpoint(String),
}

/// Recognizes a bare SQL-style spelling (`COMMIT`) or a dot-prefixed
/// spelling (`.commit`), case-insensitively. `None` means "not a
/// meta-command, fall through to the normal xsql statement pipeline".
/// `Some(Err(..))` means the keyword matched but its arguments didn't.
fn parse_meta(line: &str) -> Option<std::result::Result<Meta, String>> {
    let line = line.split(';').next().unwrap_or("").trim();
    let mut words = line.split_whitespace();
    let head = words.next()?.to_ascii_uppercase();
    let head = head.trim_start_matches('.');
    let rest: Vec<&str> = words.collect();
    Some(Ok(match head {
        "BEGIN" if rest.is_empty() => Meta::Begin,
        "COMMIT" if rest.is_empty() => Meta::Commit,
        "ROLLBACK" => match rest.as_slice() {
            [] => Meta::Rollback(None),
            [to, name] if to.eq_ignore_ascii_case("TO") => Meta::Rollback(Some((*name).to_string())),
            _ => return Some(Err("usage: ROLLBACK  |  ROLLBACK TO <name>".into())),
        },
        "CHECKPOINT" | "SAVEPOINT" => match rest.as_slice() {
            [name] => Meta::Checkpoint((*name).to_string()),
            _ => return Some(Err("usage: CHECKPOINT <name>  (alias: SAVEPOINT)".into())),
        },
        _ => return None,
    }))
}

fn repl(out_encoding: Option<xsql::xml::encoding::XmlEncoding>) -> ExitCode {
    println!("xsql {} — interactive mode", env!("CARGO_PKG_VERSION"));
    println!("End statements with `;`. Commands: .help  .dump  BEGIN  COMMIT  ROLLBACK [TO name]  CHECKPOINT name (alias SAVEPOINT)  exit");

    let mut session = eval::Session::new(None);
    session.set_output_encoding(out_encoding);
    let mut current: Option<Source> = None;
    let mut buffer = String::new();
    let stdin = std::io::stdin();

    loop {
        print!("{}", if buffer.is_empty() { "xsql> " } else { " ...> " });
        let _ = std::io::stdout().flush();

        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => break, // EOF (Ctrl+Z / Ctrl+D)
            Ok(_) => {}
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }

        if buffer.is_empty() {
            match line.trim() {
                "" => continue,
                "exit" | "quit" | ".exit" => break,
                ".help" => {
                    print!("{REPL_HELP}");
                    continue;
                }
                ".dump" => {
                    if session.has_modifications() {
                        print!("{}", session.dump_modified());
                    } else {
                        println!("(no modified documents)");
                    }
                    continue;
                }
                other => match parse_meta(other) {
                    Some(Ok(Meta::Begin)) => {
                        let n = session.begin();
                        println!("BEGIN (cleared {n} checkpoint(s))");
                        continue;
                    }
                    Some(Ok(Meta::Commit)) => {
                        let report = session.commit();
                        if report.outcomes.is_empty() {
                            println!("(nothing to commit)");
                        } else {
                            for (source, outcome) in &report.outcomes {
                                match outcome {
                                    eval::CommitOutcome::Saved(path) => println!("committed -> {path}"),
                                    eval::CommitOutcome::PrintedToStdout(_) => {
                                        println!("committed {} (no file; printed below)", source.describe())
                                    }
                                    eval::CommitOutcome::Failed(e) => {
                                        eprintln!("error committing {}: {e}", source.describe())
                                    }
                                }
                            }
                            print!("{}", report.stdout_text);
                        }
                        continue;
                    }
                    Some(Ok(Meta::Rollback(to))) => {
                        match session.rollback(to.as_deref()) {
                            Ok(sources) if sources.is_empty() => println!("(nothing to roll back)"),
                            Ok(sources) => println!(
                                "rolled back: {}",
                                sources.iter().map(Source::describe).collect::<Vec<_>>().join(", ")
                            ),
                            Err(e) => eprintln!("error: {e}"),
                        }
                        continue;
                    }
                    Some(Ok(Meta::Checkpoint(name))) => {
                        let n = session.checkpoint(&name);
                        println!("CHECKPOINT '{name}' created ({n} document(s))");
                        continue;
                    }
                    Some(Err(msg)) => {
                        eprintln!("error: {msg}");
                        continue;
                    }
                    None => {}
                },
            }
        }

        buffer.push_str(&line);
        if !statement_ready(&buffer) {
            continue;
        }

        let submitted = std::mem::take(&mut buffer);
        match parser::parse_session(&submitted, current.clone()) {
            Ok((script, next_current)) => {
                current = next_current;
                match session.exec(&script) {
                    Ok(output) => print!("{output}"),
                    Err(e) => eprintln!("{}", e.render("<repl>", &submitted)),
                }
            }
            Err(e) => eprintln!("{}", e.render("<repl>", &submitted)),
        }
    }

    // Leaving the REPL: emit any pending edits so `xsql > out.xml` still works.
    if session.has_modifications() {
        print!("{}", session.dump_modified());
        eprintln!(
            "warning: uncommitted changes were not saved to disk (use COMMIT before exiting to persist them)"
        );
    }
    ExitCode::SUCCESS
}

/// A buffered REPL entry is ready to run once it lexes cleanly and its last
/// token is the `;` terminator. Unterminated strings / raw XML keep the
/// continuation prompt open (multi-line entry).
fn statement_ready(buffer: &str) -> bool {
    match lex(buffer) {
        Ok(tokens) => tokens
            .iter()
            .rev()
            .find(|t| t.tok != Tok::Eof)
            .is_some_and(|t| t.tok == Tok::Semi),
        // Real lex errors surface on parse; only "unterminated" means
        // the user is still typing.
        Err(e) => !e.message.contains("unterminated"),
    }
}

/// Reads all of stdin and decodes it (BOM / UTF-16 / decl-aware), so piped
/// scripts and XML survive shells that re-encode text streams.
fn read_stdin() -> Result<String, String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let (text, _) = xsql::xml::encoding::decode(&bytes)?;
    Ok(text)
}

/// `-c/--check`: parse each file (or stdin) as XML and report whether it is
/// well-formed. `<file>: OK` per valid input on stdout, diagnostics with
/// source context on stderr; exit code 1 if any input is invalid.
fn check_xml(paths: &[String], enc_override: Option<xsql::xml::encoding::XmlEncoding>) -> ExitCode {
    let decode = |bytes: &[u8]| -> Result<String, String> {
        match enc_override {
            Some(enc) => xsql::xml::encoding::decode_as(enc, bytes),
            None => xsql::xml::encoding::decode(bytes).map(|(text, _)| text),
        }
    };
    if paths.is_empty() {
        if std::io::stdin().is_terminal() {
            return usage_error("-c needs at least one XML file (or XML piped on stdin)");
        }
        let xml = match read_stdin() {
            Ok(text) => text,
            Err(e) => {
                eprintln!("error: cannot read XML from stdin: {e}");
                return ExitCode::FAILURE;
            }
        };
        return match parse_document_diag(&xml, false) {
            Ok(_) => {
                println!("<stdin>: OK");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprint!("{}", render_xml_error("<stdin>", &xml, &e));
                ExitCode::FAILURE
            }
        };
    }

    let mut failed = false;
    for path in paths {
        match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| decode(&bytes))
        {
            Ok(xml) => match parse_document_diag(&xml, false) {
                Ok(_) => println!("{path}: OK"),
                Err(e) => {
                    failed = true;
                    eprint!("{}", render_xml_error(path, &xml, &e));
                }
            },
            Err(e) => {
                failed = true;
                eprintln!("error: cannot read `{path}`: {e}");
            }
        }
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

/// Max chars of a source line shown in `--check` diagnostics; minified XML
/// can put the whole document on one line.
const CHECK_CONTEXT_WIDTH: usize = 120;

/// `file:line:col` diagnostic with the offending line plus one line of
/// context on each side, caret under the failure column:
///
/// ```text
/// error: db.xml:12:8: malformed XML: mismatched end tag
///   11 |   <arms>
///   12 |     <Item></arm>
///      |        ^
///   13 |   </arms>
/// ```
fn render_xml_error(name: &str, source: &str, err: &XmlParseError) -> String {
    let Some((line, col)) = err.line_col(source) else {
        return format!("error: {name}: malformed XML: {}\n", err.message);
    };
    let mut out = format!("error: {name}:{line}:{col}: malformed XML: {}\n", err.message);

    let lines: Vec<&str> = source.lines().collect();
    let first = line.saturating_sub(1).max(1);
    let last = (line + 1).min(lines.len().max(line));
    let width = last.to_string().len();
    for n in first..=last {
        let text = lines.get(n - 1).copied().unwrap_or("");
        if n == line {
            let (clipped, caret) = clip_around(text, col, CHECK_CONTEXT_WIDTH);
            out.push_str(&format!(" {n:>width$} | {clipped}\n"));
            out.push_str(&format!(" {:>width$} | {}^\n", "", " ".repeat(caret)));
        } else {
            let (clipped, _) = clip_around(text, 1, CHECK_CONTEXT_WIDTH);
            out.push_str(&format!(" {n:>width$} | {clipped}\n"));
        }
    }
    out
}

/// Clips `line` to a window of at most `width` bytes centered on 1-based
/// byte column `col`, with `…` marking trimmed ends. Returns the display
/// string and the 0-based char offset for the caret within it.
fn clip_around(line: &str, col: usize, width: usize) -> (String, usize) {
    if line.len() <= width {
        let caret = line
            .char_indices()
            .take_while(|(i, _)| *i < col.saturating_sub(1))
            .count();
        return (line.to_string(), caret.min(line.chars().count()));
    }
    let target = (col - 1).min(line.len());
    let mut start = target.saturating_sub(width / 2);
    let mut end = (start + width).min(line.len());
    while start > 0 && !line.is_char_boundary(start) {
        start -= 1;
    }
    while end < line.len() && !line.is_char_boundary(end) {
        end += 1;
    }
    let mut display = String::new();
    if start > 0 {
        display.push('…');
    }
    display.push_str(&line[start..end]);
    if end < line.len() {
        display.push('…');
    }
    let caret = (start > 0) as usize
        + line[start..target.max(start)]
            .chars()
            .count();
    (display, caret)
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("error: {message}\n\n{USAGE}");
    ExitCode::from(2)
}
