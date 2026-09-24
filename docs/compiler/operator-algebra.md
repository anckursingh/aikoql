# Operator algebra — planner rewrites (v1)

The planner (`crates/compiler/src/planner.rs`) applies three rule-based
rewrites in a fixed pass order:

```text
optimize(plan) = dedup_scans(pushdown_filters(merge_filters(plan.operators)))
```

Each pass sees the previous pass's output. v1 is rule-only; cost-based
optimization (CBO) arrives with P5-M9, when workload statistics exist.

Each rewrite below states its operation, preconditions, proof sketch, and the
test that pins it. The binding contract (TESTING-PLAN-PHASE5 rule 8): any new
planner rewrite needs a section here, a pinning test, and an oracle corpus row
(gate 6). `optimize()` never reorders Scans, never drops a non-Scan operator,
and never dedups across an intervening operator.

## 1. merge_filters

**Operation:** `Filter{p1}, Filter{p2} → Filter{p1 ++ p2}` (predicates
concatenated), for consecutive Filters.

**Preconditions:** the two Filter operators are adjacent in the pipeline — no
intervening operator.

**Proof:** Filter is conjunctive predicate application over the current row
set. Applying `p1` then `p2` to R yields `{r ∈ R | p1(r) ∧ p2(r)}` — a single
Filter with the concatenated predicate list. Predicate evaluation is
deterministic and side-effect-free, so concatenation preserves the result set
exactly.

**Pinned by:** `merge_two_filters` (planner.rs tests); alg002
result-preservation proptest (generated Filter chains through `optimize()`
vs the unoptimized executor, oracle divergence 0).

## 2. pushdown_filters

**Operation:** `Filter, Search → Search, Filter` (swap order), where Search
is `AnnSearch` or `TextSearch`.

**Preconditions:** adjacency. The direction is **fixed**: Search-then-Filter
is the chosen semantics (TDD-COMP-003) — rank the full candidate set, then
filter. The reverse order would restrict the candidate set before ranking and
change ANN/text recall, which is index-dependent.

**Proof:** execution semantics are defined as rank-then-filter; the rewritten
order matches the definition. Equivalence holds by definition of the Search
operator's semantics — this is a directed rewrite implementing the defined
semantics, **not** a commute.

**Pinned by:** `ppl006` (planner.rs tests).

**MUST-NOT:** reversing the direction (Filter-before-Search) changes recall —
any future rule doing so needs a gate-6 corpus oracle row first.

## 3. dedup_scans

**Operation:** `Scan(s1), Scan(s2) → Scan(s1)` when `s1` and `s2` are
consecutive and identical.

**Preconditions (both required):**

1. **Full-tuple identity** — `type_name`, `subject`, `roles`, `tenant` ALL
   equal. The destructure in `dedup_scans` is the compile-time guard: adding a
   new field to `IrOp::Scan` makes this rule a compile error, never a silently
   ignored security dimension.
2. **Adjacency** — no intervening operator. The row set is ACL-resolved per
   subject, so two Scans differing in any dimension MUST NOT dedup: a deduped
   plan would serve one subject's rows under another subject's query context
   (the P0 this closed, P4-M1).

**Proof:** within one plan execution the kernel snapshot is fixed; Scan is
side-effect-free and deterministic, so two consecutive identical Scans return
the same row set. With no intervening op consuming or modifying it, the second
Scan's output is never observed — removing it cannot change any downstream
operator's input. With an intervening op, the second Scan's row set becomes
the source for downstream ops; deduping across it is unsound (ppl005: removing
Scan2 would make the dead Filter between them live).

**Pinned by:** `ppl001`–`ppl005` (planner.rs tests); alg002 scan-identity
proptest (random `(type, subject, roles, tenant)` tuples — dedup allowed iff
full-tuple match, and the output scan sequence is the input with consecutive
duplicates collapsed).

## Extending this file

A new rewrite means: a section here (operation / preconditions / proof /
pinned-by), a unit pin in planner.rs, and an oracle corpus row in
`crates/runtime/tests/plan_oracle.rs` (optimize vs unoptimized over the seeded
differential corpus, divergence 0). alg001 fails the build if a rewrite ships
without its section.
