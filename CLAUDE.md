# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```
cargo build --release              # builds the binary -> target/release/xsql
cargo test                         # run all tests (from workspace root)
cargo test --test parser           # one test file (lexer|parser|xml|eval|cli)
cargo test --test eval test_name   # single test by name
```

Tests live in the `xsql-tests` crate (integration tests + `fixtures/`), not alongside the source. The `cli.rs` tests run the compiled binary end-to-end, so they need a debug build (`cargo test` handles this automatically).

## What this is

`xsql` is a SQL-like language for querying and mutating XML files, shipped as a Unix-filter CLI (script file, `-e` inline query, stdin script, or REPL). Output goes to stdout: SELECT results immediately, then every *modified* document pretty-printed after the script finishes. Errors render `file:line:col` diagnostics to stderr. See README.md for the full language reference.

## Architecture

Classic interpreter pipeline, one module per stage in `xsql/src/`:

1. **`lexer.rs`** — hand-rolled single-pass lexer producing spanned tokens. `;` terminates a statement block and the rest of that line is a comment.
2. **`parser.rs`** — recursive-descent parser → **`ast.rs`** (`Script` → `Block`s, each with a sticky `Source` from `USE`, containing `Verb`s / `Foreach` / `Expr`).
3. **`eval/mod.rs`** — the interpreter. Its module doc comment describes the execution model precisely; the key points:
   - All distinct `USE` sources load/parse in parallel (rayon).
   - Blocks are grouped by target document; groups for *different* documents run in parallel, blocks within one document stay sequential. SELECT output is buffered per block and flushed in script order.
   - `FOREACH` over ≥1024 children (`PAR_FOREACH_THRESHOLD`) uses evaluate-then-apply: expressions evaluated read-only in parallel producing mutation plans, plans applied sequentially — exact sequential semantics must be preserved. Loops containing `BREAK` stay fully sequential.
   - `eval/value.rs` — dynamically typed `Value`: numeric semantics when both operands parse as numbers, `+` concatenates otherwise, missing attributes compare as false.
4. **`xml/`** — arena DOM on top of quick-xml. `dom.rs`: nodes in a flat `Vec<Element>` linked by `NodeId` (usize) indexes — this layout is what makes read-only parallel evaluation over `&Document` safe. `parse.rs` builds it, `serialize.rs` pretty-prints (data-only: comments/formatting are not preserved — that's a roadmap item).
5. **`error.rs`** — `XsqlError` with optional `Span`; `render()` produces the `file:line:col` + source-line diagnostic. Keep spans on errors wherever a span is available.

`main.rs` is CLI arg handling + the REPL; all logic lives in the library so tests and the binary share it.

## Invariants to respect

- Parallelism must never change observable behavior — the evaluate-then-apply split and per-document block grouping exist to keep results identical to sequential execution.
- A *group* lookup matches the first element whose tag, `name`, or `id` attribute equals the given name.
- Inside a `FOREACH`, the loop variable, group name, and bare attribute name all resolve to the current element.
