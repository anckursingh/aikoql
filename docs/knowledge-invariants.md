# AIKOQL Knowledge Invariants

The invariants every AIKOQL operation must preserve. This is the contract the
kernel enforces on every write path, and the checklist any change to the
kernel or protocol surface must keep green (review PR #1, item 16).

The overarching question (review final assessment):

> Can any API call, concurrent transaction, authorization mistake, crash,
> temporal edge case, or conflicting evidence cause AIKOQL to return a
> knowledge state that violates its own invariants?

Target answer: **no**. Each invariant below names its enforcement site(s)
and its verification (kernel test / MCP test / e2e script).

---

## Epistemic

**E1. Illegal epistemic transitions are rejected.**
The `EpistemicStatus` state machine is the only legal move path;
`can_transition` gates every change.
- Enforced at: `crates/kernel/src/knowledge/kom.rs:1385` (machine),
  `crates/kernel/src/transaction/kernel.rs:2002` (`transition_epistemic_locked`),
  `crates/kernel/src/transaction/kernel/ops.rs:1228` (`transition_claim_if_legal`).
- Verified: `crates/kernel/tests/epistemic.rs`, `tests/transactions.rs`
  (`verify_rejects_illegal_epistemic_moves`, `supersede_rejects_already_superseded`,
  `invalidate_requires_evidence_and_is_rejected_when_already_invalidated`).

**E2. Every transition appends history and emits the correct event.**
Transitions go through the versioned commit pipeline: a new version, an
`EpistemicChanged` audit event, and an audit-hash chain entry.
- Enforced at: `crates/kernel/src/transaction/kernel/ops.rs` transition calls
  → `remember_locked` → `commit_version` (`kernel.rs:1005-1077`).
- Verified: `tests/transactions.rs` (`knowledge_continuity_kafka_to_rabbitmq`
  step 10: versions `[1,2,3]`, step 11: verification event note preserved).

**E3. There is exactly one protocol path to each status.**
`verified` only via `verify_knowledge` (evidence mandatory), `superseded`
only via `supersede`/`resolve_conflict`, `contradicted` only via
`contradict`/`invalidate`/resolution. The generic
`admin_transition_epistemic` is a **library-level privileged primitive
only** (review P0-2 — the `admin_` prefix is the contract, and the op is
documented as explicitly privileged) — it is not exposed on any protocol
surface (MCP/REST/shell).
- Enforced at: `crates/kernel/src/transaction/kernel.rs:2044` (doc contract);
  `crates/services/api/mcp/src/tool_registry.rs` (no registration — the
  former tool was deleted, review P0-1).
- Verified: `crates/services/api/mcp/tests/mcp_stdio.rs` m01 (tool list),
  k1 step 5 (raw `transition_epistemic` call is an error).

**E4. Kernel-managed extension keys cannot be forged through `remember()`.**
The public `remember()` boundary rejects any request carrying a
kernel-managed extension key (epistemic status/history, lifecycle history,
invalidation, evidence, derivation, confidence, valid_to, authority,
scope, content trust) — epistemic state enters only via the semantic ops.
`Kernel::KERNEL_MANAGED_EXTENSIONS` is public so callers can strip these
keys from a read-modify-write update; the epistemic block is carried
forward automatically by plain updates. `valid_from` is deliberately
caller-settable — the caller's own temporal claim.
- Enforced at: `crates/kernel/src/transaction/kernel.rs:1092`
  (`KERNEL_MANAGED_EXTENSIONS` + guard at the head of pub `remember()`;
  internal semantic ops route through `remember_trusted`).
- Verified: `tests/evidence_wiring.rs`
  (`create_stamps_authority_and_scope_by_origin` rejection block,
  `authority_is_monotonic_up_without_admin`), `mcp_stdio.rs` k1 step 1b
  (protocol-boundary rejection).

## Evidence

**EV1. Evidence is mandatory for every knowledge claim.**
`observe`, `assert_knowledge`, `verify_knowledge`, `contradict`,
`supersede`, `invalidate`, `merge` (with folded properties),
`record_experience` all reject an empty evidence list — an unbacked claim is
rejected, not downgraded.
- Enforced at: `crates/kernel/src/transaction/kernel/ops.rs:315`
  (`require_evidence`), called at the head of each semantic op.
- Verified: `tests/transactions.rs` (`observe_requires_evidence_...`,
  `assert_requires_evidence_...`, `verify_is_not_a_status_flip`,
  `invalidate_requires_evidence_...`), `tests/experiences.rs`.

**EV2. Evidence is append-only (and deduped).**
Evidence is never replaced or dropped: semantic transitions append new
evidence through one helper, `append_evidence`, which drops exact
duplicates (review P2-3). The R12 head-prefix check remains in
`remember_locked` as internal defense-in-depth.
- Enforced at: `ops.rs:373` (`append_evidence`), used by
  verify/supersede/invalidate.
- Verified: `tests/transactions.rs`
  (`supersede_with_superseded_by_links_existing_successor` — evidence len 2
  on the old claim), `tests/evidence_wiring.rs`
  (`evidence_is_append_only_on_update` — re-verifying the same evidence is
  idempotent and not double-counted).

**EV3. Confirmations are independent per verifier (review P2-4).**
`verify_knowledge` keys each confirmation by `verifier | evidence`,
recorded in `verification_keys` — the same verifier re-verifying the same
evidence adds nothing; a distinct verifier adds one confirmation.
- Enforced at: `ops.rs:359` (`confirmation_key`), keyed counting in
  `verify_knowledge`.
- Verified: `tests/transactions.rs`
  (`verify_bumps_confirmations_and_never_lowers_score`),
  `tests/evidence_wiring.rs` (`evidence_is_append_only_on_update`).

**EV4. Epistemic-critical reads decode evidence strictly.**
`trace` reads through `strict_evidence()`: a malformed evidence entry is a
surface error, never silently skipped (review P2-6), and each source's
status is reported as `ok` / `not_found` / `not_visible` (review P2-7).
- Enforced at: `kom.rs:934` (`strict_evidence`), `mcp/src/tools/query.rs`
  (`tool_trace`).
- Verified: `mcp_stdio.rs` k3 trace section.

**EV5. Evidence correction is a future model, not silent mutation.**
Correction/supersession of evidence (`E1 -> SUPERSEDED_BY -> E2`, review
P2-10) is deferred: the append-only record leaves room for it, and the
relationship index already supports arbitrary typed edges.

## Temporal

**T1. Valid time is a half-open interval `[valid_from, valid_to)`.**
`valid_to` is exclusive; `valid_from <= valid_to` when both exist —
inversion is rejected at stamp time (review P1-1), equality is a legal
zero-duration interval (a claim closed at its own assertion instant, or a
future fact collapsed before it became valid: valid at no instant).
- Enforced at: `kom.rs` (`valid_at`, `set_valid_time`) and
  `kernel.rs:1243` (interval check in `remember_locked`).
- Verified: `crates/kernel/tests/temporal.rs`
  (`inverted_interval_is_rejected_zero_duration_is_legal`),
  `tests/experiences.rs` (`match_experiences_filters_expired` — gone
  exactly at `valid_to`).

**T1b. Future facts collapse, never extend.**
Invalidating or superseding a fact whose `valid_from` lies in the future
closes the interval at `max(valid_from, now)` — the fact is never valid at
any instant (review P1-1).
- Enforced at: `kom.rs:1115` (`close_valid_time`) — the single
  validity-closing path, used by supersede/invalidate/transitions.
- Verified: `tests/temporal.rs`
  (`invalidating_a_future_fact_collapses_it_to_never_valid`).

**T2. `None` bounds are unbounded, never `0`.**
`None valid_from` = −∞, `None valid_to` = +∞. `0` is a legitimate
timestamp, not the semantic representation of the unbounded past (review
P0-2). The BETWEEN filter is Option-driven on both sides.
- Enforced at: `crates/runtime/src/lib.rs:365` (BETWEEN overlap check).
- Verified: `crates/runtime` `between_boundary_matrix_and_unbounded_sides`.

**T3. AS_OF/HISTORICAL = transaction time; BETWEEN = valid time.**
`MATCH` defaults to valid-at-now; `AS_OF`/`HISTORICAL` reconstruct committed
transaction-time versions; `BETWEEN` filters valid-time overlap.
- Enforced at: `crates/runtime/src/lib.rs` (query filter dispatch).
- Verified: `mcp_stdio.rs` k2, `scripts/e2e-k2-temporal.js`,
  `scripts/e2e-dogfood.js` Q2/Q3.

## Derivation

**D1. Every derived KO has provenance.**
`derive` and `merge` stamp the derivation record (operation, actor, reason,
sources, timestamp) and wire `DERIVED_FROM` edges to every source.
- Enforced at: `kernel.rs:2311` (`derive`), `ops.rs:870` (`merge`).
- Verified: `tests/transactions.rs`
  (`merge_is_a_first_class_derivation_with_property_folding`),
  `scripts/e2e-dogfood.js` Q4-Q6.

**D2. Every source KO exists and is readable at derivation time.**
- Enforced at: `kernel.rs:2311` (derive validates each source),
  `kernel.rs:1266-1275` (strict referential policy).

**D3. Confidence scores are validated at the model boundary (review P1-7).**
`ConfidenceContext::new` rejects non-finite and out-of-range scores
(`!(0.0..=1.0).contains(score)`) — a bad score is a rejection, never a
silent clamp.
- Enforced at: `kom.rs:1317` (`ConfidenceContext::new`), called by
  derive/verify/record_experience and the MCP tools.
- Verified: `tests/derivation.rs`
  (`confidence_context_rejects_non_finite_and_out_of_range_scores`).

**D4. Derived knowledge inherits source evidence, never full trust
(review P1-8, Model B).**
When a derivation supplies no evidence of its own, the derived KO inherits
the sources' strict evidence trails; an evidence-less source contributes
nothing, and no source context yields an explicit low-confidence baseline
(0.0), never implicit full trust.
- Enforced at: `kernel.rs` `derive` (evidence inheritance + confidence
  baseline).
- Verified: `tests/derivation.rs`
  (`confidence_baseline_comes_from_sources_never_silently_full`),
  `tests/transactions.rs`.

## Invalidation

**I1. Invalidating a premise invalidates affected derived knowledge.**
BFS over outbound `DERIVED_FROM` stamps every dependent: invalidation stamp
(actor/at/reason) + `valid_to = now` — never an epistemic status change.
- Enforced at: `ops.rs:1267-1334` (`invalidate_dependents_locked`).
- Verified: `tests/transactions.rs`
  (`invalidate_contradicts_target_and_sweeps_derivation_chain`,
  `knowledge_continuity_kafka_to_rabbitmq` steps 8/12),
  `scripts/e2e-dogfood.js` Q7/Q8.

**I2. The sweep is cycle-safe, dedup-safe, and idempotent.**
Collection happens before any mutation (collect-then-stamp, review P1-7);
a visited set bounds cycles; duplicate edges collapse to one stamp;
already-stamped nodes stop the walk.
- Enforced at: `ops.rs:1275-1298` (Phase 1), `ops.rs:1304-1309` (Phase 2
  re-check).
- Verified: `tests/transactions.rs` (`sweep_terminates_on_derived_from_cycles`,
  `sweep_collapses_duplicate_edges_to_one_stamp`,
  `repeated_sweep_is_idempotent_per_dependent`).

**I3. The sweep is authorization fail-closed.**
Each dependent stamp routes through `remember_locked`, which authorizes
Write for the initiating subject — a subject that may not write a dependent
cannot stamp it either.
- Enforced at: `kernel.rs:1260-1263` (update-path authorization).
- Verified: `tests/transactions.rs`
  (`resolve_replaced_wires_supersedes_edges_and_sweeps_dependents` runs a
  cross-principal sweep; the denied case is covered by the ACL tests).

**I4. Documented limitation: no cross-KO transaction.**
All ops run under the single pipe lock, which serializes concurrent
operations; the store layer has no multi-key atomic commit, so a storage
failure mid-sweep can leave earlier stamps committed (fail-safe direction:
stamps are conservative, never phantom). Documented at
`ops.rs:1299-1303`.

**I5. Sweep outcomes are structured, never silent (review P1-5).**
`supersede` / `invalidate` / `resolve_conflict` return `completed: bool`
and `failed: [{koid, error}]` alongside the stamped set — a partial sweep
is reported per dependent, not folded into a blanket failure.
- Enforced at: `ops.rs` (`InvalidationFailure`, `SweepOutcome`, flattened
  into `SupersedeResult` / `InvalidationResult` / `ConflictResolutionOutcome`).
- Verified: `mcp_stdio.rs` k2/k4 (response shape includes `completed` and
  `failed`).

## Conflict

**C1. Conflict resolution is explicit and recorded.**
Resolution requires an unresolved Conflict KO, a real decision (never
`Unresolved`), and a non-blank rationale; the Conflict KO records decision +
rationale (+ replacement for `ResolvedReplaced`).
- Enforced at: `ops.rs:1014` (`resolve_conflict`).
- Verified: `tests/transactions.rs`
  (`resolve_conflict_validates_decision_rationale_and_state`,
  `resolve_conflict_applies_decision_and_records_rationale`).

**C2. Equal-authority conflicts cannot be silently resolved.**
Authority-ranked resolution errors on a tie — never a silent pick.
- Enforced at: `ops.rs:1167` (`resolve_conflict_by_authority`).
- Verified: `tests/transactions.rs`
  (`authority_resolution_ranks_snapshots_and_rejects_ties`).

**C3. `ResolvedReplaced` performs full successor semantics (review P0-3).**
Both claims are superseded with `SUPERSEDES` edges to the replacement, the
replacement is pre-validated (exists, readable, current), and dependents of
both claims are swept.
- Enforced at: `ops.rs:1087` (ResolvedReplaced arm reusing the supersede
  machinery + `validate_successor` at `ops.rs:1203`).
- Verified: `tests/transactions.rs`
  (`resolve_replaced_wires_supersedes_edges_and_sweeps_dependents`).

**C4. Every assertion carries an authority; defaults never inflate it
(review P1-3/P1-4).**
`contradict` always stamps an authority — the explicitly supplied level
(validated) or the origin-derived default (`agent_derived` for agent
assertions), never inheriting the contradicted claim's higher authority.
Authority-ranked resolution requires a recorded authority on both sides:
a missing authority fails closed with `InvalidObject`, never ranks as 0.
- Enforced at: `ops.rs` (`contradict` authority stamp),
  `resolve_conflict_by_authority` (`snapshot_authority_rank` → Option).
- Verified: `tests/transactions.rs`
  (`contradict_stamps_origin_derived_authority_by_default`).

**C5. `ResolvedBothValid` partitions validity atomically or not at all
(review P2-2, shipped 2026-08-29).**
A `split_at` instant (both-valid only — any other decision carrying one is
`InvalidObject`) closes claim A's interval at the instant and opens claim
B's there, preserving each claim's other bound and leaving both epistemic
statuses untouched. Both new intervals are validated before either claim is
written, so an inverted split leaves the claims unmodified; the Conflict KO
records `resolution_split_at`.
- Enforced at: `ops.rs` (`partition_validity_locked` — validate-both-then-
  write under the pipe lock).
- Verified: `tests/transactions.rs`
  (`resolve_both_valid_splits_validity_at_split_at`,
  `resolve_both_valid_without_split_is_bare_coexistence`,
  `resolve_split_at_rejects_inverted_intervals_and_other_decisions`).

**C6. A verification names its journal event (review P2-5, shipped
2026-08-29).**
Every `verify_knowledge` commit stamps the kernel-managed `verified_event`
extension with the journal seq of the verify op's final commit — the event
that carries the confidence bump, whose koid and actor identify the
verification in the audit journal. Callers cannot forge the link:
`verified_event` is in `KERNEL_MANAGED_EXTENSIONS`.
- Enforced at: `ops.rs` (`verify_knowledge` — `journal_head + 1` under the
  single-writer pipe lock).
- Verified: `tests/transactions.rs`
  (`verify_stamps_the_verify_commit_journal_seq`).

## Experience

**X1. Expired, invalidated, or superseded experiences are never returned.**
- Enforced at: `ops.rs:1477` (eligibility gate `valid_at(now)` +
  invalidation stamp).
- Verified: `tests/experiences.rs` (`match_experiences_filters_expired`,
  `invalidated_experiences_are_not_matched`).

**X2. ACL is checked before ranking/retrieval.**
The candidate scan is ACL-filtered (read authorization per KO), and the
eligibility gate runs before any scoring — an inaccessible or ineligible
experience never enters the ranking stage (review P1-8/P1-9).
- Enforced at: `ops.rs:1476` (ACL-filtered scan), `ops.rs:1477-1506`
  (eligibility before scoring).
- Verified: `tests/experiences.rs`
  (`match_experiences_respects_shared_with_acl`,
  `revoked_experience_sharing_stops_matching`).

**X3. Sharing is opt-in and revocable.**
Cross-agent reuse requires `shared_with` Read grants; a later `remember`
with an explicit security descriptor replaces the ACL (kernel-managed keys
stripped per E4), and revoked principals stop matching immediately.
- Enforced at: `ops.rs:1414-1430` (grant construction),
  `kernel.rs:1264` (security replace on update).
- Verified: `tests/experiences.rs` (`revoked_experience_sharing_stops_matching`).

**X4. TTL arithmetic cannot overflow (review P1-6).**
`ttl_seconds` is converted to a validity end via checked math — an
overflowing TTL (`u64::MAX` seconds) is rejected, never wrapped into an
unbounded future.
- Enforced at: `ops.rs` (`record_experience`, checked_mul/checked_add).
- Verified: `tests/experiences.rs` (`record_experience_rejects_ttl_overflow`).

## Encryption

**K1. Encrypted data cannot exist without recoverable key metadata.**
Wrapped DEKs are persisted inside the store before/with the first
field-encrypted commit; a corrupt or unreadable DEK record fails the open
(fail-closed — never silently mint a fresh DEK that orphans ciphertext).
- Enforced at: `kernel.rs:686-710` (DEK load + crypto-meta check on open),
  `kernel.rs:1428` (DEK persist on remember).
- Verified: `tests/encryption.rs` e09 (restart roundtrip), e10 (corrupt DEK
  fails open).

**K2. Wrong credentials fail closed.**
`LocalKms` derives the KEK from the passphrase via Argon2id → ChaCha20-
Poly1305 AEAD; a wrong passphrase (or tampered envelope) fails
authentication — never yields a key.
- Enforced at: `crates/kernel/src/security/kms.rs:217`
  (`decrypt_v2_envelope` → `InvalidPassphrase`).
- Verified: `tests/encryption.rs` e11.

**K3. Tampered field ciphertext fails closed.**
Field values are AES-256-GCM AEAD ciphertexts (`version || nonce || ct ||
tag`); any modification fails authentication at decrypt — the kernel can
never surface a phantom plaintext.
- Enforced at: `crates/kernel/src/security/field_crypto.rs` (encrypt/decrypt).
- Verified: `tests/encryption.rs` e13.

**K4. Key purposes are domain-separated (review P0-4).**
One KEK/DEK is never used raw for two purposes: HKDF-SHA256 (RFC 5869)
derives `aikoql/dek-wrap/v1` (KEK→DEK wrap), `aikoql/store/v1` (store
key), `aikoql/field/v1` (field key).
- Enforced at: `crates/kernel/src/security/hkdf.rs` (verified against the
  RFC 5869 A.1 test vector), `envelope.rs` (wrap/unwrap),
  `field_crypto.rs`, `crates/services/api/mcp/src/engine.rs`.
- Verified: `hkdf.rs` tests (`rfc5869_test_vector_a1`,
  `domain_separation_yields_distinct_keys`).

**K5. Crypto scheme is versioned; unknown versions fail closed (review P2-14).**
The store carries a crypto-meta record (`__encryption__/meta`) stamped on
first encrypted open and verified on every later open; an unknown version
is an explicit error, never a guess.
- Enforced at: `kernel.rs:695-708`, `envelope.rs:29` (`CRYPTO_META_V1`).
- Verified: `tests/encryption.rs` e12.

## Capability separation (review P1-5)

**A1. Epistemic decisions are separate capabilities.**
`verify_knowledge` requires the `verifier` role, `invalidate` requires
`operator`, `resolve_conflict`/`resolve_conflict_by_authority` require
`arbiter` — over the MCP/REST gateway. Unauthenticated stdio sessions and
`admin` retain unrestricted access (the OS process boundary is the trust
boundary); a role-less **TCP** session is fail-closed for every tool
(review P1-10). Direct kernel-library callers (embedded use) authorize at
the ACL layer, which is always enforced.
- Enforced at: `crates/services/api/mcp/src/authz.rs`
  (restricted table + trust-mode-aware passthrough) + kernel ACL
  authorization on every semantic op.
- Verified: `authz.rs` `capability_separation_of_duties`,
  `mcp_stdio.rs` (full suite green with the new table).

**A2. Provenance actors are bound to the session identity (review P1-9).**
Protocol tools (`derive`, `record_experience`, ...) bind the derivation/
experience actor to the authenticated session subject (`subject_of(args).name`,
injected before dispatch — on TCP forced to the token-assigned agent id).
A caller-supplied `actor` argument is ignored, so provenance cannot be
spoofed through the protocol boundary.
- Enforced at: `mcp/src/tools/knowledge.rs` (actor binding),
  `mcp/src/dispatcher.rs` (session identity injection).
- Verified: `mcp_stdio.rs` k3 (caller passes `actor: "agent-7"`; the
  stamped actor is the session subject).
