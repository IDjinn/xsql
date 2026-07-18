# xsql

SQL-like language for querying and mutating XML files. Built for automated
editing toolchains: reads a script (or inline query), applies it to XML
documents in memory, and writes the result to stdout — a well-behaved Unix
filter.

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

In the REPL, statements run when the `;` terminator closes them (multi-line
input supported); `USE` and in-memory edits persist between statements.
`.dump` prints the modified documents, `.help` shows usage, `exit` leaves —
emitting any pending edits to stdout on the way out.

Output always goes to stdout: `SELECT` results are printed immediately;
every document a script *modified* is serialized (pretty-printed) after the
script finishes. Errors go to stderr with `file:line:col` diagnostics and a
non-zero exit code.

## Language

`;` terminates a statement block — and everything after `;` on that line is
ignored, so `; free text` lines double as comments.

```sql
; USE is sticky: it applies to every following block until the next USE.
USE database.local.xml

; strip attributes from every element of a group
FOREACH arm IN arms
    DELETE IGNORE arm.unlock_civi_science
    DELETE IGNORE arm.science
;

; replace a group's children wholesale
REPLACE GROUP new_continued_cost
RAW XML `
<ItemSpec id="52034301" level="1" cost="500"/>
<ItemSpec id="52034302" level="2" cost="1000"/>
`;

; query: print matching elements (does not modify the file)
SELECT GROUP goods
FOREACH good IN goods
    WHERE goods.id > 52034301
;

; OUTPUT picks which attributes are printed (like SQL's column list);
; without OUTPUT the whole element prints — `OUTPUT *` is the implicit default
SELECT GROUP goods
FOREACH good IN goods
    WHERE good.cost > 500
    OUTPUT good.id, good.cost, good.cost * 2 AS double_cost
;
SELECT GROUP goods OUTPUT id, level;    without FOREACH: loops implicitly

; OUTPUT in a nested loop joins scopes into one flat row
FOREACH office IN offices
    FOREACH member IN office
        WHERE member.cost > 5
        OUTPUT office.id AS office, member.name, member.cost
;

; edit exactly one element
FOREACH office IN office
    WHERE office.id = 216000
    SET office.name = "New Office Name"
    BREAK;
;

; upsert (idempotent): match children by id (then name, then tag);
; matched -> cited attributes updated, the rest preserved; no match -> inserted
MERGE INTO GROUP goods
RAW XML `
<ItemSpec id="52034301" cost="600"/>
<ItemSpec id="52034999" level="9" cost="9000"/>
`;

; add an attribute only where missing — existing values win, even different ones
FOREACH arm IN arms
    MERGE arm.tier = 1
;

; WHERE works on any attribute; a missing attribute evaluates as null,
; so the element is silently skipped. WHERE REQUIRED makes the attribute
; mandatory instead: an element without it aborts the run with an error.
SELECT GROUP goods
FOREACH good IN goods
    WHERE REQUIRED level >= 2
;

; nested FOREACH: iterate the current element's children (IN <outer var>),
; or a group found inside the current element (IN <subgroup name>);
; inner loops can read and write outer scopes (office.total below)
FOREACH office IN offices
    WHERE office.id = 216000
    FOREACH member IN office
        SET member.reviewed = 1
        SET office.total = office.total + member.cost
;

; more verbs
INSERT INTO GROUP goods RAW XML `<ItemSpec id="999"/>`;
DELETE GROUP legacy_stuff;

; global settings (any script position, apply to the whole run)
SET FORMAT = OFF;          compact single-line XML output
SET IGNORE_COMMENTS = OFF; preserve XML comments in loaded documents
ANALYZE;                   print per-stage timings to stderr
```

### Reference

| Construct | Meaning |
|---|---|
| `USE <path>` / `USE "path"` / `USE INPUT` | selects the current document (`INPUT` = XML piped on stdin); sticky until the next `USE` |
| `SELECT GROUP g [FOREACH ...]` | prints the group, or the elements passing the loop's `WHERE`s |
| `REPLACE GROUP g RAW XML \`...\`` | replaces the group's children with the fragment |
| `INSERT INTO GROUP g RAW XML \`...\`` | appends the fragment to the group |
| `MERGE INTO GROUP g RAW XML \`...\`` | upsert (idempotent): matches children by `id`, then `name`, then tag; matched → cited attributes updated (rest preserved; fragment children, when present, replace the existing ones), unmatched → inserted |
| `DELETE [IGNORE] GROUP g` | removes the whole group element |
| `FOREACH v IN g <ops>` | iterates the group's direct children |
| `FOREACH v2 IN v <ops>` (nested) | iterates the current element's children (`v` = an enclosing loop variable) or a group found inside the current element; ops after a nested loop belong to it, and a `BREAK` inside it only ends the inner loop |
| `WHERE <expr>` | guard: skips remaining ops for non-matching elements (a missing attribute participates as null) |
| `WHERE REQUIRED attr <cond>` | like WHERE, but the attribute is mandatory: an element without it is a hard error (plain WHERE skips it silently) |
| `SET v.attr = <expr>` | writes an attribute |
| `MERGE v.attr = <expr>` | writes the attribute only when missing; an existing value wins, even a different one (idempotent) |
| `OUTPUT <expr> [AS name], ...` | emission point: prints a flat element with only the cited attributes/expressions, evaluated when execution reaches it (missing attributes are omitted; non-attribute expressions need `AS`); works in SELECT loops, plain FOREACH loops and nested loops (mixing scopes into one row) |
| `OUTPUT *` | prints the whole current element — the implicit default of a SELECT loop without OUTPUT |
| `DELETE [IGNORE] v.attr` | removes an attribute (`IGNORE`: no error if absent) |
| `DELETE v` | removes the current element |
| `BREAK` | stops the loop |
| `SET <setting> = ON\|OFF` | global setting (script scope, needs no `USE`); persists across REPL statements |
| `ANALYZE` | shorthand for `SET ANALYZE = ON`: per-stage timing report on stderr |

Settings (values `ON`/`OFF`, also `TRUE`/`FALSE`/`1`/`0`; names case-insensitive):

| Setting | Default | Effect |
|---|---|---|
| `FORMAT` | `ON` | `ON`: pretty-printed XML output; `OFF`: compact, one line per document/selected element |
| `IGNORE_COMMENTS` | `ON` | `ON`: XML comments are dropped when parsing; `OFF`: comments are preserved and re-emitted (they are never loop elements) |
| `ANALYZE` | `OFF` | `ON`: after the run, prints a per-stage report to stderr — lex, parse, stdin/file read, XML parse per document, per-block execution, serialization, output assembly, stdout write, total DOM memory, total time |

A *group* is the first element whose tag — or `name`/`id` attribute — equals
the given name. Group names may be identifiers (`arms`), numbers
(`SELECT GROUP 110000` — matches `<Group id="110000">`), or quoted strings
(`SELECT GROUP "Group"` — for names that collide with keywords). Inside a loop, the loop variable, the group name and a bare
attribute name all refer to the current element (`good.id`, `goods.id` and
`id` are interchangeable). In nested loops the enclosing variables stay
visible — the innermost binding wins — so an inner loop can read and write
attributes of its outer elements.

Expressions: `= != <> < > <= >= AND OR NOT + - * /`, numbers, `"strings"`,
attribute references. Values are dynamically typed — numeric semantics apply
whenever both operands parse as numbers; `+` on non-numbers concatenates.
Missing attributes compare as false (SQL-NULL-ish).

## Performance

- Hand-rolled single-pass lexer and recursive-descent parser; arena-based
  DOM (flat `Vec`, index links) on top of quick-xml.
- Rayon parallelism: documents load/parse in parallel, blocks for different
  documents execute in parallel, and large `FOREACH` loops (≥1024 children,
  no `BREAK`) evaluate expressions in parallel before applying mutations
  sequentially — exact sequential semantics, parallel speed.

## Workspace

- `xsql/` — the library + binary
- `xsql-tests/` — all tests (lexer, parser, DOM, interpreter, CLI end-to-end)
  and fixtures; run `cargo test` from the workspace root

## Roadmap

- Preserve original formatting on output (attribute order is kept; comments
  are kept with `SET IGNORE_COMMENTS = OFF`; whitespace layout is not).
