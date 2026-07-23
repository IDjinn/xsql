---
name: xsql
description: Write and run xsql scripts to query or mutate XML files with this project's xsql CLI. Use whenever the task is "read/filter/update/delete data in an XML file" and an xsql binary or the xsql source (this repo) is available — instead of hand-rolling XML parsing/editing code.
---

# xsql — query and mutate XML with a SQL-like language

xsql is a Unix-filter CLI: it reads a script, applies it to XML document(s)
in memory, and writes the result to stdout. Use it instead of writing
ad-hoc XML-parsing code whenever the task is "look at this XML" or "change
these attributes/elements in this XML file."

## 0. Get a binary

```bash
cargo build --release        # -> target/release/xsql
# or, for quick iteration without a release build:
cargo run --quiet -- -e "..."
```

If the target repo already ships a compiled `xsql`/`xsql.exe`, use that
instead of rebuilding.

## 1. Always inspect before you mutate

Never write a mutating script blind. First run a **read-only** query to
confirm the document's actual shape (group/container names, tag names,
attribute names) — XML in the wild rarely matches assumptions:

```bash
xsql -e 'USE path/to/file.xml SELECT GROUP the_group;'
xsql -e 'USE path/to/file.xml SELECT TAG SomeTag;'      # any depth, no assumed container
```

If you don't know the container name, dump the whole document (no `USE`
verb needed beyond selecting the file) or grep the raw XML first.

## 2. Write the mutation, test it, then commit it to disk

xsql never edits the file in place — it prints the fully re-serialized
document to stdout. The safe loop:

```bash
xsql -e 'USE file.xml UPDATE goods SET cost = cost * 2 WHERE cost > 500;' > /tmp/out.xml
diff file.xml /tmp/out.xml     # eyeball the change before trusting it
mv /tmp/out.xml file.xml       # only after the diff looks right
```

Do **not** pipe straight to the source path (`xsql ... > file.xml`) without
reviewing output first — a bad `WHERE` or a typo'd attribute name silently
produces a document you didn't intend, and the original is gone the moment
the shell truncates the file.

`SET IGNORE_COMMENTS = OFF;` before `USE` if the document has comments you
need to preserve — the default drops them on parse.

## 3. Prefer idempotent verbs for anything that might re-run

- `MERGE`/`MERGE ... SET`/`MERGE INTO GROUP` write only where a value is
  **missing** — safe to run twice, safe in a retry loop. Use this over
  `SET`/`UPDATE` whenever the script might execute more than once against
  the same document (CI, automation, "just in case" reruns).
- `UPDATE`/`SET` always overwrite — use them only for genuinely
  unconditional changes.
- `DELETE IGNORE` / `DELETE FROM ... WHERE` over plain `DELETE` when the
  target might legitimately already be absent.

## 4. Use WHERE REQUIRED to catch schema drift instead of silently skipping

Plain `WHERE attr <cond>` treats a missing attribute as false and skips the
element quietly — fine for optional fields, dangerous for fields you
assumed always exist. If an attribute *should* always be present, guard
with `WHERE REQUIRED attr <cond>` so a malformed element is a hard error
instead of a silently-wrong result.

## 5. GROUP vs TAG — pick deliberately, don't guess

- `GROUP g` — first element whose tag/`name`/`id` equals `g`; operates on
  its direct children. Use for documents with a real container structure.
- `TAG t` — every element with tag `t`, at any depth, regardless of
  attributes. Use for flat/irregular documents with no reliable container.

Guessing wrong silently returns zero rows (typical failure: using `GROUP`
on a document that has no such container) — if a query returns nothing,
try the other selector before assuming the data isn't there.

## 6. Quick syntax cheat sheet

```sql
; `;` ends a block; everything after `;` on that line is a comment
USE file.xml                                    ; sticky until next USE
USE INPUT                                       ; XML piped on stdin (filter mode)

SELECT GROUP g;                                 ; print a group as-is
SELECT GROUP g OUTPUT id, level;                ; implicit loop, chosen columns
SELECT TAG t;                                   ; every <t> anywhere

FOREACH v IN g
    WHERE v.attr > 5
    OUTPUT v.id, v.attr
;

FOREACH v IN g WHERE v.id = 1 SET v.name = "x" BREAK; ;   ; edit exactly one

UPDATE g SET a = e, ... WHERE cond LIMIT 1;     ; sugar for the FOREACH above
MERGE g SET a = e WHERE cond;                   ; idempotent UPDATE
DELETE FROM g WHERE cond;                        ; removes matches, container stays
DELETE GROUP g;                                  ; removes the container itself
DELETE TAG t;                                    ; removes every <t>

REPLACE GROUP g RAW XML `<Item id="1"/>`;        ; wholesale replace children
INSERT INTO GROUP g RAW XML `<Item id="2"/>`;    ; append
MERGE INTO GROUP g RAW XML `<Item id="1" cost="9"/>`;  ; upsert by id/name/tag

SET FORMAT = OFF;            ; compact output
SET IGNORE_COMMENTS = OFF;   ; keep XML comments
ANALYZE;                     ; per-stage timing to stderr
```

Expressions: `= != <> < > <= >= AND OR NOT + - * /`, numbers, `"strings"`,
attribute refs. Numeric semantics when both sides parse as numbers, `+`
concatenates otherwise, missing attribute = false in `WHERE` (but omitted,
not blank, in `OUTPUT`).

Full reference: this repo's `README.md`, or the generated docs site under
`docs/` (`/docs/language/reference`).

## 7. Verify with the test suite when changing xsql itself

If the task is modifying xsql's own source (not just using it as a tool):

```bash
cargo test --test parser        # lexer|parser|xml|eval|cli
cargo test --test eval test_name
```

`cli.rs` tests drive the compiled binary end-to-end and need a debug build
(`cargo test` builds it automatically).
