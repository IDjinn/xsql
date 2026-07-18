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

; edit exactly one element
FOREACH office IN office
    WHERE office.id = 216000
    SET office.name = "New Office Name"
    BREAK;
;

; more verbs
INSERT INTO GROUP goods RAW XML `<ItemSpec id="999"/>`;
DELETE GROUP legacy_stuff;
```

### Reference

| Construct | Meaning |
|---|---|
| `USE <path>` / `USE "path"` / `USE INPUT` | selects the current document (`INPUT` = XML piped on stdin); sticky until the next `USE` |
| `SELECT GROUP g [FOREACH ...]` | prints the group, or the elements passing the loop's `WHERE`s |
| `REPLACE GROUP g RAW XML \`...\`` | replaces the group's children with the fragment |
| `INSERT INTO GROUP g RAW XML \`...\`` | appends the fragment to the group |
| `DELETE [IGNORE] GROUP g` | removes the whole group element |
| `FOREACH v IN g <ops>` | iterates the group's direct children |
| `WHERE <expr>` | guard: skips remaining ops for non-matching elements |
| `SET v.attr = <expr>` | writes an attribute |
| `DELETE [IGNORE] v.attr` | removes an attribute (`IGNORE`: no error if absent) |
| `DELETE v` | removes the current element |
| `BREAK` | stops the loop |

A *group* is the first element whose tag — or `name`/`id` attribute — equals
the given name. Group names may be identifiers (`arms`), numbers
(`SELECT GROUP 110000` — matches `<Group id="110000">`), or quoted strings
(`SELECT GROUP "Group"` — for names that collide with keywords). Inside a loop, the loop variable, the group name and a bare
attribute name all refer to the current element (`good.id`, `goods.id` and
`id` are interchangeable).

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

- Flag to preserve comments, attribute order and original formatting on
  output (currently data-only pretty-print).
