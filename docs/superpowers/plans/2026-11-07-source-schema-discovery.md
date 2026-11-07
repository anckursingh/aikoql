# Source schema discovery → canonical ontology — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Register external databases in `aikoql.toml`, introspect them from Studio, store their schema as versioned KOs, propose and (on approval) publish a domain ontology whose classes map to the physical tables, and optionally import chosen containers — all inside PR #31.

**Architecture:** Providers gain richer introspection; a new `sources/` module in `aikoql-mcp` turns a configured source into `ConnectorMetadata` (existing type, extended), persists it (`aikoql:source*` KOs), and builds an ontology draft (new, in `aikoql-ingestion`) that is published through the existing ontology KO + `MappingEntry` machinery. Studio gets one new panel over four REST routes. No new engine; the IR→proposal code is reused and its class-collapse bug fixed.

**Tech Stack:** Rust (`aikoql-ingestion`, `aikoql-mcp`, providers, one new crate `aikoql-clickhouse` on `ureq`), Studio vanilla JS, Node/playwright e2e, Python (`sqlite3` stdlib, to build e2e fixtures).

**Spec:** `docs/superpowers/specs/2026-11-07-source-schema-discovery-design.md` (read it first — its "Verified facts" and "Decisions" bind every task).

## Global Constraints

- **One PR, #31.** Every task lands on `feature/competitor-clickhouse-restamp`. Retitle the PR in T15. No other branch or PR.
- TDD RED-first: each task = a RED commit (fails on an *assertion*, never a compile error — add the smallest stub) then the GREEN commit. Trailer on every commit: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.
- The branch ends with the dogfood re-stamp as the tip: `AIKOQL_BIN=<abs path to a built aikoql-mcp> python scripts/dogfood-review-loop.py full --kb <fresh dir>` (add `--kb` if the plugin holds `./kb`), commit `docs/PR6-TDD-DISPOSITIONS.md`, then `bash scripts/check-disposition-head.sh` must pass.
- **Push once, at the end of T15**, after every gate below ran locally. Never push per task; cancel superseded runs; no re-run/probe pushes (GitHub credit is limited).
- Gates before the push: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings` (zero warnings); `cargo test -p aikoql-mcp -p aikoql-ingestion -p aikoql-postgres -p aikoql-mongodb -p aikoql-neo4j -p aikoql-sqlite -p aikoql-clickhouse --no-fail-fast`; Studio e2e `node tests/e2e/studio/run.js`; every `scripts/check-*.sh`; `python3 scripts/bump-version.py --check`. List anything that could not run locally (live Postgres/Mongo/Neo4j cells if Docker is down, Windows, coverage-floor).
- **Windows-safe code and tests** (the last PR failed Windows on this): never match OS error *text*, classify by `io::ErrorKind`; build paths with `Path::join`; no `\n`-only assumptions in file fixtures; no hard-coded `/tmp`.
- Live-database tests are env-gated through `connectors::Live::pg()/mongo()/neo4j()` exactly like ONT-001; register any new gated cell in `tests/gated.toml` (`scripts/check-skip-drift.sh` decides). Cells that need only SQLite always run.
- **No secret ever reaches a KO, an audit line, a REST response, a log line or an error string.** Connection strings are read from the env var named by `url_env` at call time and passed through the existing `imports.rs::redact_uri/redact_secrets`.
- New config tables use `#[serde(deny_unknown_fields)]`; unset = feature absent and existing behaviour byte-identical.
- Prompt/ontology compatibility: nothing here changes `OntologyDef`'s encoding; `property_defs` is global by name (a name used with two types is a conflict, see T3).

## Hardening standard (owner request 2026-11-07: best-in-class robustness is a product feature)

- **Every task ships adversarial + fuzz tests, not only happy paths.** Fuzz = seeded `xorshift64` loops (the env-hygiene gate bans unseeded RNGs and `set_var`): fixed seeds, iteration count `AIKOQL_FUZZ_ITERS` (read-only env; default 2 000, run 50 000 locally before the push), the failing seed and input printed in the assertion message so a failure is reproducible with one command.
- **Hostile-input pool** reused everywhere (copy the ~20-line generator per crate; do not share across crates): empty, NUL, newline, `"`/`'`/backtick/`;--`, SQL/Cypher/CH injection fragments, 10 000-char names, RTL/zero-width/emoji/combining marks, Unicode lookalikes, reserved words (`order`, `select`), path-like (`../x`), 5 000-element lists, JSON nested 10 000 deep.
- **Invariants over examples:** fuzz asserts properties — never panics, never hangs (time bound), output valid and bounded, no secret echo, read-only sources byte-identical afterwards, idempotent re-runs, tenant isolation, permutation invariance (fingerprint, draft class set).
- **Findings found while hardening are fixed in the same task** (RED first) and ledgered; none are "noted for later".

## Review Focus

1. **Two tables in one source become two classes**, never one (`postgresTable` collapse) — pinned in T1, re-pinned end to end in T12.
2. **Two sources with a same-named table are never merged automatically**; only an explicit `merges` pair makes one class with two mappings — T3 and T9.
3. **A schema change after approval leaves the human/approved ontology untouched** and surfaces as a diff, with a stale-fingerprint 409 on apply — T8, T9.
4. **A reserved/odd container name** (`order`, `user data`, `2024_sales`, `Ünïcode`, `a-b`) yields a valid, unique class name; an empty database yields an empty draft, not an error — T1, T3.
5. **Hostile names and payloads anywhere** (injection fragments, NUL, 10 000-char names, deep JSON) never panic, inject, or escape rendering — T1, T4, T5, T9, T11, T13.
6. **An unreachable, slow or huge source** (dead port, 5 000 tables) fails fast with a secret-free message or truncates with a flag, and never hangs a request thread — T6, T7.
7. **Tenant isolation:** tenant A cannot list, discover, approve or import tenant B's source, nor read its snapshots — T9.

## File Structure

| File | Responsibility |
|---|---|
| `crates/ingestion/src/connector_bridge.rs` (modify) | extended metadata types, `class_name_for_container`, per-container class hint |
| `crates/ingestion/src/source_schema.rs` (create) | `fingerprint`, `diff_metadata`, `SchemaDiff` |
| `crates/ingestion/src/ontology_draft.rs` (create) | `definition_from_sources` → `DefinitionDraft` (JSON accepted by `tool_save_ontology`) |
| `crates/providers/{postgres,sqlite,mongodb,neo4j}/src/lib.rs` (modify) | richer introspection |
| `crates/providers/clickhouse/` (create) | `aikoql-clickhouse`: HTTP introspection, read-only |
| `crates/services/api/mcp/src/config.rs` (modify) | `[[sources]]` |
| `crates/services/api/mcp/src/sources/{mod,introspect,store,ontology,import}.rs` (create) | the production service |
| `crates/services/api/mcp/src/api_rest.rs`, `http.rs` (modify) | routes, admin gate, ontology precedence |
| `crates/services/api/mcp/src/imports.rs` (modify) | library-style imports; CLI keeps its behaviour |
| `crates/services/api/mcp/src/studio.rs`, `assets/studio/app.js` (modify) | Data Sources panel |
| `crates/services/api/mcp/tests/source_discovery.rs` (create) | the production-workflow test |
| `crates/services/api/mcp/tests/source_hardening.rs`, `scripts/fuzz-sources.sh` (create) | adversarial + fuzz capstone |
| `docs/SOURCES.md`, `CHANGELOG.md`, `AGENTS.md` (modify/create) | docs |

## Task protocol (applies to every task; steps below name only what is specific)

- **S1** Write the listed failing tests (assertions as named). **S2** Run the task's test command; it must FAIL on assertions. **S3** Commit `test(<area>): RED — …`. **S4** Implement the listed signatures. **S5** Run the test command (PASS), `cargo clippy -p <crates> --all-targets -- -D warnings`, `cargo fmt --all`. **S6** Commit `feat(<area>): …`.
- Test commands: ingestion → `cargo test -p aikoql-ingestion --lib <filter>`; mcp unit → `cargo test -p aikoql-mcp --bin aikoql-mcp -- <filter>`; mcp integration → `cargo test -p aikoql-mcp --test <name>`. PATH: `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"`.

---

### Task 1: One class per container (fix the collapse)

**Files:** Modify `crates/ingestion/src/connector_bridge.rs` (:88 and the test at :312), `crates/services/api/mcp/tests/connector_certification.rs` (ONT-001 assertions that expect `postgresTable`-style classes). Test: same files.

**Interfaces — Produces:** `pub fn class_name_for_container(container: &str) -> String` — singular PascalCase; splits on non-alphanumerics and case humps; a leading digit gets prefix `T`; a reserved-looking or empty result becomes `Thing`; only ASCII-safe `[A-Za-z0-9]` kept after transliterating letters via `char::is_alphanumeric` (Unicode letters allowed, others dropped). Singularise only `…ies→…y`, `…ses→…s`, trailing `s` not preceded by `s`.

- [ ] Tests (ingestion): `class_name_for_container` maps `customers→Customer`, `order_items→OrderItem`, `Categories→Category`, `address→Address` (no stripping "ss"), `2024_sales→T2024Sale`, `user data→UserData`, `a-b→AB`, `""→Thing`; `connector_metadata_to_ir_gives_each_container_its_own_class` (3 containers → `discover_ontology_from_ir(...).classes` names `{Customer, Order, Product}`); `source_kind_is_not_the_class` (no class contains `Table`/`Collection`/`NodeLabel`); the entity `mentions` still say `Table 'customers'…`.
- [ ] Implement: `type_hint: Some(class_name_for_container(&container.name))`; keep `connector_type`/physical name in `mentions` and in the entity name (`{label}.{container}`), unchanged. Update ONT-001 and the unit test at :312 to the new class names.
- [ ] Run: `cargo test -p aikoql-ingestion --lib connector_bridge` and `cargo test -p aikoql-mcp --test connector_certification ont001` (live cells skip without DBs; the SQLite-free assertions still run).
- [ ] **Fuzz/adversarial (T1):** `class_name_fuzz_never_panics_and_is_a_valid_identifier` — 20k seeded strings from the hostile pool: no panic; result non-empty, starts alphabetic, contains only alphanumerics, `len() <= input.len() + 1`, deterministic (same input → same output).

### Task 2: Richer metadata, fingerprint and diff

**Files:** Modify `connector_bridge.rs`; create `crates/ingestion/src/source_schema.rs`; export from `lib.rs`. Update every `FieldInfo {…}`/`ContainerInfo {…}` literal (grep `FieldInfo {` in `crates/`) — derive `Default` on both so call sites use `..Default::default()`.

**Interfaces — Produces:**
```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provenance { #[default] Declared, Observed }
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldProfile { pub types: Vec<String>, pub presence_ratio: f64, pub null_ratio: f64, pub samples: u64 }
// FieldInfo gains: #[serde(default)] provenance: Provenance, #[serde(default)] profile: Option<FieldProfile>
// ContainerInfo gains: #[serde(default)] schema: Option<String>, #[serde(default)] unique_groups: Vec<Vec<String>>, #[serde(default)] truncated: bool (metadata-level: on ConnectorMetadata)
impl ConnectorMetadata { pub fn fingerprint(&self) -> String }   // sha-256 hex
pub struct SchemaDiff { pub added_containers: Vec<String>, pub removed_containers: Vec<String>, pub changed: Vec<ContainerDiff> }
pub struct ContainerDiff { pub name: String, pub added_fields: Vec<String>, pub removed_fields: Vec<String>, pub type_changes: Vec<(String, String, String)> }
pub fn diff_metadata(old: &ConnectorMetadata, new: &ConnectorMetadata) -> SchemaDiff
```
`ConnectorMetadata` also gains `#[serde(default)] pub truncated: bool`.

- [ ] Tests: old JSON without the new keys still deserialises (serde defaults); `fingerprint_ignores_row_counts_and_order` (shuffle containers/fields, change `row_count` → same); `fingerprint_changes_on_new_column_type_change_or_nullability`; `diff_reports_added_removed_and_retyped_fields`; `diff_of_identical_is_empty`; `observed_profile_serialises_with_presence_ratio`.
- [ ] Implement per the signatures (canonicalise by sorting names; exclude `row_count`, `profile.samples`; include `provenance` and types). The bridge adds, for observed fields, a fact line `…: observed types=[…] presence=0.91` and keeps declared output byte-identical (existing bridge tests stay green).
- [ ] Run: `cargo test -p aikoql-ingestion --lib source_schema connector_bridge`.
- [ ] **Fuzz/adversarial (T2):** `fingerprint_permutation_invariance_fuzz` (random metadata, random shuffles of containers/fields/references/unique groups ⇒ equal), `fingerprint_single_mutation_changes_it_fuzz` (add/remove/retype/null-flip/rename ⇒ differs), `diff_symmetry_fuzz` (`diff(a,b).added == diff(b,a).removed`, `diff(a,a)` empty), JSON round trip keeps the fingerprint; a 5 000-container metadata fingerprints in < 1 s.

### Task 3: Ontology draft (proposal → publishable definition)

**Files:** Create `crates/ingestion/src/ontology_draft.rs`; export from `lib.rs`.

**Interfaces — Consumes:** T1 class naming, T2 metadata, existing `connector_metadata_to_ir`, `discover_ontology_from_ir`. **Produces:**
```rust
pub struct SourceInput<'a> { pub name: &'a str, pub kind: &'a str, pub meta: &'a ConnectorMetadata }
pub struct ClassMerge { pub a: String, pub b: String }          // "source.container" refs
pub struct DefinitionDraft {
    pub definition: serde_json::Value,   // {namespace, version, classes[], relationships[], property_defs[], mappings[]}
    pub classes: Vec<DraftClass>,        // name, sources, status
    pub warnings: Vec<String>,
}
pub enum ClassStatus { New, PossibleDuplicate { with: String } }
pub fn definition_from_sources(sources: &[SourceInput], merges: &[ClassMerge]) -> DefinitionDraft
```
Rules: one class per container; same class name from two sources → both kept, suffixed `<PascalSource>` (e.g. `CrmCustomer`), each `PossibleDuplicate`; a `ClassMerge` collapses the pair into one class (the first ref's name) with two `mappings`; each mapping's `property_map` maps physical column → `snake_case` canonical (identity unless it differs); `property_defs` are global by name — a name seen with two different types is widened to `Text` and pushes a warning; relationships from `references` (domain/range = class names; composite FK ⇒ name suffixed with the first column and a warning); empty input ⇒ empty draft; `warnings` also lists observed-only fields.

- [ ] Tests: `three_tables_three_classes_with_mappings` (mapping `{source:"postgres", physical_type:"customers", class:"Customer"}`); `fk_becomes_relationship_with_domain_and_range`; `same_name_in_two_sources_is_possible_duplicate_not_merged` (classes `CrmCustomer`, `ShopCustomer`, both flagged); `explicit_merge_yields_one_class_two_mappings`; `conflicting_property_types_widen_to_text_with_warning`; `odd_container_names_yield_unique_valid_class_names` (`order`, `Order`, `user data`, `2024_sales` → no duplicates); `empty_sources_is_empty_draft`; and in the **mcp** crate `draft_definition_round_trips_through_ontologydef` (`OntologyDef::from_property_map(parse_properties(draft.definition))` is Ok and `validate()` passes) so the JSON is exactly what `tool_save_ontology` accepts.
- [ ] Implement; keep functions small — class naming from T1, merging in one pass over a `BTreeMap<String, Vec<(source, container)>>`.
- [ ] Run: `cargo test -p aikoql-ingestion --lib ontology_draft` + the mcp round-trip test.
- [ ] **Fuzz/adversarial (T3):** `draft_fuzz_always_valid` — random multi-source metadata (hostile names, duplicate names, random references incl. dangling ones, random merges incl. unknown refs): never panics; class names unique; every mapping/relationship endpoint names an existing class; the definition decodes through `OntologyDef::from_property_map` (mcp-side test) and `validate()` passes; class count = containers − applied merges; draft is invariant under source order except for tie-broken names.

### Task 4: Provider introspection upgrades

**Files:** Modify `crates/providers/{postgres,sqlite,mongodb,neo4j}/src/lib.rs`; tests in each crate and (live, gated) `connector_certification.rs`.

**Interfaces — Produces** (additive; existing methods keep working):
- postgres: `ForeignKeyInfo` gains `child_columns: Vec<String>, parent_columns: Vec<String>` (keep the single-column fields = first pair for current callers); `TableSchema` gains `unique_groups: Vec<Vec<String>>`, `schema: String` (from `table_schema`); read `pg_constraint`/`information_schema.key_column_usage` ordered by `ordinal_position`.
- sqlite: `fn introspect_foreign_keys(&self) -> Result<Vec<ForeignKeyInfo>, String>` (`PRAGMA foreign_key_list`, composite aware); `unique_groups` from `PRAGMA index_list` where `unique=1`.
- mongodb: `CollectionSchema` gains `fields: Vec<FieldProfile>` (name, observed BSON types, presence ratio, null ratio, samples) from a `$sample` aggregation of `sample_limit` documents (new `introspect_collection_sampled(name, sample_limit)`); `introspect_collection` unchanged.
- neo4j: `fn introspect_label_schema(&self, label: &str) -> Result<Vec<(String, Vec<String>, bool)>, String>` — property, observed types, mandatory — via `CALL db.schema.nodeTypeProperties()` filtered to the label, falling back to a `LIMIT sample_limit` sample when the procedure is unavailable; `fn introspect_rel_patterns(&self) -> Result<Vec<(String, String, String)>, String>` (rel type, from label, to label) from `db.schema.visualization()` or a sampled `MATCH ()-[r]->()`.

- [ ] Tests: SQLite (always runs): `sqlite_foreign_keys_composite`, `sqlite_unique_groups`; pure-function unit tests for the Mongo profile aggregation (`profile_from_docs` on in-memory `Vec<Document>`: ratios, mixed types `[Int, Null]`); Postgres/Neo4j composite-FK and schema-procedure cells added to `connector_certification.rs` behind `Live::pg()/neo4j()` (SQL fixtures: a composite FK, a unique constraint, a table in a non-`public` schema).
- [ ] Implement; the Mongo profile logic is a pure fn so it is testable without a server.
- [ ] Run: `cargo test -p aikoql-sqlite -p aikoql-mongodb -p aikoql-postgres -p aikoql-neo4j`; live cells run if Docker databases are up (`scripts/` or CI-style env), otherwise listed as not run.
- [ ] **Fuzz/adversarial (T4) — fix what it finds:** (a) **Neo4j label injection:** `introspect_label_props`, `count_nodes`, `import_nodes` interpolate the label into ``(n:`…`)`` unescaped; add a shared `quote_label()` (double every backtick) and RED tests with labels like ``x`) DETACH DELETE (m //`` against the live container proving no data is deleted. (b) SQLite hostile identifiers (`"; DROP TABLE x; --`, quotes, unicode, reserved words, 300-char names) introspect and import without error or injection. (c) `profile_from_docs` fuzz over random BSON incl. depth-200 nesting (cap recursion at 64 levels, deeper keys reported as `Document`), huge arrays, NaN/Infinity, NUL keys: no panic/stack overflow. (d) `label_props_from_sample` fuzz over random JSON. (e) Postgres: a table named ``a"b`` / with a space / reserved word introspects (live cell).

### Task 5: ClickHouse provider crate

**Files:** Create `crates/providers/clickhouse/{Cargo.toml,src/lib.rs}` (`name = "aikoql-clickhouse"`, `publish = false`, workspace version); add to `Cargo.toml` members; if `scripts/bump-version.py --check` or the architecture gate lists crates, register it there.

**Interfaces — Produces:**
```rust
pub struct ClickHouseConnector { /* base url, database, user, password, agent */ }
impl ClickHouseConnector {
    pub fn connect_with_timeout(url: &str, database: &str, timeout_ms: Option<u64>) -> Result<Self, String>; // issues `SELECT version()`
    pub fn list_tables(&self) -> Result<Vec<String>, String>;
    pub fn introspect_all(&self) -> Result<Vec<TableSchema>, String>;
}
pub struct TableSchema { pub name: String, pub engine: String, pub columns: Vec<ColumnInfo>, pub sorting_key: Vec<String>, pub primary_key: Vec<String>, pub partition_key: String, pub row_count_estimate: u64 }
pub struct ColumnInfo { pub name: String, pub ch_type: String, pub is_nullable: bool, pub is_low_cardinality: bool, pub in_primary_key: bool }
```
All queries go over HTTP with `?readonly=1&default_format=JSONEachRow`, read `system.tables`/`system.columns` only, send credentials from the URL userinfo, cap response bodies at 8 MiB, and never put the password in an error (use the `redact_uri` pattern).

- [ ] Tests against a stub HTTP server (a `TcpListener` thread in the test, as `chat/stub.rs` does in this repo's PR #30 — copy the ~40 lines, do not depend on it): `introspect_all_parses_system_tables_and_columns` (Nullable(String), LowCardinality(String), key columns), `sends_readonly_1_and_no_write_verbs` (assert the request line/query contain `readonly=1` and never `INSERT|CREATE|DROP|ALTER`), `password_is_never_in_errors`, `oversized_body_is_rejected`, `unreachable_is_a_short_error`.
- [ ] Implement with `ureq`; classification of timeouts by `io::ErrorKind` (see Global Constraints).
- [ ] Run: `cargo test -p aikoql-clickhouse`; `cargo clippy --workspace --all-targets -- -D warnings`; `python3 scripts/bump-version.py --check`; `bash scripts/check-architecture-hygiene.sh`.
- [ ] **Fuzz/adversarial (T5):** the stub server replays malformed replies — truncated JSON, empty body, non-UTF-8 bytes, 50-level nested JSON, numbers beyond `u64`, thousands of tables, a column list of 100 000, slow-drip body, HTTP 500/401/redirect loop — each must return a short `Err` or a bounded result, never panic or hang (`timeout_ms` honoured, elapsed asserted). Table/database names go to ClickHouse as **HTTP query parameters** (`param_t` + `{t:String}`), never string-concatenated; test with hostile names and assert the recorded request has no raw name in the SQL text.

### Task 6: `[[sources]]` configuration

**Files:** Modify `crates/services/api/mcp/src/config.rs` (TomlConfig, RuntimeConfig, tests next to `chat_config_*`), create `crates/services/api/mcp/src/sources/mod.rs` (config types only here).

**Interfaces — Produces:**
```rust
#[derive(Clone, Debug, PartialEq, Deserialize)] #[serde(deny_unknown_fields)]
pub(crate) struct SourceConfig {
    pub name: String,                 // [A-Za-z0-9_-]{1,64}, unique
    pub kind: SourceKind,             // postgres | mongodb | neo4j | sqlite | clickhouse
    pub url_env: String,              // NAME of the env var holding the connection string / sqlite path
    pub database: Option<String>,     // mongodb, clickhouse
    pub tenant: Option<String>,
    #[serde(default = "d_sample")] pub sample_limit: u32,        // 100, max 10_000
    #[serde(default = "d_containers")] pub max_containers: u32,  // 500
    #[serde(default = "d_timeout")] pub timeout_secs: u64,       // 15, min 1
    #[serde(default = "d_rows")] pub import_max_rows: u64,       // 100_000
}
impl SourceConfig { pub(crate) fn validate(&self) -> Result<(), String>; pub(crate) fn connection(&self) -> Result<String, String> /* reads the env var; "source X: env var Y is not set" */ }
pub(crate) static SOURCES: OnceLock<Vec<SourceConfig>>;   // set at startup next to CHAT_CONFIG
pub(crate) fn find_source(name: &str, tenant: Option<&str>) -> Result<&'static SourceConfig, String>; // "source not found" for a tenant mismatch (no existence leak)
```
- [ ] Tests: absent ⇒ empty; all keys parse; typo key rejected naming the key; bad name / duplicate name / `sample_limit` 0 or >10 000 / `timeout_secs` 0 / `kind` unknown rejected; `connection()` errors when the env var is unset and never echoes a value; `find_source` hides another tenant's source (same error as unknown). (Do **not** `set_var` — the env-hygiene gate bans it; use `PATH` as the always-set variable for the positive case, as in the chat client test.)
- [ ] Implement; startup sets `SOURCES`.
- [ ] **Fuzz/adversarial (T6):** `sources_config_fuzz_never_panics` — random TOML-ish `[[sources]]` blocks (wrong types, huge ints, duplicate/lookalike names such as `crm`/`crm` with a Cyrillic `с`, NUL/newline in `url_env`, 5 000 entries): only `Ok` or an error naming the offending key, never a panic; a name that passes validation is `[A-Za-z0-9_-]{1,64}`; `connection()` never returns or echoes the value in any `Err`.

### Task 7: Production introspection adapters

**Files:** Create `crates/services/api/mcp/src/sources/introspect.rs`.

**Interfaces — Consumes:** T2 metadata, T4/T5 providers, T6 config. **Produces:**
`pub(crate) fn introspect_source(src: &SourceConfig, mode: Mode) -> Result<ConnectorMetadata, String>` with `pub(crate) enum Mode { Schema, Sample }`. Maps each provider's schema into `ConnectorMetadata` (declared for SQL kinds; observed + `FieldProfile` for Mongo/Neo4j; `unique_groups`, `schema`, composite FKs as multi-field `ReferenceInfo`). Truncates at `max_containers` and sets `meta.truncated = true`. `Mode::Schema` never reads values (Mongo/Neo4j fall back to names-only/procedure metadata with `Provenance::Observed` and no profile numbers). Errors: `"source <name>: <short reason>"`, redacted, classified connect/timeout/auth without echoing the DSN.

- [ ] Tests (SQLite, always runs): a temp SQLite file with `customers`, `orders(customer_id → customers.id)` ⇒ 2 containers, 1 reference, PK flags, `Mode::Schema` ok; `truncates_and_flags_over_max_containers` (`max_containers = 1`); `unreachable_source_fails_fast_without_the_dsn` (SQLite path in a missing dir, and a closed TCP port for Postgres with `timeout_secs = 1`; assert elapsed < 5 s and the error text contains neither the path's directory nor the password); live equivalents for pg/mongo/neo4j behind `Live::*` that assert the same containers as ONT-001 now come from `introspect_source` (this replaces the hand-built metadata there).
- [ ] Implement as a `match src.kind` dispatch of small per-kind functions.
- [ ] **Fuzz/adversarial (T7):** SQLite as the always-on target — a non-database file, an empty file, a truncated header, a directory, a 0-permission file ⇒ short secret-free `Err`; a DB with 5 000 tables (`max_containers = 500` ⇒ `truncated`, < 5 s), a 2 000-column table, hostile table/column names; **read-only proof:** SHA-256 of the file before and after `introspect_source` is identical and the connection is opened `SQLITE_OPEN_READ_ONLY`; a concurrent writer holding a lock ⇒ fails within `timeout_secs`, not forever.

### Task 8: Schema Knowledge Objects

**Files:** Create `crates/services/api/mcp/src/sources/store.rs`.

**Interfaces — Produces:**
```rust
pub(crate) struct SnapshotOutcome { pub run_koid: Option<String>, pub fingerprint: String, pub changed: bool, pub diff: SchemaDiff, pub previous_fingerprint: Option<String> }
pub(crate) fn record_snapshot(k: &Kernel, src: &SourceConfig, meta: &ConnectorMetadata) -> Result<SnapshotOutcome, String>;
pub(crate) fn latest_snapshot(k: &Kernel, src: &SourceConfig) -> Result<Option<ConnectorMetadata /*with fingerprint*/>, String>;
```
KO types (all tenant = `src.tenant`, `Origin::System`, ACL owner `system`): `aikoql:source` (one per source: name, kind, database — **never** the url), `aikoql:source-table` (one per container, updated in place with CAS: `source`, `name`, `schema`, `fields` (list of maps incl. provenance/profile), `references`, `unique_groups`, `row_estimate`, `fingerprint`), `aikoql:schema-run` (one per run that *changed* anything: `source`, `fingerprint`, `previous`, `containers`, `diff` summary). An unchanged fingerprint writes **nothing**. Removed containers get their `aikoql:source-table` tombstoned via `forget`, which keeps history.

- [ ] Tests (in-memory kernel, `chat::stub::kernel()` pattern): first snapshot writes 1 source + N tables + 1 run, `changed=true`; identical second snapshot writes nothing (`changed=false`, `run_koid=None`, journal head unchanged); adding a column bumps exactly that table's KO version and records a diff with the field name; a removed table is tombstoned and `AS_OF` the earlier timestamp still shows it; **no stored property anywhere contains the connection string** (set the source's env var name to `PATH`, assert no KO text contains `PATH`'s value); tenant-scoped source writes carry that tenant and another tenant's `scan_by_type` sees none.
- [ ] Implement; find existing KOs with `scan_by_type` + property match (the pattern in `tool_discover_ontology`).
- [ ] **Fuzz/adversarial (T8):** `snapshot_sequence_fuzz` — random mutate/snapshot sequences: after every step `aikoql:source-table` live count == current containers, exactly one live `aikoql:source`, `aikoql:schema-run` count == number of fingerprint changes, and replaying the same snapshot is a no-op; `concurrent_snapshots_do_not_duplicate` — 8 threads snapshotting the same source ⇒ one source KO, one table KO per container, runs consistent; a 2 000-table snapshot completes in bounded time; hostile container names round-trip byte-exact.

### Task 9: Discovery service, ontology publish, REST routes

**Files:** Create `sources/ontology.rs`; extend `sources/mod.rs` with the tool fns; modify `api_rest.rs` (routes, `is_admin_route`, error-status mapping, `json_response` 502), `http.rs` (`active_ontology_for`), `tools/mod.rs` if tools are registered there.

**Interfaces — Produces:**
- `GET /api/v1/sources` → `[{name, kind, tenant, database, last_run:{fingerprint, containers, truncated}|null}]` (authenticated; filtered by the request `tenant`).
- `POST /api/v1/sources/discover {source, mode?: "schema"|"sample", tenant?}` → `SnapshotOutcome` + `containers` + `truncated`. Admin.
- `POST /api/v1/sources/ontology {sources: [name], merges?: [[ref,ref]], preview: bool, fingerprints?: {name: fp}, expected_version?, tenant?}` → preview: `{classes:[{name,sources,status}], adds:[…], untouched:[…], mappings:[…], warnings:[…], fingerprints:{…}}` writing nothing; apply: requires `fingerprints` equal to each source's latest snapshot (else **409** `schema changed since preview`) and `expected_version` when a manual ontology exists; merges into the existing manual ontology (spec decision 7), saves via the `tool_save_ontology` code path, then `refresh_ontologies`. Admin.
- Error mapping: `source not found` → 404; `schema changed since preview` → 409; `source <n>: …` → 502; validation (`missing`/`bad`) → 400.
- `active_ontology_for` unchanged in meaning: an applied source ontology **is** the manual ontology (spec decision 7), so Studio's Ontology panel edits it with the normal CAS.
- Audit: `audit_log(db, subject, "sources", "discovered"|"previewed"|"published"|"error", "source=… tenant=… fp=… classes=N")` — ASCII-sanitised like `chat::audit_safe` (the 200-byte slice in `audit_log` panics on a multi-byte cut).

- [ ] Tests (route-level, `m3_kernel`/`m10_call` pattern, SQLite sources, `PATH` trick not needed — the sqlite path env var is set by the *test process* via the per-child `SOURCES` static, so inject `SOURCES` through a `#[cfg(test)] fn set_sources_for_test` guarded by a mutex): `sources_list_hides_other_tenants`; `discover_is_admin_only_viewer_403`; `discover_unknown_source_404_same_as_other_tenants_source`; `preview_writes_nothing` (journal head unchanged); `apply_requires_matching_fingerprint_else_409`; `apply_merges_into_manual_ontology_without_touching_existing_classes` (pre-seed a manual class `Widget` with a custom property, apply, assert `Widget` byte-identical and new classes + mappings present); `apply_with_stale_expected_version_409`; `schema_change_after_apply_changes_nothing_until_reapproved` (alter the SQLite table, re-discover, assert ontology KO version unchanged and the diff lists the new column); `merge_pair_creates_one_class_with_two_mappings`; `audit_line_has_no_connection_string`; `match_class_expands_to_each_source` (after apply, `compile_with_ontology("MATCH Customer …")` returns one plan per mapping).
- [ ] Implement; the apply path builds `DefinitionDraft` (T3) from `latest_snapshot` metadata, never from live introspection (the reviewer approves what they saw).
- [ ] **Fuzz/adversarial (T9):** `sources_routes_fuzz` — seeded random bodies (wrong types, nulls, 5 000-element arrays, nesting 10 000 deep, hostile strings, huge ints) for every `/api/v1/sources*` route × {admin, viewer, anonymous, other-tenant}: never a panic, never a 5xx for client errors (400/401/403/404/409 only; 502 only for a genuinely failing source), response always valid JSON with no connection string; `apply_is_idempotent` (second identical apply ⇒ ontology version unchanged); `apply_fuzz_merges` (random merge sets ⇒ draft invariants from T3 hold in the saved ontology); an unknown route under `/api/v1/sources/` ⇒ 404 JSON.

### Task 10: In-server import of chosen containers

**Files:** Modify `crates/services/api/mcp/src/imports.rs` (extract library functions), create `sources/import.rs`; routes in `api_rest.rs`.

**Interfaces — Produces:** `pub(crate) fn import_containers(k: &Kernel, src: &SourceConfig, containers: &[String]) -> Result<ImportReport, String>` with `ImportReport { imported: BTreeMap<String,u64>, skipped: Vec<String>, run_id: String }`, implemented by refactoring `run_pg_import / run_sqlite_import / run_mongo_import / run_neo4j_import` so the body takes a `&Kernel` and returns `Result` (the CLI wrappers keep their messages and `process::exit`). `POST /api/v1/sources/import {source, containers:[…], tenant?}` — admin; refuses containers not in the latest snapshot; stops a container at `import_max_rows` and reports it in `skipped` with `"row cap"`; imported KOs keep `type_name` = the physical container and `tenant = src.tenant`; ClickHouse ⇒ 400 `import not supported for clickhouse`.

- [ ] Tests: existing import tests/certification stay green unchanged (the refactor's guard); `import_sqlite_container_creates_kos_with_physical_type_and_tenant`; `import_unknown_container_400`; `row_cap_is_reported_not_silent` (`import_max_rows = 1`, 3 rows); `import_requires_admin`; `second_import_is_idempotent` (same KOIDs, no duplicate objects — the existing deterministic-identity guarantee).
- [ ] Implement: this is the riskiest refactor — do it first as a no-behaviour-change commit (CLI tests green), then add the REST route.
- [ ] **Fuzz/adversarial (T10):** import a SQLite table filled with seeded random rows — NULLs, i64::MIN/MAX, NaN/±Inf, empty/huge (1 MiB) text and blobs, invalid-UTF-8 text, 50-char unicode identifiers: KO count == row count, values round-trip, a second import is idempotent, the row cap is exact, and importing while the source is being written never corrupts the target; a container named like another tenant's type does not leak across tenants.

### Task 11: Studio — Data Sources panel

**Files:** Modify `studio.rs` (sidebar button `data-panel="sources"` after Ask, `#panel-sources`), `assets/studio/app.js` (`loadSourcesPanel`, `ACTIONS`: `discoverSource`, `previewSourceOntology`, `applySourceOntology`, `importSource`, titles map, `switchPanel`).

**Behaviour:** on open call `GET /api/v1/sources`; one card per source (name, kind, tenant, last snapshot fingerprint + container count, `truncated` badge) with buttons **Discover** (schema only), **Preview ontology**, **Import…** (container checkboxes, hidden unless admin session). Preview shows classes with `New`/`Possible duplicate` badges, mappings, warnings, and per-pair **Merge** checkboxes; **Publish** sends `fingerprints` + `expected_version` + chosen `merges`; a 409 shows "Schema changed — preview again". Every dynamic value through `esc()`; delegated handlers only; no literal `token=`; errors rendered as text. When no `[[sources]]` exist: "No sources configured — see docs/SOURCES.md".

- [ ] Tests: the existing `studio_js_escapes_untrusted_values_before_markup` lint and `studio_handlers_take_data_from_dataset_*` must pass (use `esc(…)` operands or inline `.map(… => '<…').join('')`, as the Ask panel does). E2E (extend `tests/e2e/studio/run.js`; the harness builds two SQLite files with `python3 -c "import sqlite3…"` — `customers`/`orders` and `clients` — and writes two `[[sources]]` plus their env vars into the server's environment): `Data Sources lists both sources`; `Discover shows the container count`; `Preview shows Customer/Order classes, a possible-duplicate for Customer vs clients`; `Publish with a merge makes MATCH Customer return rows from both` (after Import); `a hostile table name is rendered inertly` (a SQLite table named `<img src=x onerror=window.__pwned=1>`); `no CSP violation / console error on the panel`; the existing sweep must still click every control (empty-state buttons must *react*, not be disabled).
- [ ] Implement; run `cargo build -p aikoql-mcp` before the e2e (assets are embedded).
- [ ] **Fuzz/adversarial (T11):** the e2e feeds ten hostile container/column/source names (markup, quotes, `javascript:`, 10 000 chars, RTL) and asserts nothing executes, nothing overflows the layout container, and no CSP/console error appears; keyboard-only operation of Discover → Preview → Publish.

### Task 12: The production-workflow test

**Files:** Create `crates/services/api/mcp/tests/source_discovery.rs` (server spawned with `tests/common::spawn_server`, REST over loopback with the existing helpers).

- [ ] One always-on test `sqlite_two_sources_end_to_end`: configure two SQLite sources (`crm` with `customers(id,email)`, `shop` with `clients(id,email_address)`) ⇒ `discover` both ⇒ `snapshot` KOs exist ⇒ `ontology preview` flags `Customer` vs `Client` not merged ⇒ `apply` with a `merges: [["crm.customers","shop.clients"]]` ⇒ `import` both ⇒ `POST /api/v1/aikoql {"query":"MATCH Customer RETURN *"}` returns the rows of both sources ⇒ alter `crm.customers` (new column) ⇒ re-discover shows the diff and the ontology version is unchanged. One gated test `postgres_mongo_neo4j_end_to_end` behind `Live::*` with the same shape using the ONT-001 fixtures (register in `tests/gated.toml` with an `ungated_by` home if the skip-drift gate demands).
- [ ] Assert at the end that the server log and every response body contain none of the env-var *values* (the DSNs), and that `GET /api/v1/sources` as another tenant returns `[]`.
- [ ] Run: `cargo test -p aikoql-mcp --test source_discovery`.
- [ ] **Chaos (T12):** in the always-on test, delete the SQLite file mid-flow ⇒ discover returns a clean 502, state unchanged; run discover/preview/apply/import concurrently from 4 threads ⇒ final ontology and KO counts equal the serial result.

### Task 13: Adversarial and fuzz capstone

**Files:** Create `crates/services/api/mcp/tests/source_hardening.rs`; add `scripts/fuzz-sources.sh` (runs the fuzz tests with `AIKOQL_FUZZ_ITERS=50000` and prints seeds).

- [ ] Tests against a spawned server with two SQLite sources: `secret_sweep` (every response body, header, audit line, log line and every stored KO property across all routes contains none of the DSN/env values — plant a distinctive canary password in the connection string and `grep` the whole data dir and logs for it, including the WAL files); `read_only_proof` (all source files byte-identical after every route has been exercised, including failed ones); `resource_bounds` (5 000-table source: discover < 10 s, response < 8 MiB, server RSS growth < 200 MiB); `tenant_matrix` (every route × every tenant pairing ⇒ only the owning tenant succeeds, errors identical to "not found"); `crash_consistency` (SIGKILL the server mid-import, restart: no torn KOs, re-import completes, snapshots unaffected); `slowloris_source` (a TCP listener that accepts and never answers, as a Postgres source ⇒ discover fails within `timeout_secs + 2 s`, request thread freed, next request served); `fuzz_matrix` (the T9 route fuzz at `AIKOQL_FUZZ_ITERS`).
- [ ] Anything these find is fixed with its own RED first and ledgered. Report iteration counts and seeds in the PR body.

---

### Task 14 (stretch): apply `property_map` at query time

**Files:** `crates/compiler/src/parser/mod.rs` (:150-200 mapping expansion), `crates/compiler/src/semantic.rs`.

**Spike first (one step, no code kept):** read the expansion at `compile_with_ontology_scoped` and decide whether translating `WHERE`/`RETURN` property names per mapping (canonical → physical via the mapping's `property_map`, inverse of what T3 emits) is a change of ≲ 60 lines. **If not, stop here: leave the limitation documented in `docs/SOURCES.md` ("property names must match across mapped sources until property_map translation lands") and mark this task not done in the PR description.** If yes: RED `match_class_where_uses_each_sources_physical_property` (`MATCH Customer WHERE email = 'a'` ⇒ plan A filters `email`, plan B filters `email_address`) then GREEN; run the full `cargo test -p aikoql-compiler -p aikoql-runtime`.

### Task 15: Docs, PR #31 housekeeping, gates, re-stamp, one push

- [ ] `docs/SOURCES.md`: the `[[sources]]` table (with an example using `url_env`), security model (read-only, no secrets in KOs, no SSRF because the operator names hosts, admin-only, tenant binding), the schema KO types, preview→publish semantics (merge into the manual ontology, fingerprint guard), drift behaviour, import limits (`import_max_rows`), ClickHouse = discovery only, known limits (T14 outcome, Mongo/Neo4j observed schemas are samples), and how to run the live cells.
- [ ] `CHANGELOG.md` entry under `[Unreleased]`; one `AGENTS.md` docs-table row; update the README only if it lists docs.
- [ ] Retitle PR #31: `feat: source schema discovery → canonical ontology (+ ClickHouse benchmark column)`; replace its body with the scope, the "not run locally" list and the T14 outcome (`gh pr edit 31 --title … --body …`).
- [ ] Run every gate in *Global Constraints*; fix to zero; re-stamp as the tip; `bash scripts/check-disposition-head.sh`.
- [ ] `git push origin feature/competitor-clickhouse-restamp` **once**; watch with a single `gh pr checks 31`; fix any CI failure locally and push once more only after the whole failure class is reproduced locally.

## Self-review

- **Spec coverage:** decisions 1→T6; 2→T7 (`Mode`), T10; 3→T8; 4→T2/T4/T7; 5→T1/T3; 6→T3/T9; 7→T9; 8→T9/T6; non-goals respected (no credential entry, no ClickHouse import, T14 stretch).
- **Type consistency:** `ConnectorMetadata`/`FieldInfo`/`ContainerInfo` defined once in T2 and consumed by T3 (`SourceInput`), T7 (adapters), T8 (`record_snapshot`); `SourceConfig`/`find_source` defined in T6, used in T7–T10; `SchemaDiff` from T2 used in T8/T9; `DefinitionDraft` from T3 used in T9.
- **Open risks:** T10 refactor size; T4 live cells need Docker locally; T14 may be dropped; `bump-version`/architecture gates may need the new crate registered (T5 runs them).
