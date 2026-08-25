# AikoQL MVP Test Report

> generated from TESTING-PLAN.md §9.1 by scripts/certify.js

Registry: `docs/TESTING-PLAN.md` §9.1 (MVP-QA-001, 45 test IDs + gates). Statuses are never invented — per the spec execution rules, unimplemented rows stay NOT_IMPLEMENTED/BLOCKED, never PASS.

## Summary

- P0: **26/30 pass (87%)**
- P1: **10/13 pass (77%)**
- Sev-1: 0 · Sev-2: 0
- Benchmarks: pinned baselines present
- **Final decision: NO-GO** (12 blocking items)

## Gate readout

| Gate | Verdict |
| --- | --- |
| P0 correctness | FAIL (87%) |
| P1 correctness | FAIL (77%) |
| Security | PASS |
| Connectors | FAIL |
| Evidence | PASS |
| Evolution | PASS |
| Temporal | PASS |
| Recovery | PASS |
| Docker | PASS |
| E2E | FAIL |

## Per-ID results

| ID | Pri | Gate | Status | Evidence |
| --- | --- | --- | --- | --- |
| MVP-KO-001 Create KO | P0 | G1 | PASS | conformance t01/t04, idempotency keys; MCP query path (dogfood) |
| MVP-KO-002 Update KO | P0 | G1 | PASS | t02 OCC update; temporal current-truth (`update_carries_valid_time_forward`, supersede keeps history) |
| MVP-KO-003 Delete KO + relation | P0 | G1 | PASS | tombstone (t09/t10) + referential policy (t06b–d) + `mvp_ko_003_deleted_endpoint_is_not_exposed_by_traversal` (TDD 2026-08-24: runtime Traverse now drops Deleted/Erased endpoints — red → fix in `crates/runtime/src/lib.rs` Traverse op → green) |
| MVP-KO-004 Idempotent ingestion | P0 | G1 | PASS | INC-001, `idempotency_key` tests, t06 retry-commits-once |
| MVP-EXT-001 Raw evidence 100% addressable | P0 | G5 | PASS | `mvp_ext_001_all_nine_segment_kinds_are_addressable` (TDD 2026-08-24): 9-segment fixture, every segment resolvable via candidate evidence + document_id. Red list was plain bullets + image captions → fixes: `push_bullet_facts` emits plain bullets at reduced confidence (0.5 floor — keeps the G10 pack-budget ranking); payload-less visual blocks keep their alt/caption text as paragraph evidence |
| MVP-EXT-002 Artifact section must not destroy prose | P0 | G5 | PASS | `mvp_ext_002_prose_in_fenced_section_stays_retrievable` (TDD 2026-08-25): Artifact arm emits section paragraphs as facts at 0.5 confidence — the G10 pack-budget negative measurement is preserved via the confidence floor, prose stays retrievable |
| MVP-EXT-003 Formula preservation | P1 | G5 | PASS | `mvp_ext_003_formula_preserved_as_evidence_and_retrievable` (2026-08-25): plain `E = mc^2` line + ```math fence both retrievable with document_id evidence — green without production change |
| MVP-ONT-001 Auto-discovery pg/mongo/neo4j | P1 | — | NOT_IMPLEMENTED | NOT_IMPLEMENTED — needs connectors (TP-5); A9 bridge + fixtures only |
| MVP-ONT-002 Same entity across sources | P0 | G1 | PASS | `multi_source_ontology.rs` config-driven identity (customer 123 across pg/mongo/neo4j/doc fixtures) |
| MVP-ONT-003 Conflicting source values | P1 | G1 | PASS | `epistemic.rs` + e03: conflicting values retained with provenance, no silent choice |
| MVP-CON-001..004 PostgreSQL/MongoDB/Neo4j/PGVector | P0 | G4 | NOT_IMPLEMENTED | NOT_IMPLEMENTED — fixtures + A9 bridge only; connector matrix is TP-5 (scope conflict with §2 of MVP-QA-001 — see 9.3) |
| MVP-CON-005 Source timeout | P1 | — | NOT_IMPLEMENTED | connector-side NOT_IMPLEMENTED; product-side rollback semantics = t06k transact all-or-nothing |
| MVP-CON-006 Auth failure, no secrets in logs | P0 | G3 | OPEN | product auth failures covered (MCP token tests); **TDD**: log-redaction assertion (credentials never in ordinary logs) |
| MVP-CON-007 Outage ≠ deletion | P1 | — | OPEN | repo-side ✅ (`git_change_set_propagates_git_failure`, no-changes → cached); connector-side ❌ with connectors |
| MVP-EVO-001 Modify source | P0 | G6 | PASS | FRESH-001 + INC-002 (`freshness_sla_source_update_to_query_visibility_measured`) |
| MVP-EVO-002 Rename source | P1 | G6 | PASS | INC-003 (`rename_preserves_entity_identity`) |
| MVP-EVO-003 Relationship change A→B to A→C | P0 | G6 | PASS | `mvp_evo_003_relation_change_drops_old_relation` — `drop_changed_relations` drops relations for changed paths (re-parse supplies current) |
| MVP-EVO-004 Delete source | P0 | G6 | PASS | `mvp_evo_004_deleted_source_leaves_no_stale_current_relation` — all-deleted branch now drops relations too (facts keep their [STALE] survival semantics) |
| MVP-EVO-005 Re-ingest 10× no growth | P1 | G6 | PASS | `mvp_evo_005_reingest_ten_times_no_uncontrolled_growth` — 10 incremental cycles ≡ fresh full ingest; [STALE] markers no longer re-appended for re-supplied facts |
| MVP-TEMP-001..004 Historical/current/future/change query | P0/P1 | G7 | PASS | `temporal.rs` (`valid_at` half-open, `as_of`, history in commit order, future validity + invalidation collapse) + `e2e-k2-temporal.js`; change query = history + trace with provenance |
| MVP-SEC-001 Unauthorized access, no leak | P0 | G3 | PASS | t11 default deny, t34 A/B scenario, CTX differential ACCESS_DENIED |
| MVP-SEC-002 Permission propagation every layer | P0 | G3 | PASS | authorize() confinement + ACL-filtered scans (`match_experiences`) + CTX permission differential |
| MVP-SEC-003 Revocation | P0 | G3 | PASS | `revoked_experience_sharing_stops_matching` (share → revoke → not matched) |
| MVP-SEC-004 Sensitive logging | P0 | G3 | PASS | `mvp_sec_004_raw_secret_never_survives_redaction_or_rendering` — R8 now replaces the whole secret-bearing field (statements, snippets, relation endpoints, events, temporal) instead of marker-prefixing; raw value never reaches rendered context |
| MVP-QRY-001..005 Valid/invalid/unknown/injection/determinism | P0/P1 | G1 | PASS | `golden_snapshots.rs`, `grammar_coverage.rs`, `fuzz_parser.rs`, `same_task_twice_renders_identical_context`; unknown-entity = semantic error, healthy-empty = "no authoritative knowledge" (§34–36) |
| MVP-CTX-001 Relevant context | P1 | — | PASS | `retrieval_quality.rs`, ranked packs with snippets + provenance |
| MVP-CTX-002 Irrelevant-fact suppression | P0 | — | PASS | entity gate + keyword hygiene + exact-token escape (G12 row, gate tests) |
| MVP-CTX-003 Entity-anchored retrieval (Customer A/B/C) | P0 | — | PASS | G12 q-00 scenario + entity-gate tests — same shape |
| MVP-CTX-004 Evidence inclusion (answer only in prose) | P0 | G5 | PASS | evidence snippets render verbatim source; `e2e_answer_quality` |
| MVP-CTX-005 Context budget | P1 | — | PASS | `ctx_min_*` 1000-KO minimization tests |
| MVP-REC-001 Restart durability | P0 | G8 | PASS | `durability.rs`, `e2e-restart.js`, chatbot real-server restart |
| MVP-REC-002 Backup/restore | P0 | G8 | PASS | `mvp_rec_002_backup_destroy_restore_round_trip` (mcp_real_world): verified live backup → destroy → restore → reopen → same KOID + content; engine-level `snapshot_to`/`restore_from` (trait + `EncryptedStore` raw delegation so ciphertext stays verbatim); **server restart required after restore** (in-memory derived state) |
| MVP-REC-003 Interrupted ingestion | P0 | G8 | PASS | `crash_kill.rs` (taskkill/SIGKILL mid-write → consistent reopen, journal head ≥ observed) |
| MVP-DEP-001 Clean Docker startup | — | G9 | PASS | ci.yml docker job: build → run → health check healthy |
| MVP-DEP-002 Fresh install → ingest → query | — | G9 | PASS | `e2e-dogfood.js` CI job (documented instructions) |
| MVP-DEP-003 Persistent container restart | — | G9 | PASS | `scripts/e2e-volume-restart.js` (CI docker job): container A remembers a KO on a named volume, container B (fresh container, same volume) resolves the same KOID + content; green without a production change — pins the Dockerfile /data contract |
| MVP-BENCH-001..003 G10/G11/G12 no regression | — | G12 | PASS | canonical baselines pinned in §3 rows 128–130 + weekly bench regression CI (>20% alert) |
| Suite N Agent memory | — | — | PASS | §32 `agent_memory_bench` (D 20/20 vs B 12/20); not an MVP blocker per the spec |
| MVP-PRG-001 Program representation | P1 | — | PASS | MEM-005 (`experiences.rs`: identity/inputs/outputs/permissions/pre/post/side effects/provenance) |
| MVP-PRG-002 Unauthorized program not selectable | P0 | G3 | PASS | PRG-004 + t12 ACL + denied execution |
| MVP-PRG-003 Invalid program metadata rejected | P1 | — | PASS | `record_experience_rejects_invalid_program_metadata` — incomplete shape (goal/action/outcome), TTL overflow, NaN/out-of-range confidence all rejected deterministically; no partial write |
| MVP-E2E-001 PostgreSQL → KO → query | P0 | G4 | NOT_IMPLEMENTED | NOT_IMPLEMENTED — connectors (TP-5) |
| MVP-E2E-002 Document → evidence → KO → query | P0 | G5 | PASS | `e2e_pipeline.rs` + `e2e_answer_quality` (answer grounded in document evidence) |
| MVP-E2E-003 Repository → KB → query | P0 | — | PASS | `e2e-dogfood.js` + G10 D treatment |
| MVP-E2E-004 Multi-source query | P0 | G4 | OPEN | fixture-level ✅ (`multi_source_ontology.rs` merges pg/mongo/neo4j/doc fixtures); live-connector ❌ |
| MVP-E2E-005 Permissioned multi-source | P0 | G3 | PASS | CTX permission differential + t34 (unauthorized source cannot enter context even when referenced) |
| INV-001..010 invariants | — | G14 | OPEN | no-orphan (t06b/c), idempotence, provenance, evidence, authz closure, temporal consistency, atomicity (t06k), restart, determinism ✅; INV-010 source isolation = MVP-CON-007 (repo ✅ / connector ❌) |
| §23 Artifacts (artifacts/ + release-gate.md) | — | — | PASS | TDD 2026-08-25: `scripts/certify.js` regenerates the 5 artifacts + the release-gate verdict from this registry; `--self-test` pins the decision logic on fixtures, `--check` fails CI on stale artifacts |

See `failed-tests.md` for the non-pass rows and their root-cause notes.
