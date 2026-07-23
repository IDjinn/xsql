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

; the same edit, MySQL style: UPDATE is sugar for the loop above
; (LIMIT 1 = stop after the first match, like BREAK)
UPDATE office SET name = "New Office Name" WHERE id = 216000 LIMIT 1;

; without WHERE/LIMIT it updates every element of the group;
; assignments take full expressions and multiple targets
UPDATE goods SET cost = cost * 2, level = level + 1 WHERE cost > 500;

; MERGE shorthand: write only where the attribute is missing
MERGE arms SET tier = 1;

; DELETE FROM removes matching elements (DELETE GROUP removes the container)
DELETE FROM goods WHERE cost > 500;

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

; TAG selects by tag name instead of by group: it matches EVERY element
; with that tag, wherever it sits — for documents without a regular
; group/container structure. GROUP matches one container (by tag, `name`
; or `id`) and iterates its children; TAG iterates the matches themselves.
SELECT TAG ItemSpec;                            all ItemSpec elements, any depth
FOREACH i IN TAG ItemSpec SET i.reviewed = 1;   document-wide edit
UPDATE TAG ItemSpec SET cost = 0 WHERE cost < 0;
DELETE TAG Obsolete;                            removes every <Obsolete> element
; nested: IN TAG searches only the current element's subtree
FOREACH office IN offices
    FOREACH m IN TAG Member
        SET m.office = office.id
;

; ROOT is like TAG but with no tag filter at all: every element in the
; document, any tag — for when you don't know (or don't want to name) the
; tag ahead of time. Same document-wide/nested-subtree rule as TAG.
FOREACH v IN ROOT WHERE v.type = 0 OUTPUT v.id;
UPDATE ROOT SET reviewed = 1 WHERE cost > 500;

; aggregate OUTPUT: COUNT/MIN/MAX/SUM/AVG summarize the whole loop into one
; row of bare numbers instead of one row per element — no XML wrapper.
; A pure-aggregate OUTPUT must be the loop's only OUTPUT (no mixing with
; plain attributes/expressions); zero contributing rows reports 0.
FOREACH v IN ItemSpec WHERE v.type = 0 OUTPUT COUNT(v.id);              ; -> 2
FOREACH v IN ItemSpec WHERE v.type = 0
    OUTPUT MIN(v.cost), MAX(v.cost), SUM(v.cost), AVG(v.cost)
;                                                                        ; -> 10,30,40,20

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
| `DELETE [IGNORE] TAG t` | removes every element with tag `t` (`IGNORE`: no error when none match) |
| `SELECT TAG t [FOREACH ...\|OUTPUT ...]` | prints every element with tag `t`, wherever it sits |
| `SELECT ROOT [FOREACH ...\|OUTPUT ...]` | prints every element in the document, any tag |
| `UPDATE (TAG t \| ROOT \| [GROUP] g) SET a = e, ... [WHERE expr] [LIMIT 1]` | MySQL-style sugar for a FOREACH: guard, then the SETs; `LIMIT 1` = BREAK after the first match |
| `MERGE (TAG t \| ROOT \| [GROUP] g) SET a = e, ... [WHERE expr] [LIMIT 1]` | same, but attributes are written only where missing (idempotent) |
| `DELETE FROM (TAG t \| ROOT \| [GROUP] g) [WHERE expr] [LIMIT 1]` | removes the matching elements (the container survives) |
| `FOREACH v IN g <ops>` | iterates the group's direct children |
| `FOREACH v IN TAG t <ops>` | iterates every element with tag `t` — document-wide at top level, within the current element's subtree when nested (zero nested matches iterate nothing) |
| `FOREACH v IN ROOT <ops>` | iterates every element regardless of tag — document-wide at top level, within the current element's subtree when nested; for when the tag isn't known ahead of time |
| `FOREACH v2 IN v <ops>` (nested) | iterates the current element's children (`v` = an enclosing loop variable) or a group found inside the current element; ops after a nested loop belong to it, and a `BREAK` inside it only ends the inner loop |
| `WHERE <expr>` | guard: skips remaining ops for non-matching elements (a missing attribute participates as null) |
| `WHERE REQUIRED attr <cond>` | like WHERE, but the attribute is mandatory: an element without it is a hard error (plain WHERE skips it silently) |
| `SET v.attr = <expr>` | writes an attribute |
| `MERGE v.attr = <expr>` | writes the attribute only when missing; an existing value wins, even a different one (idempotent) |
| `OUTPUT <expr> [AS name], ...` | emission point: prints a flat element with only the cited attributes/expressions, evaluated when execution reaches it (missing attributes are omitted; non-attribute expressions need `AS`); works in SELECT loops, plain FOREACH loops and nested loops (mixing scopes into one row) |
| `OUTPUT *` | prints the whole current element — the implicit default of a SELECT loop without OUTPUT |
| `OUTPUT COUNT(expr) \| MIN(expr) \| MAX(expr) \| SUM(expr) \| AVG(expr), ...` | aggregate: summarizes every element the loop reaches into one comma-joined line of plain numbers (no XML, no per-element rows). Must be the loop's only OUTPUT; `COUNT` counts non-null values, the rest ignore non-numeric/missing values; zero contributing rows reports `0` |
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
the given name. A *tag* selector (`TAG t`) instead matches **all** elements
whose tag equals `t` — attributes don't participate — at any depth; use it
when the document has no reliable group structure. `ROOT` goes one step
further: no tag filter at all, every element in the document (or, nested,
every element in the current element's subtree) — use it when you don't
know the tag name up front, e.g. a first exploratory pass over an unfamiliar
file. Group names may be identifiers (`arms`), numbers
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

Aggregate functions (`COUNT`, `MIN`, `MAX`, `SUM`, `AVG`) are only valid as a
direct `OUTPUT` item — `OUTPUT COUNT(v.id)`, not `WHERE COUNT(v.id) > 0`.
They replace the loop's usual one-row-per-element `OUTPUT` with a single
line of comma-joined plain numbers, computed over every element the loop
reaches (after `WHERE`).

## Performance

- Hand-rolled single-pass lexer and recursive-descent parser; arena-based
  DOM (flat `Vec`, index links) on top of quick-xml.
- Rayon parallelism: documents load/parse in parallel, blocks for different
  documents execute in parallel, and large `FOREACH` loops (≥1024 children,
  no `BREAK`) evaluate expressions in parallel before applying mutations
  sequentially — exact sequential semantics, parallel speed. TAG and ROOT
  loops parallelize only when no match is nested inside another.
- Output strings are pre-sized from the DOM (serializer and final output
  assembly), avoiding repeated reallocation on large documents.

## Workspace

- `xsql/` — the library + binary
- `xsql-tests/` — all tests (lexer, parser, DOM, interpreter, CLI end-to-end)
  and fixtures; run `cargo test` from the workspace root

## Roadmap

- Preserve original formatting on output (attribute order is kept; comments
  are kept with `SET IGNORE_COMMENTS = OFF`; whitespace layout is not).
- More aggregate functions: `FIRST`/`LAST` (first/last matching element's
  value), `CONCAT`/`GROUP_CONCAT` (join values with a separator), a distinct
  variant of `COUNT`. `GROUP BY`-style multi-row aggregation (one aggregate
  row per distinct value of some attribute, instead of one row for the whole
  loop) is a bigger, separate feature.
