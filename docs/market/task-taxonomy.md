# Wave 3.1 task taxonomy (12 classes)

Spec: MVP-QA-003A §5. One evaluation harness, one judgment contract:
the 12 workload classes define what each task measures; the gt contract
defines how an answer is judged. Mechanical slice, deterministic —
every rule here is enforceable by a committed test.

## The 12 classes

| Class | Name | What it measures |
|---|---|---|
| W1 | lookup | Single-doc fact retrieval; lexical match should suffice |
| W2 | semantic-probe | Meaning match with **zero token overlap** against both units (below) |
| W3 | synthesis | Joining ≥2 units into one answer |
| W4 | hop/cross-doc | Multi-doc traversal (relation hops, cross-doc joins) |
| W5 | temporal | Current claim vs historical claim separation |
| W6 | contradiction | Conflicting sources surfaced with authority, not merged |
| W7 | provenance | Which source document supports the answer |
| W8 | personal | Personal-fact queries in agent memory |
| W9 | policy | Organization-policy facts |
| W10 | planning | Multi-step plan recall |
| W11 | unknown-probe | The corpus does NOT answer the question (trap) |
| W12 | longitudinal | Change over time, memory across sessions |

## Ground-truth contract (7 fields, every task)

`gt.variants` (accepted answers), `gt.evidence` (source), `gt.relationships`
(expected relation text or `"none"`), `gt.temporal`
(`current`/`historical`/`mixed`), `gt.authority`
(`source_code`/`documentation`/`deployment_observed`/`organization_policy`),
`gt.ambiguity` (`none`/`conflict`/`unknown`), plus the `units` list of
verbatim corpus sentences / document ids the answer must be assembled
from. Enforced by `w31_mkt_001` (every task, every field, vocabulary
checks, unit backing).

## Class-definition rules (asserted in the corpus test)

**W2 zero-overlap.** Every W2 probe shares zero tokens with BOTH units
under the no-stopwords `tokens()` contract (lowercase, split on
non-alphanumeric, stopwords kept — "the"/"a"/"on" count). A probe with
any token overlap would be answerable by lexical overlap and would
pollute the RAG-vs-AIKOQL comparison. Enforced by
`assert_w2_zero_overlap` in `wave31_market.rs` for every W2 task in the
union and the holdout.

**W11 traps.** Unknown-probe units are real corpus sentences that do NOT
answer the question; the correct response delivers neither unit. The
win-zone judge inverts for these tasks: `score = 2 − hits`. The corpus
test asserts the class membership mechanically (class == "W11"); the
trap-ness is carried by the corpus content itself (see corpus-version).

## Mechanical shape counters

Defined in `wave31_market.rs`, independent of the declared `kind`:

- **multi-source**: the task's units' backing docs (via
  `unit_backing_docs` over the union corpus, relation-triple units
  resolved through the merged IR) span ≥2 distinct document ids.
- **relationship-dependent**: `gt.relationships != "none"`.
- **temporal / contradictory / unknown**: class membership
  W5 / W6 / W11 respectively.

These are the §5 acceptance thresholds; the measured values are pinned
in corpus-version.md.

## Judgment contract (win-zone, reused from Wave 3 / G11)

A treatment scores a task when its delivered units hit the task's units
(hits / 2), with the W11 inversion above. Semantic probes (W2) cannot be
judged by overlap — the zero-overlap contract makes the win-zone judge
itself the measurement: a lexical treatment must score 0 on W2 by
construction. The three-way comparison harness (#161) reuses this
contract unchanged so RAG, Graph-RAG, and AIKOQL are judged identically.
