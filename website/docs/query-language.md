---
title: AikoQL Query Language
description: The complete AikoQL (KOQL) query reference — statements, clauses, operators, and examples
---

# AikoQL Query Language (KOQL)

AikoQL (colloquially **KOQL** — the Knowledge Object Query Language) is the
query language of the aikoql knowledge database. It is small by design: five
statements, one comparison operator set, and a handful of clauses that compose
into knowledge-graph, temporal, and hybrid vector+text queries.

This reference documents the grammar **as the parser accepts it today**
(`crates/compiler/src/parser`), not the aspirational syntax of older design
docs. Every example here parses.

## Running a query

Queries run in three places — the shell, the MCP `aikoql` tool, and the SDKs:

```bash
# interactive shell
aikoql-mcp shell ./kb
aikoql> MATCH note RETURN *
```

```python
# Python SDK
from aikoql import Agent
db = Agent.connect("./kb")
db.aikoql("MATCH note RETURN *")
```

```json
// MCP tool call (any language)
{ "name": "aikoql", "arguments": { "query": "MATCH note RETURN *" } }
```

The Studio UI (`http://127.0.0.1:9091/studio`) has a Query Editor panel for
the same language, with `explain` plan output.

## Lexical conventions

| Element | Rule | Example |
|---|---|---|
| Keywords | Case-insensitive; full word must match | `MATCH`, `match`, `Match` |
| Identifiers | Start with a letter or `_`; then letters, digits, `_`, `:` | `name`, `aikoql:document` |
| Strings | Double quotes only; **no escape sequences** — the first `"` ends the string | `"hello"`, `"C:\temp"` |
| Numbers | Digits with at most one decimal point; **no sign, no exponent** | `35`, `1.5` |
| Booleans | `true` / `false` — **lowercase only** | `WHERE active == true` |
| Null | `null` — lowercase only | `WHERE owner == null` |
| Comments | `//` to end of line; no `/* */` | `// a note` |

Identifiers preserve case. There is **no quoting mechanism for identifiers** —
a property whose name collides with a keyword (e.g. `count`, `explain`,
`order`) cannot be referenced by name.

## Values and expressions

Values are one of: string, number, boolean, null. There are no lists, no
function calls, and no property paths (`a.b` does not parse).

### Comparison operators

| Operator | Meaning | Example |
|---|---|---|
| `==` | equal | `WHERE dept == "Engineering"` |
| `!=` | not equal | `WHERE status != "closed"` |
| `>` | greater | `WHERE score > 0.8` |
| `<` | less | `WHERE age < 30` |
| `>=` | greater or equal | `WHERE retries >= 3` |
| `<=` | less or equal | `WHERE temp <= 35` |

A single `=` is a parse error (`AIKOQL1013`). There is no `LIKE`, no `IN`,
no `IS NULL`, and no regex operator — a property compared to `null` with
`==` / `!=` is the null check.

### Boolean operators

`AND` and `OR` combine comparisons. SQL precedence applies: **`AND` binds
tighter than `OR`**. There is no `NOT` and no parentheses.

```aikoql
MATCH Person
WHERE company == "Visa" AND city == "Amsterdam" OR role == "intern"
RETURN *
```

Reads as `(company == "Visa" AND city == "Amsterdam") OR role == "intern"`.

## MATCH

The workhorse. Scan a type, filter it, optionally add temporal, search,
graph, and aggregation clauses, then project.

```text
MATCH <type> [clauses...] RETURN <projection>
```

Clauses may appear in **any order** before `RETURN`. Each clause may appear
at most once, except `WHERE` (predicates accumulate). A bare `AND` / `OR`
at clause position also adds a predicate, so
`MATCH X AND a == 1 RETURN *` is legal.

| Clause | Grammar | Purpose |
|---|---|---|
| `WHERE <predicate>` | boolean expression, repeatable | filter by property |
| `SIMILAR TO "<text>" [SCORE BM25] [USING EMBEDDING]` | one query string | lexical / vector search |
| `TRAVERSE <relation> [DEPTH <n>]` | relation name; DEPTH defaults to 1 | follow graph edges |
| `AS_OF <t>` | epoch millis or strict ISO string | state as of a moment |
| `AS_OF JOURNAL <n>` | epoch millis integer | state as of a journal sequence |
| `BETWEEN <t> AND <t>` | from **<** to, else `AIKOQL1015` | versions valid in a window |
| `HISTORICAL` | no argument | every committed version |
| `CHANGESET BETWEEN <t> AND <t>` | from < to | mutations in a window |
| `EPISTEMIC <status> (, <status>)*` | e.g. `verified, asserted` | only given epistemic states |
| `SOURCE "<id>"` | provenance id string | only knowledge from a source |
| `GROUP BY <key>, <agg> (, ...)*` | see aggregates | group rows |
| `ORDER BY <key> [ASC\|DESC] (, ...)*` | ASC default | sort results |
| `[LEFT] JOIN <type> ON <l> == <r>` | equality only | join two types |
| `LIMIT <n> [OFFSET <m>]` | non-negative integers | page results |

### RETURN projections

```text
RETURN * | explain | field (, field)*
```

- `*` — every property of every matched object.
- `explain` — the query plan instead of results (the plan is also available
  from the Studio explain view and the HTTP explain endpoint).
- A field list — the named properties only. **No aliases** (`AS x` does not
  parse — see [Known limits](#known-limits)).

### WHERE

```aikoql
MATCH Person WHERE company == "Visa" RETURN *
MATCH Ticket WHERE status != "closed" AND priority > 2 RETURN *
MATCH Sensor WHERE temp <= 35 OR active == false RETURN *
MATCH Task WHERE owner == null RETURN *
```

### SIMILAR TO

Text search, vector search, or both fused. Modes are additive — nothing
switches silently:

| Query | Retrieval |
|---|---|
| `SIMILAR TO "x"` | lexical (Jaccard text search) — the deterministic default; embeddings are ignored even if indexed |
| `SIMILAR TO "x" SCORE BM25` | lexical (Tantivy BM25; Jaccard fallback when the maintainer is not attached) |
| `SIMILAR TO "x" USING EMBEDDING` | vector (embedding index over the query text) |
| `SIMILAR TO "x" SCORE BM25 USING EMBEDDING` | hybrid fusion — both legs run and merge with RRF |

`SCORE BM25` and `USING EMBEDDING` may appear in either order. Asking for
`USING EMBEDDING` on a store with no embeddings fails rather than silently
answering text-side. When a temporal or `EPISTEMIC` clause is present,
`USING EMBEDDING` is dropped and the query runs relationally.

```aikoql
MATCH Article SIMILAR TO "tokenization" RETURN *
MATCH Article SIMILAR TO "tokenization" SCORE BM25 LIMIT 10 RETURN *
MATCH Article SIMILAR TO "tokenization" SCORE BM25 USING EMBEDDING RETURN *
```

### TRAVERSE

Follow a named relationship from each matched object, `DEPTH` hops deep
(default 1).

```aikoql
MATCH Service TRAVERSE DEPENDS_ON DEPTH 2 RETURN *
MATCH Person SIMILAR TO "John" TRAVERSE managed_by RETURN *
```

### Temporal clauses

Exactly one temporal clause per query. Times are written in exactly two
spellings — an epoch-millis integer or a strict ISO instant (UTC):

| Spelling | Accepted | Example |
|---|---|---|
| Epoch millis | non-negative integer, no fraction | `AS_OF 1791582896539` |
| ISO date | `YYYY-MM-DD` (UTC) | `AS_OF "2026-10-09"` |
| ISO datetime | `YYYY-MM-DDTHH:MM:SS` (UTC) | `AS_OF "2026-10-09T05:15:32"` |

Rejected at parse time (`AIKOQL1011`): fractional seconds, timezone
offsets/suffixes (`+00:00`, `Z`), years outside 1970–9999, out-of-range
fields.

```aikoql
MATCH Employee AS_OF 1724025600000 RETURN *        -- what was true then
MATCH Employee AS_OF JOURNAL 1791582896539 RETURN * -- pinned to a journal seq
MATCH Config BETWEEN 1724025600000 AND 1730000000000 RETURN *
MATCH Employee HISTORICAL RETURN *                -- every committed version
MATCH Service CHANGESET BETWEEN 1724025600000 AND 1730000000000 RETURN *
```

`BETWEEN` requires from **strictly before** to; the window enumerates
closed generations.

### EPISTEMIC and SOURCE

```aikoql
MATCH Fact EPISTEMIC verified, asserted RETURN *
MATCH Doc SOURCE "fixture.md" RETURN *
```

`EPISTEMIC` keeps only knowledge whose current epistemic status is one of
the listed states. `SOURCE` keeps only knowledge whose provenance chain
traces to the given source id.

### GROUP BY

Group rows by keys and/or aggregate. Aggregates are `COUNT`, `SUM`, `AVG`,
`MIN`, `MAX`; only `COUNT` takes `*`.

```aikoql
MATCH Ticket GROUP BY status, COUNT(*) RETURN *
MATCH Reading GROUP BY sensor, AVG(temp), MAX(temp) RETURN *
```

`ORDER BY count` is a special case: the keyword `count` is accepted as a
sort key so aggregate output can be ordered by its count column.

```aikoql
MATCH Ticket GROUP BY status, COUNT(*) ORDER BY count DESC RETURN *
```

### ORDER BY

```aikoql
MATCH Employee ORDER BY salary DESC RETURN name, salary
MATCH Log ORDER BY ts, level RETURN *
```

### JOIN

One join per query; inner by default, `LEFT` prefix for a left join; only
`==` in `ON`.

```aikoql
MATCH Person JOIN Company ON employer == name RETURN *
MATCH Person LEFT JOIN Company ON employer == name RETURN *
```

### LIMIT / OFFSET

```aikoql
MATCH Log LIMIT 50 RETURN *
MATCH Log LIMIT 50 OFFSET 100 RETURN *
```

`OFFSET` requires a preceding `LIMIT`. Both must be whole non-negative
numbers.

## Indexes

AikoQL has **no index DDL** — there is no `CREATE INDEX` statement, and a
query never names an index. Index choice is a planner decision
(`crates/compiler/src/planner.rs`): each scan operator maps to a storage
strategy — `Scan → FullScan`, `AnnSearch → VectorIndex`,
`TextSearch → TextIndex`, everything else `Inline` — so
`USING EMBEDDING` transparently runs on the HNSW vector index and
`SCORE BM25` on the text index.

Property indexes are declared out of band, on the tool surface rather than
in the language:

- MCP tool `index_create` — declare a property index and analyze behind it
- CLI `aikoql index [DB]` — storage/index statistics

## CREATE

Create one knowledge object. Zero or more property assignments; properties
are double-quoted-string, number, boolean, or null values.

```text
CREATE <type> [property == value (, property == value)*]
```

```aikoql
CREATE note body == "hello"
CREATE service name == "settlement" owner == "Payments Team" tier == 1
CREATE sensor temp == 35.5 active == true
```

## UPDATE

Update one object, addressed by type + KOID string.

```text
UPDATE <type> "<koid>" [property == value (, property == value)*]
```

```aikoql
UPDATE note "01a0ecc867810000000000000000a9c9" body == "hello, world"
```

## DELETE

Delete one object by type + KOID string.

```text
DELETE <type> "<koid>"
```

```aikoql
DELETE note "01a0ecc867810000000000000000a9c9"
```

## INGEST

```text
INGEST "<source>" [EXTRACT tables] [EXTRACT entities] [BUILD relationships] [COMMIT]
```

```aikoql
INGEST "invoice.pdf"
INGEST "invoice.pdf" EXTRACT tables EXTRACT entities BUILD relationships COMMIT
```

`EXTRACT` takes `tables` or `entities`; `BUILD` takes `relationships`;
all flags and `COMMIT` are optional. Note: the parser records these flags
but execution currently ingests the source as one op — the per-stage
dispatch is not wired yet.

## Error codes

Diagnostics have the shape
`AIKOQL####: <message> at line L, column C. Hint: <hint>`.

| Code | Meaning |
|---|---|
| `AIKOQL1010` | unexpected token (unknown statement or misplaced keyword) |
| `AIKOQL1011` | expected a different token (incl. all temporal-literal rejections) |
| `AIKOQL1012` | unexpected end of input (`MATCH Person` with no `RETURN`) |
| `AIKOQL1013` | invalid comparison operator — hint names the six real ones |
| `AIKOQL1014` | invalid plan produced by the query (e.g. conflicting scan) |
| `AIKOQL1015` | invalid temporal range — `BETWEEN` needs from < to |
| `AIKOQL1030` | unknown type |
| `AIKOQL1031` | unknown property on a closed-schema type |
| `AIKOQL1033` | unknown relationship in `TRAVERSE` |
| `AIKOQL1034` | `TRAVERSE` DEPTH must be ≥ 1 |
| `AIKOQL1035` | security types (`aikoql:role`, `aikoql:policy`) cannot be queried |

## Known limits

These are the language's current edges — write queries accordingly:

- **No `NOT`, no `LIKE`, no `IN`, no `IS NULL`, no parentheses** in
  predicates. Combine `==`/`!=`/comparisons with `AND`/`OR` only.
- **No property paths** (`a.b`) and **no quoted identifiers** — property
  names colliding with keywords cannot be referenced.
- **No escape sequences in strings** — the first `"` ends the string.
- **No negative numbers, no exponents** — `-5` and `1e3` do not lex.
- **`true` / `false` / `null` are lowercase-only** as values.
- **No aliases in `RETURN`** (`name AS n` does not parse; a trailing alias
  after a statement is silently discarded — see below).
- **`HISTORICAL` takes no argument** (older docs show `HISTORICAL <t>`; the
  parser accepts the bare keyword only).
- **Trailing tokens are silently ignored** — the parser does not enforce
  end-of-input after a statement. `MATCH X RETURN * GARBAGE` parses; treat
  anything you write past the statement as untested.
- **`LIMIT` without `OFFSET`** is fine; `OFFSET` without `LIMIT` is an
  error.
- One temporal clause, one `JOIN`, one `SIMILAR`, one `TRAVERSE` per query
  (a repeated `SIMILAR`/`TRAVERSE` silently overwrites the earlier one).
