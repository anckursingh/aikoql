# Source schema discovery → canonical ontology (design)

Status: proposed 2026-11-07 for PR #31 (`feature/competitor-clickhouse-restamp`), one PR, no follow-ups.
Origin: Codex assessment of the current code, re-checked against the repo (see "Verified facts").

## Goal
An operator registers external databases (PostgreSQL, MongoDB, Neo4j, SQLite, ClickHouse);
Studio's **Data Sources** panel introspects them (schema only by default), records the source
schema as versioned Knowledge Objects, proposes a domain ontology, and — after explicit
approval — publishes it so `MATCH Customer` queries every mapped source. Optionally imports
selected containers.

## Verified facts (the plan builds on these)
- `connector_metadata_to_ir` sets every container's `type_hint` to `<connector>Table|Collection|NodeLabel`
  (`ingestion/src/connector_bridge.rs:88`) and `discover_ontology_from_ir` uses `type_hint` as the
  class name (`ingestion/src/ontology.rs:119`): all tables of a source collapse into ONE class. This is the
  first thing to fix.
- `OntologyProposal` (ingestion) has no consumer outside tests: nothing turns it into the kernel's
  `OntologyDef`, and the live providers are only wired to `ConnectorMetadata` by hand in
  `tests/connector_certification.rs` (ONT-001).
- `OntologyDef.mappings` (`source`, `physical_type`, `class`, `property_map`) already exists, and the compiler
  expands `MATCH <Class>` into one plan per mapping (`compiler/src/parser/mod.rs:127-190`). Imported KOs keep
  the physical table/collection/label as `type_name`. `property_map` is stored but NOT applied at query time.
- Imports are offline CLI functions (`imports.rs: run_pg_import…`) that open the database themselves and call
  `process::exit` on error. `deploy-connector` stores `config:{}` in a KO and connects to nothing.
- `active_ontology_for` (`http.rs:120`) knows two kinds: tag `auto-discovered`, else "manual"; the manual
  ontology is one KO per tenant, saved in place with a CAS on `expected_version`.
- Provider gaps: Postgres keeps one column pair per composite FK; Mongo samples the first 100 documents and
  reports names only; Neo4j reads `LIMIT 1`; no ClickHouse provider exists (only the benchmark column).

## Decisions
1. **Sources are operator config, not Studio input.** `[[sources]]` in `aikoql.toml`: `name`, `kind`,
   `url_env` (the NAME of an env var holding the connection string), optional `database`, `tenant`, limits.
   Studio lists and operates configured sources by name. This removes credential entry, credential storage in
   KOs, and SSRF (a Studio user can never choose a host). A Studio "add source" form is out of scope.
2. **Read-only, schema-only by default.** `mode: "schema"` reads metadata; `"sample"` adds bounded value
   profiling; import is a separate admin action on named containers.
3. **Source schema is knowledge, at table granularity.** KO types `aikoql:source`, `aikoql:source-table`
   (fields/references are properties, not KOs), `aikoql:schema-run`. Versions give schema history through the
   existing MVCC; an unchanged fingerprint writes nothing.
4. **Declared vs observed.** Postgres/ClickHouse/SQLite types are `declared`; Mongo/Neo4j sampled ones are
   `observed` with `types`, `presence_ratio`, `null_ratio`, `samples`.
5. **One class per container, physical identity kept.** Class = singular PascalCase of the container name;
   the physical container stays the KO `type_name` and appears in a `MappingEntry`.
6. **No automatic cross-source merge.** Same-named classes from two sources stay separate (suffixed by source)
   and are reported as `possible_duplicate`; a human approves `merges: [[a, b]]`, which yields ONE class with
   two mappings.
7. **Approval never overwrites human work.** Publishing merges into the tenant's existing manual ontology:
   existing classes/properties are untouched, discovered ones are added, and the source's own mappings are
   replaced. Guarded by `expected_version` (CAS) and by the schema fingerprint the reviewer saw (stale → 409).
8. **Admin-only, tenant-bound, audited.** All `/api/v1/sources/*` writes need the admin role; a source with a
   `tenant` is invisible to other tenants; each action writes an id-only audit line.

## Non-goals
Studio credential entry; CDC/continuous sync; ClickHouse row import; fuzzy/ML class matching;
query-time `property_map` translation is a stretch task (T13) and is dropped if the compiler change is not small.
