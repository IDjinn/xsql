use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use xsql::ast::Source;
use xsql::eval;
use xsql::lexer::{Tok, lex};
use xsql::parser;

const USAGE: &str = "\
xsql - SQL-like language for querying and mutating XML files

Usage:
  xsql                          interactive mode (REPL)
  xsql <script.xsql>            run a script file
  xsql -e \"<query>\"             run an inline query
  <producer> | xsql             read the script from stdin
  <xml> | xsql script.xsql      pipe an XML document to `USE INPUT`

Options:
  -e, --eval <QUERY>   inline query
  -i, --interactive    force interactive mode even with piped stdin
  -h, --help           show this help
  -V, --version        show version
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
    let mut script_path: Option<String> = None;
    let mut force_repl = false;
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
            "-e" | "--eval" => match iter.next() {
                Some(query) => eval_query = Some(query),
                None => return usage_error("missing query after -e"),
            },
            _ if arg.starts_with('-') => {
                return usage_error(&format!("unknown option `{arg}`"));
            }
            _ => {
                if script_path.is_some() {
                    return usage_error("only one script file may be given");
                }
                script_path = Some(arg);
            }
        }
    }

    if eval_query.is_some() && script_path.is_some() {
        return usage_error("-e and a script file are mutually exclusive");
    }

    let mut script_from_stdin = false;
    let (source_name, source) = if let Some(query) = eval_query {
        ("<eval>".to_string(), query)
    } else if let Some(path) = script_path {
        match std::fs::read_to_string(&path) {
            Ok(text) => (path, text),
            Err(e) => {
                eprintln!("error: cannot read script `{path}`: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else if force_repl || std::io::stdin().is_terminal() {
        // No args and nothing piped in (or -i forced it): interactive mode,
        // like node/python.
        return repl();
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

    match eval::run_with_report(&script, stdin_xml) {
        Ok((output, report)) => {
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
                    print!("{output}");
                    let _ = std::io::stdout().flush();
                    report.push("write stdout", write_start.elapsed());
                    eprint!("{}", report.render(total_start.elapsed()));
                }
                None => print!("{output}"),
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

fn repl() -> ExitCode {
    println!("xsql {} — interactive mode", env!("CARGO_PKG_VERSION"));
    println!("End statements with `;`. Commands: .help  .dump  BEGIN  COMMIT  ROLLBACK [TO name]  CHECKPOINT name (alias SAVEPOINT)  exit");

    let mut session = eval::Session::new(None);
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

fn read_stdin() -> std::io::Result<String> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    Ok(text)
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("error: {message}\n\n{USAGE}");
    ExitCode::from(2)
}
