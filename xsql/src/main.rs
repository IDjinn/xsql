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
  -h, --help           show this help
  -V, --version        show version
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut eval_query: Option<String> = None;
    let mut script_path: Option<String> = None;
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
    } else if std::io::stdin().is_terminal() {
        // No args and nothing piped in: interactive mode, like node/python.
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

    let script = match parser::parse(&source) {
        Ok(script) => script,
        Err(e) => {
            eprintln!("{}", e.render(&source_name, &source));
            return ExitCode::FAILURE;
        }
    };

    let uses_input = script.blocks.iter().any(|b| b.source == Source::Input);
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

    match eval::run(&script, stdin_xml) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", e.render(&source_name, &source));
            ExitCode::FAILURE
        }
    }
}

fn repl() -> ExitCode {
    println!("xsql {} — interactive mode", env!("CARGO_PKG_VERSION"));
    println!("End statements with `;`. Commands: .help  .dump (print modified docs)  exit");

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
                    print!("{USAGE}");
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
                _ => {}
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
