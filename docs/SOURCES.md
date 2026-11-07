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
- **Admin only.** `discover`, `ontology` and `import` need the `admin` role in multi-user mode. Listing
  is readable. A tenant's sources are invisible to other tenants; a missing and a foreign source answer
  identically (`404 source not found`).
- **Bounded.** Every call has a deadline. A source that accepts a connection and then goes silent fails
  at its timeout (the blocking driver call runs behind a deadline gate; at most 8 abandoned calls may be
  stuck at once, after which new calls are refused until they clear). Containers are capped at
  `max_containers`; imports at `import_max_rows`.
- **Audited.** Each call writes one id-only line (`tool: sources`): source, outcome, fingerprint prefix.

## What discovery records

Per source, as ordinary knowledge objects in the source's tenant (queryable, versioned, `AS_OF`-able):

| type | one per | holds |
|---|---|---|
| `aikoql:source` | source | name, kind, schema fingerprint, container count, references |
| `aikoql:source-table` | container | fields (type, nullability, keys, provenance), unique groups, schema namespace |
| `aikoql:schema-run` | change | fingerprint, previous fingerprint, the diff |

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
  `merges: [["crm.customers", "shop.clients"]]` → one class, two mappings.
- **Preview writes nothing.** It returns the classes, what would be added / refreshed / left untouched,
  warnings (type conflicts, name clashes, composite foreign keys, observed-only fields) and the schema
  `fingerprints` it was computed from.
- **Publish needs the fingerprints you reviewed** (else `409 schema changed since preview`) and, when a
  hand-written ontology exists, its `expected_version`. It merges into the tenant's manual ontology and
  **never changes what a human wrote**: existing classes, properties, relationships and mappings win;
  classes discovery itself created earlier are refreshed with new columns; identical content writes nothing.
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

- Supported for `postgres`, `sqlite`, `mongodb`. **Neo4j and ClickHouse answer 400** — import them with the
  CLI (`aikoql-mcp import …`); ClickHouse is discovery-only.
- Idempotent: identical rows write nothing; a changed row updates in place.
- **Capped and synchronous.** Default 10 000 rows per container; the response says which containers were cut
  (`truncated`). Commits are per row (~270 rows/s on the v2 engine), so raise `import_max_rows` knowingly.
  Use the CLI for bulk loads.
- A type that already holds objects from another source, or from a CLI import, is **skipped with a reason**
  rather than overwritten (object identity is `(container, primary key)`).

## REST

| route | role | |
|---|---|---|
| `GET /api/v1/sources[?tenant=]` | any signed-in | configured sources + last run |
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
- Postgres composite foreign keys become one relationship (with a warning); `FieldProfile` ratios for
  Neo4j are not available (the schema procedure reports types and "mandatory" only).
- Imports are synchronous and per-row committed; see Importing.
- Two sources of the same kind with a same-named table pool their objects under one type; the second import
  is refused, and the ontology preview warns about the shared physical type.
- **Tenant isolation is by request parameter.** REST sessions carry no tenant claim of their own, so a signed-in
  user chooses the tenant, exactly as for the ontology and graph routes. A source's tenant keeps tenants' data
  apart, but it is not an authorization boundary between mutually untrusting tenants; the `admin` role gates every
  write.
- No Studio form for adding a source: sources are operator configuration by design.
