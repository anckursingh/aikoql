# Data sources — discover external schemas, publish a canonical ontology

An operator names external databases in `aikoql.toml`. Studio's **Data Sources**
panel (or the REST routes below) then **discovers** each source's schema, records
it as versioned knowledge, **previews** an ontology with one class per table,
lets a person **merge** classes and **publish**, and optionally **imports**
chosen tables. After publishing, `MATCH Customer` reads every source mapped to
`Customer`.

Supported kinds: `postgres`, `mongodb`, `neo4j`, `sqlite`, `clickhouse`.

## Configure

```toml
[[sources]]
name = "crm"                 # [A-Za-z0-9_-]{1,64}, unique
kind = "postgres"            # postgres | mongodb | neo4j | sqlite | clickhouse
url_env = "CRM_DSN"          # the NAME of an environment variable that holds the connection string
# database = "analytics"     # required for mongodb and clickhouse
# tenant = "acme"            # the source belongs to one tenant; omit for the global scope
# sample_limit = 100         # records sampled per container for schemaless kinds (1-10000)
# max_containers = 500       # cap on tables/collections/labels read (1-100000)
# timeout_secs = 15          # per call (1-600)
# import_max_rows = 10000    # per container, per import (1-100000000)
```

Connection strings per kind (always read from the environment variable named by `url_env`):

| kind | value |
|---|---|
| postgres | `host=db user=ro password=… dbname=crm` |
| mongodb | `mongodb://user:pass@host:27017` (+ `database = "…"`) |
| neo4j | `http://user:pass@host:7474` |
| sqlite | a file path |
| clickhouse | `http://user:pass@host:8123` (+ `database = "…"`) |

Unknown keys are rejected. With no `[[sources]]`, nothing changes and Studio says
"No sources configured".

## Security model

- **Operator-defined hosts.** Studio users pick a source *by name*; they can never choose a host,
  port or path, so the server cannot be steered at internal addresses (no SSRF).
- **No secrets stored or shown.** The connection string lives only in the environment. It is never
  written to a knowledge object, the audit log or the server log, and never appears in a response or
  an error message (`source <name>: <short reason>`; passwords and URLs are scrubbed).
- **Read-only.** SQLite is opened `READ_ONLY` (the file is byte-identical afterwards), the Postgres session
  is set read-only, ClickHouse requests carry `readonly=1`. Use a read-only database account anyway:
  MongoDB and Neo4j cannot be forced read-only from the client.
- **Admin only.** Every sources route, listing included, needs the `admin` role in multi-user mode. A tenant's sources are invisible to other tenants; a missing and a foreign source answer
  identically (`404 source not found`).
- **Bounded.** Every call has a deadline. A source that accepts a connection and then goes silent fails
  at its timeout. The driver also stops its own work just inside that limit (a SQLite interrupt, a MongoDB
  operation deadline, PostgreSQL `statement_timeout`, ClickHouse `max_execution_time`, the Neo4j request
  timeout), so the thread returns and its slot frees. As a backstop the call runs behind a gate: at most 8
  abandoned calls may be stuck at once, after which new calls are refused until they clear. Containers are capped at
  `max_containers`; imports at `import_max_rows`.
- **Audited.** Each call writes one id-only line (`tool: sources`): source, outcome, fingerprint prefix.

## What discovery records

Per source, as ordinary knowledge objects in the source's tenant (queryable, versioned, `AS_OF`-able):

| type | one per | holds |
|---|---|---|
| `aikoql:source` | source | name, kind, schema fingerprint, container count, references |
| `aikoql:source-table` | container | fields (type, nullability, keys, provenance), unique groups, schema namespace, **indexes**, **CHECK constraints**, view kind, storage properties |
| `aikoql:schema-run` | change | fingerprint, previous fingerprint, the diff |

What each kind reports beyond columns and keys (all part of the schema fingerprint and the diff):

| kind | indexes | constraints | other |
|---|---|---|---|
| postgres | every secondary index, with method, uniqueness, key columns or expressions, and a partial index's predicate | CHECK constraints (named); a full unique index on plain columns counts as a uniqueness guarantee, an expression or partial one is recorded but never claimed | schema namespace |
| neo4j | property indexes (range, text, point, vector, fulltext) | uniqueness and node-key (unique groups), existence and node-key (mandatory properties) — best effort: older servers or a role without `SHOW` privileges report none | |
| clickhouse | data-skipping indexes | — | engine, sorting / primary / partition key, views and materialized views with their defining query and `derived_from` lineage to the tables they read; a materialized view's hidden `.inner*` storage table is not a container |
| mongodb | — | — | observed field types as a union (`Int32|String`), presence and null ratios in `sample` mode |
| sqlite | unique indexes only | — | |

Re-running discovery with an unchanged schema writes **nothing**. A changed schema updates the affected
table objects in place (new version), tombstones removed containers (history kept) and adds a run with the
diff. Row counts are not part of the schema fingerprint.

`mode`: `"schema"` (default) reads catalogs only for SQL kinds; for MongoDB and Neo4j, which have no
catalog, it infers names and types from a small sample (≤20 records) and keeps no ratios. `"sample"`
samples `sample_limit` records and keeps a value profile (observed types, presence and null ratios).
Declared types (SQL, ClickHouse) and observed types (Mongo, Neo4j) are marked differently.

## From schema to ontology

`POST /api/v1/sources/ontology {sources, merges?, preview, fingerprints?, expected_version?, tenant?}`

- **One class per container** (singular PascalCase: `customers` → `Customer`), with typed properties,
  relationships from references, and a mapping from each physical container to its class.
- **No automatic cross-source merging.** The same class name in two sources stays two classes
  (`CrmCustomer`, `ShopCustomer`, flagged *possible duplicate*). A person merges concepts explicitly with
  `merges: [["crm.customers", "shop.clients"]]` → one class, two mappings. A merge can also align
  properties: `merges: [{"a": "crm.customers", "b": "shop.clients", "properties": [["email", "email_address"]]}]`
  folds `email_address` into the canonical `email`, so the canonical property answers in both databases.
  A fold that would make two columns of one container collide is refused with a warning.
- **Suggestions, never decisions.** A preview lists `suggestions`: pairs of containers from *different* sources
  that look like one concept (equal or synonym class names such as customer/client, aligned fields, key
  roles, value types), each with a `confidence`, the `evidence` behind it and the property matches that
  would need an alignment. A shared `id` alone is never evidence. Nothing is applied until a person sends
  it back as a merge (Studio: **Apply this merge**). Union types observed in a Mongo field (`Int32|String`)
  publish as `Text`, `Int|Double` as `Float`, with a warning.
- **Preview writes nothing.** It returns the classes, what would be added / refreshed / left untouched,
  warnings (type conflicts, name clashes, composite foreign keys, observed-only fields) and the schema
  `fingerprints` it was computed from.
- **Publish needs the fingerprints you reviewed** (else `409 schema changed since preview`) and, when a
  ontology is active, its `expected_version` (the preview's `active_version`; `base` says whether that is a
  `manual` or an `auto-discovered` ontology, whose classes are carried over). It merges into the tenant's ontology and
  **never changes what a human wrote**: existing classes, properties, relationships and mappings win;
  classes discovery itself created earlier are refreshed with new columns (a class counts as discovery's only if its
  description names containers that the ontology's mappings tie to that class; typing "Discovered from…" is not enough); identical content writes nothing.
- **Drift never rewrites the ontology.** A schema change after publishing is recorded and reported; the
  ontology stays as approved until someone previews and publishes again.

Querying: `MATCH Customer` expands into one plan per mapped container. `WHERE` property names are translated
per source through the mapping's `property_map` (physical column → canonical name; when several columns claim
one canonical name the lexicographically first wins). **Result rows keep each source's physical column names**
— there is no projection operator to rename them. Edit a mapping's `property_map` in the Ontology panel to map
`email_address` to `email`.

## Importing

`POST /api/v1/sources/import {source, containers}` copies chosen, already-discovered tables as objects typed by
the **physical** container name, tagged `src:<source>`.

- Supported for every kind. Values keep their exact form: Postgres `numeric`, timestamps, `uuid`, `json`, arrays
  and enums are read as text and parsed (a decimal past 15 significant digits stays exact text, never a rounded
  float; timestamps are UTC, `ISO` style); MongoDB keeps `_id` as a property and writes Decimal128 and dates
  as text; ClickHouse reads each cell by its declared type (UInt64 and 128-bit integers, decimals and `nan` as
  exact text).
- **Neo4j** is read as a graph: the nodes of the chosen labels (capped, in `elementId` order) and the
  relationships whose *both* ends were imported; a relationship's properties ride on its start node as
  `rel:<TYPE>` lists tagged with the target's KOID. A node with several labels is one object.
- **ClickHouse** is one `SELECT … ORDER BY <sorting key> LIMIT cap+1`, so the rows are a single consistent
  snapshot as of the query's start, streamed with row, per-row and total-size bounds. `ReplacingMergeTree` and
  `CollapsingMergeTree` tables are read `FINAL` (rows are their latest versions) and keyed by their sorting key;
  any other table's rows are identified by content (a 128-bit hash), so an updated row is a *new* object and
  identical rows collapse. A reply that is cut short or unparseable is an error, never a smaller table. Views
  import too; for a very large table raise `import_max_rows` knowingly.
- Idempotent: identical rows write nothing; a changed row updates in place.
- **Capped and synchronous.** Default 10 000 rows per container; the response says which containers were cut
  (`truncated`). Commits are per row (~270 rows/s on the v2 engine), so raise `import_max_rows` knowingly.
  Use the CLI for bulk loads.
- A type that already holds objects from another source, or from a CLI import, is **skipped with a reason**
  rather than overwritten (object identity is `(container, primary key)`).

## REST

| route | role | |
|---|---|---|
| `GET /api/v1/sources[?tenant=]` | admin | configured sources + last run |
| `POST /api/v1/sources/discover` `{source, mode?, tenant?}` | admin | read and record the schema |
| `POST /api/v1/sources/ontology` | admin | preview / publish |
| `POST /api/v1/sources/import` `{source, containers, tenant?}` | admin | copy tables |

Statuses: `400` invalid request / nothing to publish / not yet discovered, `404` source not found,
`409` stale preview or version conflict, `502` the source failed (`source <name>: …`).

## Testing it

The always-on suites use SQLite sources and need nothing else. The live cells run when the
`AIKOQL_TEST_*` variables are set (CI's `connectors` job provides Postgres, MongoDB, Neo4j and ClickHouse):
`AIKOQL_TEST_PG_DSN`, `AIKOQL_TEST_MONGO_URI`/`_DB`, `AIKOQL_TEST_NEO4J_URI`/`_USER`/`_PASSWORD`,
`AIKOQL_TEST_CLICKHOUSE_URL`/`_DB`.

`bash scripts/fuzz-sources.sh [iterations]` runs the seeded fuzz and adversarial suites at volume
(default 50 000). Every suite is seeded; a failure prints its seed. The adversarial capstone
(`tests/source_hardening.rs`) attacks the assembled server: a canary connection string swept through every
response, log and stored file; read-only proof; a 5 000-table source; tenant and role matrices; `SIGKILL`
mid-import then resume; a silent source; and route fuzz.

## Known limits

- Result rows are not renamed to canonical property names (see Querying).
- MongoDB and Neo4j schemas are samples: a field absent from the sample is not reported.
- The shared `id` property definition widens to `Text` when any source's key is not an integer (a Mongo
  ObjectId `_id`); queries still compare the stored values, so `WHERE id > 10` works on integer ids.
- Merge suggestions need a class-name signal or two shared exact field names to be considered, so two
  containers that only resemble each other through contained field names (and nothing else) are not offered.
- Imports of a Mongo collection whose `_id` is the string `"1"` in one document and the integer `1` in another
  collide on one KOID (the id's text is the identity).
- Postgres composite foreign keys become one relationship (with a warning); `FieldProfile` ratios for
  Neo4j are not available (the schema procedure reports types and "mandatory" only).
- Imports are synchronous and per-row committed; see Importing.
- Two sources of the same kind with a same-named table pool their objects under one type; the second import
  is refused, and the ontology preview warns about the shared physical type.
- **Tenant isolation is by request parameter.** REST sessions carry no tenant claim of their own, so the caller
  chooses the tenant, exactly as for the ontology and graph routes. That is why every sources route is admin-only:
  an administrator is a global operator. A source's tenant keeps tenants' data apart, but it is not an
  authorization boundary between mutually untrusting tenants; per-user tenancy would be an auth-model change.
- No Studio form for adding a source: sources are operator configuration by design.
