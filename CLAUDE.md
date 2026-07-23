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

`xsql` is a SQL-like language for querying and mutating XML files, shipped as a Unix-filter CLI (script file, `-e` inline query, stdin script, or REPL). Output goes to stdout: SELECT results immediately, then every *modified* document pretty-printed after the script finishes. Errors render `file:line:col` diagnostics to stderr. README.md is intentionally minimal (description/usage/build); the full language reference lives in the `docs/` submodule (xsql-docs, `content/docs/language/`).

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
- A *group* lookup matches the first element whose tag, `name`, or `id` attribute equals the given name. A *tag* lookup (`TAG t`) matches **every** element whose tag equals `t` (attributes don't participate); a *root* lookup (`ROOT`) matches **every** element regardless of tag — same idea as `TAG`, minus the filter, for when the tag name isn't known ahead of time. Both `TAG` and `ROOT` loops only take the parallel FOREACH path after a disjointness check, since matches can nest.
- The MySQL-style shorthands (`UPDATE`, `MERGE ... SET`, `DELETE FROM`) are pure parser sugar: they desugar to `Verb::Foreach` (`WHERE` guard first, then the writes, `LIMIT 1` → `BREAK`) — the interpreter never sees them.
- Inside a `FOREACH`, the loop variable, group name, and bare attribute name all resolve to the current element.
- Aggregate `OUTPUT` items (`COUNT`/`MIN`/`MAX`/`SUM`/`AVG`/`FIRST`/`LAST`/`ANY`/`ALL`, parsed generically as `Expr::Call`) are evaluated across every element the loop reaches rather than per element: `plan_ops`/`plan_child`/`plan_element` thread an `agg_contribs: Vec<Vec<Option<Value>>>` accumulator alongside `actions`/`emits`, and `run_foreach` folds it into one final `Emit::Text` line (bare comma-joined values, no XML) after the loop finishes. `COUNT(*)` is the one aggregate whose argument is the literal `Expr::Star` instead of an expression — it counts every reached row unconditionally. A pure-aggregate `OUTPUT` must be the loop's only `OUTPUT` (checked by `aggregate_output`) — mixing it with plain attributes/expressions, or with another `OUTPUT`, is a hard error.
- `SELECT (GROUP|TAG|ROOT) ... AS alias` is display-only: the alias overrides only the outermost tag passed into the serializer (`serialize_subtree_as_into`/`render_emits`'s `alias` param) — the DOM itself is never touched, and descendants keep their real tags. `RENAME GROUP g AS new_tag` / `RENAME TAG t AS new_tag` are the permanent counterpart — they resolve the target the same way `DELETE GROUP`/`DELETE TAG` do, then mutate `Element::tag` in place. Don't conflate the two: `AS` alone never mutates.
