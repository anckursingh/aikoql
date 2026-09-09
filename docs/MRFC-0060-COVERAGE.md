# MRFC-0060 Coverage Table — P3-M5 (constraint engine modes/severity/cross-object, §64–66)

One row per MRFC-0060 section touched by P3-M5, status = IMPLEMENTED / DESCoped /
PRIOR (shipped before P3-M5 — cited, not re-measured). Evidence names the pin.

## Implemented in P3-M5

| § | Subject | What shipped | Evidence |
| --- | --- | --- | --- |
| §16 | Cardinality | `CardinalityConstraint { name, relationship_type, min_outbound, max_outbound, mode, severity }` — counts Direction::Outbound rels of the bound type on the written KO; min violated when fewer, max when more; other rel types don't count | cst003 (max + min block, "c_member"/"c_dept" in errors, unrelated rels ignored, advisory records via `violation_events()`); `evaluate_cardinality` next to `evaluate_full` in `remember`/`transact` |
| §22 | Cross-type constraints | A constraint binds its schema's type only — the same relationship shape on another type violates nothing (cross-type discrimination) | cst004 (Person blocked, Team allowed with the same rels) |
| §24 | Temporal constraints | `TemporalConstraint { name, start_property, end_property, mode, severity }` — start ≤ end ordering only. Int compares numerically, Text lexicographically (ISO-8601 orders correctly), Null bound skips, mixed types = violation event, missing bound = skip | cst005 |
| §30 | Enforcement modes | ENFORCED / VALIDATED → violation blocks (`result.valid = false`); ADVISORY → warning recorded, write succeeds; DISABLED → skipped entirely. Per unique + check + cardinality + temporal constraint | cst001 mode matrix; cst006; `ConstraintEvaluator::dispatch` |
| §31 | Severity | `ViolationSeverity { Error, Warning, Info }` stamped per-constraint on every `ViolationEvent` (= `ConstraintViolation` with constraint_name/message/severity/mode/timestamp/koid); severity classifies, mode governs blocking | cst002 catalog + builder defaults |
| §36 | Constraint dependency graph | No explicit graph structure — write-set filtering + counters give incremental evaluation: `ConstraintEvalStats { evaluated, skipped_disabled, skipped_unaffected }`, only constraints whose referenced properties intersect the write-set run | cst006 (write-set filter, empty-write-set skim, kernel delta pin — an unaffected write never moves `evaluated`) |
| §30 (ADVISORY) | Advisory write-through | `ConstraintResult::into_kresult()` returns Ok whenever `valid` — "Write succeeds but violation is recorded" | cst001 advisory-only write succeeds with `warnings[0]` recorded |
| §30 + plan | Zero-overhead DISABLED | `Schema::has_enabled_constraints()` gates the evaluator in the kernel — an all-Disabled schema never invokes it | cst007 (duplicate email + impossible predicate both succeed, `evaluated == 0`, empty event ring) |
| — (plan) | Diagnostics surface | Kernel violation-event ring (VecDeque cap 256, oldest evicted) + MCP `constraint_diagnostics` tool returning events[] + stats{} | cst002/003 via `k.violation_events()`; MCP 131/0; full-loop dogfood through the repo-built plugin binary (`mcp-constraint-diagnostics-smoke.mjs` GREEN): register_schema → advisory write succeeds → enforced-unique duplicate blocked → events carry mode/severity/koid |
| — (plan) | MCP schema registration | `register_schema` tool: properties/uniques/checks/cardinality/temporal parsers with mode/severity/scope/timing string enums (defaults Enforced/Error/Type/Immediate), check exprs via `CheckExpression::parse` | MCP `register_schema_tool_drives_constraint_diagnostics` (advisory check records event, enforced unique blocks); dogfood loop above |
| — (REC-002) | Persistence | Schema codec v2 (`b"SCH2"` magic + v1 body + per-unique/per-check mode+severity + cardinality + temporal sections); decode tries v2 then falls back to v1 positional (defaults Enforced/Error, empty new sections) — old DB rows keep loading | `schema_round_trip_full_and_canonical` (mode/severity round-trip + canonical re-encode), `schema_v1_rows_fall_back_with_defaults` (hand-built v1 row → defaults → re-encode is v2) |
| §16/§24/§30 (deferred) | Deferred constraints honor mode/severity at commit | `evaluate_deferred` looks up each deferred unique/check's mode+severity from the schema and dispatches accordingly; Disabled deferred constraints never collected | regression: deferred suites green; dispatch shared with the immediate path |

## Prior (shipped pre-P3-M5, cited)

| § | Subject | Where |
| --- | --- | --- |
| §10/§11 | Nullability, requiredness, defaults | `SchemaRegistry::validate` (C1/C7) |
| §14 | Uniqueness | `check_uniqueness` (C2), scopes Type/Tenant/Global — now mode/severity-aware (M5a) |
| §18 | Domain constraints | Range/Length/Enum/Pattern/Format (C1) — deliberately no mode field: always Enforced (documented) |
| §19 | Check constraints | `CheckExpression` tree (C5) — now mode/severity-aware (M5a) |
| §28 (partial) | Provenance | `provenance_required` + trusted-source checks in `evaluate_full` (AC-17) |
| §30 (subset) | Not-null/check/unique pushdown | `constraint_caps` C6 connector pushdown paths unchanged |

## Descoped — accepted limitations (KSE-style closure)

| § | Subject | Limitation | Mitigation / reopen trigger |
| --- | --- | --- | --- |
| §20 | Cross-property constraints | No dedicated class — expressible as check expressions over multiple `Property` refs | Reopen if a constraint shape needs evaluation beyond check-expression semantics |
| §21 | Cross-object constraints | Generic cross-object predicate class not built; cardinality + uniqueness scopes cover the shipped cross-object needs | Reopen when a cross-object invariant can't be expressed as cardinality/uniqueness |
| §22 | Cross-type constraint classes | No constraint that spans two schema types — per-type binding only (pinned by cst004) | Reopen on a concrete multi-type invariant requirement |
| §23 | Graph constraints | Acyclic/symmetric/transitive/relationship-uniqueness not built | Reopen with a named consumer (MRFC-0030 workflows were the HLD's motivation) |
| §24 | Temporal non-overlap intervals | Ordering only; interval-overlap checks not built | Reopen when a consumer needs interval algebra |
| §25 | Bitemporal constraints | Not built | Reopen with a bitemporal consumer |
| §26/§27/§29 | Security / tenant / confidence constraints | Outside P3-M5 scope | Per-phase, if the HLD's later phases land |
| §30 | INFERRED mode | Four modes only; no inference-deduction mode | Reopen with the inference engine (C8) |
| §32–§35 | Constraint provenance, constraints-as-KO, programs-as-KO, constraint compilation | Outside P3-M5 scope | Per-phase |
| §36 | Explicit dependency-graph structure | Replaced by write-set filtering + counters (no persisted graph) | Reopen if an optimization needs static dependency ordering |
| — | Domain-constraint modes | Domain constraints have no mode field (always Enforced) — the domain loop stays outside dispatch | Reopen if a per-property advisory domain check is demanded |
