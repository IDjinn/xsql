# xsql

SQL-like language for querying and mutating XML files. Built for automated
editing toolchains: reads a script (or inline query), applies it to XML
documents in memory, and writes the result to stdout — a well-behaved Unix
filter.

## Build

```
cargo build --release        # -> target/release/xsql
```

## Usage

```
xsql                                 # interactive REPL (like node/python)
xsql script.xsql                     # run a script file
xsql -e "USE db.xml SELECT GROUP arms;"      # inline query
cat script.xsql | xsql               # script from stdin
cat db.xml | xsql -e "USE INPUT ..." # XML from stdin (pipeline filter)
```

```sql
USE db.xml
FOREACH good IN goods
    WHERE good.cost > 500
    SET good.cost = good.cost * 2
;
```

Output goes to stdout: `SELECT` results print immediately, and every
document a script *modified* is pretty-printed after the script finishes.
Errors go to stderr with `file:line:col` diagnostics and a non-zero exit code.

## Docs

Full language reference, architecture and performance notes live at
[xsql-docs](https://github.com/IDjinn/xsql-docs) (checked out here under
`docs/` as a submodule).

## Workspace

- `xsql/` — the library + binary
- `xsql-tests/` — all tests (lexer, parser, DOM, interpreter, CLI end-to-end)
  and fixtures; run `cargo test` from the workspace root
- `docs/` — the xsql-docs submodule
