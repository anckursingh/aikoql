# Wave 3.1 market corpus — version v1.0 (frozen)

Spec: MVP-QA-003A §5 (MKT-001), §7 (holdout). This file is the version
record of the evaluation corpus. Any edit to the corpus modules must
bump the version here and name what was invalidated; the Wave 3 pinned
tasks (`trackb::QUESTIONS`, `trackb::MARKET_QUESTIONS`) are frozen at
their Wave 3 numbers and may never be edited.

## Layout

| Part | Module | Docs | Tasks | Extractors |
|---|---|---|---|---|
| Wave 3.1 dev (new) | `tests/common/trackb31_docs.rs` | 36 | 135 (`MARKET_QUESTIONS_31`) | `w31-market-synthetic` |
| Wave 3 dev (pinned) | `tests/common/trackb.rs` `docs()` | 15 | (`QUESTIONS`) | Wave 3 |
| Wave 3 market (pinned) | `tests/common/trackb.rs` `market_docs()` | 4 | (`MARKET_QUESTIONS`) | Wave 3 |
| **Dev corpus (union)** | — | **55** | **148** | — |
| Holdout (frozen) | `tests/common/trackb_holdout.rs` | 6 | 24 (`HOLDOUT_QUESTIONS`) | `w31-holdout-synthetic` |

Pinned Wave 3 doc ids: kb-api, kb-audit, kb-depth, kb-fin, kb-growth-a,
kb-growth-b, kb-ledger, kb-mkt, kb-net, kb-ops, kb-payments,
kb-payments-v2, kb-sec, kb-warranty-a, kb-warranty-b, kb-arch, kb-deps,
kb-incident, kb-policy. Some Wave 3.1 tasks deliberately reuse Wave 3
documents (kb-payments, kb-audit, kb-arch, kb-incident) — the union is
the evaluation corpus, not just the new files.

Holdout doc ids: ho-route, ho-route-update, ho-route-notes, ho-driver,
ho-compliance, ho-widgets (Northwind Logistics — a deliberately different
domain from the dev corpus's payments/SaaS platform).

## Class distribution (union, 148 tasks)

| Class | Tasks | Class | Tasks |
|---|---|---|---|
| W1 lookup | 14 | W7 provenance | 10 |
| W2 semantic-probe | 11 | W8 personal | 10 |
| W3 synthesis | 12 | W9 policy | 12 |
| W4 hop/cross-doc | 13 | W10 planning | 10 |
| W5 temporal | 16 | W11 unknown-probe | 15 |
| W6 contradiction | 15 | W12 longitudinal | 10 |

All 12 classes ≥ 10 tasks. Holdout: 24 tasks, 2 per class.

## Shape thresholds (measured, pinned by `w31_mkt_001_market_corpus_expansion`)

- Multi-source (units span ≥2 docs): 46/148 = **31.1%** (need ≥20%)
- Relationship-dependent (`gt.relationships != "none"`): 92/148 = **62.2%** (need ≥20%)
- Temporal (class W5): 16/148 = **10.8%** (need ≥10%)
- Contradictory (class W6): 15/148 = **10.1%** (need ≥10%)
- Unknown (class W11): 15/148 = **10.1%** (need ≥10%)

## Design rules carried by the corpus

- **Contradiction-as-fact**: conflict docs carry explicit
  "X conflicts with Y" facts plus `conflicts_with` relations — the
  contradiction is first-class knowledge, not only a scoring construct.
- **Temporal change pairs**: kb-sla-change, kb-retention-v1/v2,
  kb-risk-change, ho-route/ho-route-update give the temporal class
  its current-vs-historical structure.
- **Customer* naming**: CustomerAlex / CustomerPriya / CustomerDev keep
  customer KOs from merging with the engineer entities Alex/Priya/Dev —
  the resolver must split what naming keeps apart, and merge only true
  identity.
- **W2 zero-overlap** (see task-taxonomy): every W2 probe shares zero
  tokens with both units under the no-stopwords `tokens()` contract,
  asserted in the test.
- **W11 traps**: unknown-probe units are real corpus sentences that do
  NOT answer the question (correct response delivers neither). kb-dr-plan
  is a deliberate trap doc ("The DrPlan document is still under review.").
- **W12 longitudinal pairs**: capacity/retention evolution pairs
  (kb-retention-v1/v2) give day-over-day checkpoints.

## Freeze rules (spec §7)

- The holdout is never scored during development: `wave31_market.rs`
  asserts structure only (integrity, disjoint ids, ≥20 tasks, gt shape).
  No scoring threshold may ever live in that test.
- The one evaluation pass runs in the Wave 3.1 comparison harness
  (#161) with frozen machinery; its printed results are pinned into the
  evidence docs. After that pin, no corpus edit without a version bump
  and an invalidation note here.
- Wave 3 measurements (`wave3_market_reality.rs`) are untouched by this
  corpus: the Wave 3 numbers stay frozen.

## Scenario corpora (DEC-001 / TEMP-001) — v1.0

Two small scenario corpora back the decision and temporal experiments
(`trackb31_docs::decision_docs` / `timeline_docs`), NOT part of the
frozen 148-task union corpus:

- kb-deploy-{v1,v2,policy,runbook}: deployment-window policy lineage,
  supersession recorded in the kernel (wave31_decision.rs), plus a live
  conflicting runbook claim. History facts are past-tense statements in
  the current doc so stale statements stay exact-substring
  distinguishable from history.
- kb-retry-{v1,v2,v3}: retry-limit timeline; the current doc states the
  full history and the reasons (change/why dimensions).

Rule added by these corpora: the historical question must name the
entity ("What was the retry limit in February?") — the exact-token gate
(≥2 content tokens or a ranked entity) is the compiler's lexical
reachability ceiling; recorded in losses.md, not silently designed
around.
