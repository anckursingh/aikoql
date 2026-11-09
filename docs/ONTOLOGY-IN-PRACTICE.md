# The ontology in practice — a device-intelligence case study and the plan it produced

Written 2026-11-09 by walking a real scenario end to end on live databases (ClickHouse 24.8,
Postgres 16) through the same REST routes Studio uses, **not** from the earlier assessment
document. Every finding below was observed; the reproduction is
`scripts/case-device-intelligence/` (seed + walk) and, once Phase F lands, an env-gated live test.

## 1. The customer and their questions

An anti-fraud / device-intelligence team (the shape public vendors describe: a device fingerprint
made of browser, OS, screen, timezone and language components; a stream of visits; risk
decisions) keeps its high-volume telemetry in **ClickHouse** and its accounts in **Postgres**.
The schema used here is **illustrative, built from those public concepts only**; it is not any
vendor's schema.

| table (source) | role | rows |
|---|---|---|
| `devices` (ClickHouse, ReplacingMergeTree by `visitor_id`) | the device fingerprint, versioned by `updated_at` | 3 000 (+50 later versions) |
| `visits` (ClickHouse, MergeTree) | raw telemetry: device, ip, account, time | 200 500 |
| `risk_decisions` (ClickHouse) | allow / challenge / block with a reason | 601 |
| `device_activity` (ClickHouse view) | per-device visits, distinct ips, distinct accounts | 3 000 |
| `accounts` (Postgres, the CRM) | system of record for accounts | 1 000 |

Raw `visits` stay in ClickHouse; AIKOQL is the **knowledge layer**: what we know about a device,
over time, across systems, with provenance. Its four questions:

1. **What do we know about device X over time?**
2. **Which devices are similar to X?**
3. **Which accounts share a device?** (an edge from the CRM to ClickHouse)
4. **The risk engine says *block*, the CRM says *active* — which wins, and why?**

## 2. What the walkthrough found

Discover → preview → publish → import ran cleanly for both sources (5 classes, 7 601 objects).

| question | result | evidence |
|---|---|---|
| 1 history | **Works for tables with a replacing key.** Re-importing a changed device kept three versions; `HISTORICAL` returns 0.97 → 0.97 → 0.99. | the importer keys a replacing engine on its sorting key |
| 1 history (views / plain tables) | **Broken.** One new visit changed one `device_activity` row; re-import produced **3 001 objects for 3 000 devices**: a duplicate, no version chain, stale aggregate left behind. | row identity is a content hash |
| 2 similar | **Partly.** Exact fingerprint equality works (8 devices). `SIMILAR TO` works on imported rows but returns only `koid`/`score`, no properties, and scores are flat (0.467 for many). | `RETURN *` is not hydrated for similarity hits |
| 3 shared device | **Cannot be answered as a graph.** Imported objects have no relationships (`relationships: null`); the ontology declares none (`relationships: []`). The count is there (`distinct_accounts: 5`), the accounts are not. | ClickHouse has no foreign keys; only Postgres FKs become edges |
| 3 cross-source | `LEFT JOIN … ON a == b` works, but only if the right side is a *physical* type (`accounts`); the canonical class `Account` yields `joined: null`. | JOIN does not resolve ontology classes |
| 4 contradiction | **Invisible.** CRM `acct_0077` = active; the risk engine holds four decisions for it including *block*. Nothing links them, nothing flags the disagreement, nothing says which source wins. | no record-level identity or conflict objects |
| authoring | The Ontology panel is a raw JSON/YAML textarea. A customer cannot curate classes, keys, relationships or mappings, nor see the evidence behind them. | `#ontology-yaml-input` |

The shape of the gap: AIKOQL aligns **schemas** (classes, properties) well, and imports **rows**.
What a customer needs in between is the **record layer**: *which rows are the same thing*, *which
rows relate*, and *what do the sources disagree about*, all explainable and reviewable.

## 3. The pattern to adopt: identity as knowledge

`anckursingh/engineering-intelligence` (`internal/identity`) already solved the same problem for
people across GitHub and Jira, and its design carries over:

- **Claims are knowledge objects.** Every resolution is a persistent `SourceIdentity` object
  (`source`, `source_identity` key, `matching_rule`, `confidence`, timestamps,
  `canonical_*` reference) with a `RESOLVES_TO` edge to the canonical entity.
- **Merge only on an exact canonical key** (`login:x`, `email:x`). Weaker evidence stays two
  entities; nothing merges silently.
- **Every decision is explainable**: rule + confidence travel with it (`email` 0.7, `login` 1.0).
- **Deterministic external ids** (`…:identity:<source>:<key>`) make resolution idempotent;
  the canonical reference is denormalised onto the claim so reuse is one read.
- **Cross-source reuse**: a claim from another source for the same key resolves to the same entity.

Mapped here: `Device` keyed by `visitor_id` (or a fingerprint hash), `Account` keyed by
`account_id`/email, claims from ClickHouse and Postgres resolving to one canonical `Account`.

### 3b. What `engineering-intelligence` says AIKOQL must offer

Its `docs/AIKOQL-INTEGRATION.md` and `docs/ONTOLOGY.md` state the contract a real application
puts on AIKOQL: *the ontology belongs to the application; AIKOQL stores objects without
understanding the domain, and gains only **generic** capabilities.* The generic capabilities it
asks for, checked against what an import produces today:

| asked for | an imported object today |
|---|---|
| idempotent write by a stable external id | partial: Postgres/SQLite/Mongo key on the primary key; ClickHouse views and plain tables key on a content hash (Phase A) |
| relationships: create, inbound / outbound / filtered traversal | none from ClickHouse; only Postgres FKs (Phase B) |
| temporal filtering that separates **source time** from **ingestion time** | ingestion time only (`commit_ts`); no `source_updated_at` / `valid_from` (Phase G) |
| provenance: source system, source object id, connector version, ingestion run, observed time | a `src:<source>` tag and `imported`; no source object id, run id or connector version (Phase G) |
| identity decisions that are auditable | none at record level (Phase C) |
| events: subscribe, replay, checkpoint | exists for the kernel; not wired to source change (out of scope here) |

**Phase G — Provenance and bitemporal metadata on imported objects.** Every imported object
records `source_system`, `source_object_id` (the identity key values), `source_updated_at` (a
column the owner names, e.g. `updated_at`), `ingestion_run`, `connector_version` and
`observed_at`, in the object's extensions, so "what did we believe *then*" and "what did the
source say *then*" are different, answerable questions.

## 4. Where this stands against the assessment document

| document section | status | evidence |
|---|---|---|
| PR1 production adapters (introspection → `ConnectorMetadata`) | done | `sources/introspect.rs`, live tests |
| PR2 schema as versioned KOs | done | `aikoql:source-table` queryable; diff on change |
| PR3 domain classes, typed properties, relationships, cross-source suggestions | **partial** | classes/properties/suggestions done; **no declared identity key, no declared relationships between classes** |
| PR4 Studio Data Sources workflow | **partial** | discover/preview/suggest/publish/import exist; no mode choice (schema / sample / ingest), no evidence & confidence view, no conflict resolution, authoring is raw text |
| PR5 ClickHouse provider | done for discovery and import | in-server, bounded, streamed |
| §5.1 schema KOs incl. constraints and relationships | done | indexes, checks, lineage, storage facts |
| §5.2 canonical ontology from structural + data-profile + semantic evidence | **partial** | structural only; no profile or semantic evidence; no confidence on classes |
| §7A source identity ≠ canonical identity | **missing** | no record-level identity (this study, finding 4) |
| §7B schema change → change proposal | **missing** | drift is reported, not proposed |
| §7C declared vs observed authority | **partial** | provenance marked; no authority weighting |
| the end-to-end test over production services | done for Postgres + Mongo; **missing for a cross-source record question** | `source_multidb.rs` |

## 5. The plan

Each phase is RED-first with a real-database acceptance test; the case study is the spine.

**Phase A — Identity keys (fixes finding "views duplicate").** An ontology class declares
`identity: [visitor_id]`. The importer derives the object id from `(source, container, key
values)` instead of a content hash, so a re-imported row *updates in place* and history forms for
views and plain tables alike; rows that vanished from a full import are tombstoned (the kernel
sink already has `prune_missing`). Preview proposes keys from primary keys, unique groups and a
replacing engine's sorting key, with the evidence. *Acceptance:* one new visit ⇒ 3 000 objects,
a second version of one `DeviceActivity`.

**Phase B — Declared relationships (fixes "shared device").** The ontology can declare
`relationship: RiskDecision.visitor_id → Device.visitor_id` (and `account_id → Account.id`) with
a name, domain and range, independent of source foreign keys. Import materialises them as
object relationships whether the source is Postgres, ClickHouse or another system; discovery
*suggests* them from matching key names and value overlap. `JOIN` resolves ontology classes on
both sides. *Acceptance:* `TRAVERSE` from device `vis_00007` reaches its five accounts.

**Phase C — Record-level resolution and conflict (fixes "contradiction").** Adopt §3:
`SourceIdentity` claims with rule and confidence, `RESOLVES_TO` a canonical `Account`, exact-key
merges only, weaker matches proposed for review. A `Contradiction` is recorded when sources
assert different values for one canonical property, resolved by an explicit, declared **source
authority** (the kernel already models authority in `wave5_ka`). *Acceptance:* `acct_0077`
shows CRM *active* vs risk *block*, flagged, with the rule that decides.

**Phase D — A real ontology workbench in Studio.** Replace the textarea as the primary editor:
class cards (identity key, properties with types and provenance, relationships), a physical ⇄
canonical mapping view with confidence and evidence, a relationship editor, publish with the
fingerprints reviewed, schema history (`AS_OF` over `aikoql:source-table`), the schema / sample /
ingest mode choice, and the parked empty-source guidance. Raw JSON stays as an "advanced" tab.

**Phase E — Query ergonomics.** Canonical property names in result rows; `SIMILAR TO` hydrates
properties; a documented "similar devices" recipe (fingerprint components + embeddings of the
component text, with an honest note that scoring is lexical until a real model is configured);
incremental import by an `updated_at` watermark; guidance on the volume pattern (raw telemetry
stays in the source, import entities and aggregates).

**Phase F — The case study as proof.** `docs/CASE-DEVICE-INTELLIGENCE.md` (the story, the
schema, the four questions with their answers), an env-gated live test over ClickHouse +
Postgres, and Studio e2e steps (discover → publish → import → traverse), so the story stays true.

**Order:** A → B → C (the data-model spine, each unlocking one customer question), then D (what
makes it usable by a customer), E, F alongside. A and B first: history and the shared-device
graph depend on them and they are the smallest changes.

## 6. Decisions to confirm
1. **Where identity lives:** a field of the ontology class (proposed) versus a separate mapping
   document. Class-level keeps one place for a customer to read.
2. **Authority:** a ranked list of sources per canonical property (proposed), versus
   per-record timestamps. Ranked sources match how fraud and CRM teams already reason.
3. **Relationship materialisation:** at import (proposed: queryable by `TRAVERSE`, versioned) versus
   resolved lazily at query time (cheaper, but no history of the edge).
