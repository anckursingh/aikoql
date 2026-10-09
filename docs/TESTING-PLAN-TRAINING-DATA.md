# AikoQL Training Data Engine — test plan

Sibling of `docs/IMPLEMENTATION-PLAN-TRAINING-DATA.md`; milestone ids T-01..
T-16 match its §5 table. The design's §25 phases and §26 gates bind this
plan; the standing rules below are the launch plan's, restated for a Python
component.

## 1. Standing rules (non-negotiable)

1. **Real RED first.** A RED is a failing assert or a measured counter that
   moves — never a vacuous placeholder. REDs are archived
   (`docs/red-archive/t-xx-*.red.log`).
2. **Do not weaken assertions to get GREEN.**
3. **Determinism is a property, tested every milestone:** same snapshot +
   seed + configuration + generator version ⇒ same example IDs, same
   manifest, byte-identical canonicalized JSONL (design G7/§24).
4. **AIKOQL is the oracle:** a generated query is valid only through
   compile → plan → execute → scenario-match → auth → evidence (design
   §14). Tests that "look at" a query instead of running it are RED.
5. **Security tests are fail-closed:** one unauthorized KO, one secret, one
   redacted-then-restored context ⇒ the dataset gate fails and the run
   records the violation (S1–S8, §26).
6. **No performance claim without a structural metric or a reproducible
   benchmark** — examples/sec and rates are measured against committed
   quick cells locally; the 10K + seed-sweep runs ride CI once T-12 lands.
7. **No unseeded RNG in tests** (repo test-env-hygiene rule, Python
   edition); hypothesis uses a fixed profile/derandomize in CI legs.
8. One milestone = one commit set (test RED → feat → docs); the re-stamp
   ritual (R3-005/F8) applies to every stream.

## 2. Test layers

| layer | tooling | covers |
|---|---|---|
| unit | pytest, mirrors `src/` one-to-one | every module, typed errors, fail-closed validation |
| property/fuzz | hypothesis (the SDK's `test_fuzz_estate.py` + corpus-pin pattern) | the seven slices below; corpus pinned so removing coverage is caught |
| integration | real AIKOQL via the SDK (embedded hatch first, MCP for the full path) | snapshot, query oracle, context adapter, authorization |
| dataset gates | `aikoql-training validate` | the §5 fail-closed list; a poisoned fixture must fail each gate |
| eval set | T-14 machine-checkable cases | design E1–E9 |
| mutation | validator mutants must be killed (the F-03 harness idea, Python edition) | gate teeth, T-14 |
| model scorecard | T-15 eval harness | compile rate, KO recall/precision, groundedness, refusal rate, secret-leak rate |

## 3. Fuzz slices

| id | target | property | corpus pin |
|---|---|---|---|
| FZ-T1 | canonical-schema validator | arbitrary JSON (hypothesis `jsons()`) never panics, never accepts unknown fields; fail-closed on missing required | `test_fuzz_estate_pin.py` pattern |
| FZ-T2 | manifest + checksum reader | tampered/truncated manifest or checksums refuse the dataset | pinned |
| FZ-T3 | config parser | adversarial YAML (negative ratios, unknown keys, wrong seed types) → typed errors; no silent defaults for security options | pinned |
| FZ-T4 | template engine | entity/property names containing aikoql keywords, quotes, unicode, control chars → escaped questions; nothing reaches the query builder raw | pinned |
| FZ-T5 | query builder round-trip | every builder output compiles and executes; a failure is a builder bug (property test over scenario fixtures) | pinned |
| FZ-T6 | JSONL writer/reader | adversarial example content (newlines, unicode, control chars, huge fields) round-trips byte-identically | pinned |
| FZ-T7 | splitter | holdout invariant: no train example shares a test-only entity/relation/template under any seeded shuffle | pinned |

## 4. Per-milestone mechanics

| milestone | RED source | GREEN evidence |
|---|---|---|
| T-01 | schema tests fail against the empty package | `pytest tests/test_schema.py` green; recon doc cites verified interface sites |
| T-02 | snapshot identity tests fail | two snapshots of the same immutable state produce the same identity; manifest reproducible |
| T-03 | factual/relation scenario tests fail | 100% of generated scenarios execute against the source DB; expected path stored |
| T-04 | exact-path fixture fails; fabricated edge passes | multi-hop scenarios carry the real path; no fabricated edges |
| T-05 | expected-query tests fail; stub compiles nothing | 6-point oracle acceptance on every generated query; compile rate 100% |
| T-06 | context-content tests fail | context from AIKOQL only (arch assertion: no Python retrieval); no unauthorized/stale rows |
| T-07 | unsupported-claim rejection fails | 100% of accepted examples satisfy grounding; claim→context/evidence trace present |
| T-08 | March/August + evidence-ID tests fail | March ⇒ v1, August ⇒ v2 (real intervals); every provenance example cites real evidence |
| T-09 | unknown/ambiguous/contradiction fixtures fail | labels machine-readable; uncertainty never a false positive; Conflict metadata preserved |
| T-10 | authorized/unauthorized subject fixtures fail | same question, different subjects ⇒ different (correct) results; unauthorized rows never in context |
| T-11 | near-duplicate questions split apart; writer properties fail | holdout-aware split; JSONL deterministic; manifest + sha256; atomic output; interrupted-run cleanup |
| T-12 | each gate missing its tooth lets the poisoned dataset through | every §5 gate enforced; CLI end-to-end on a fixture DB |
| T-13 | metrics/error-category tests fail | typed errors with stage/scenario/code/example_id; metrics carry no sensitive content |
| T-14 | determinism-across-seeds, leakage, security, E1–E9 fail | 10K POC corpus + eval set regenerate identically; all gates green; validator mutants killed |
| T-15 | scorecard absent | first fine-tune produces a committed scorecard; no training run without one |
| T-16 | question→query→context→answer fails eval cases | wrapper reuses the validated pipeline; refusal/unknown paths correct on the eval set |

## 5. Dataset gates (fail-closed, `aikoql-training validate`)

| gate | threshold |
|---|---|
| compiler success | 100% of generated query examples (design §26) |
| execution success | 100% |
| scenario match | 100% (query result equals the scenario's expected result) |
| grounding | 100% of accepted answer examples |
| evidence coverage | required evidence present, or the example is refused |
| authorization | violations = 0 |
| secret/PII leakage | 0 (reuse the ingestion secret-filter output; never bypass) |
| schema validity | 0 invalid |
| leakage (split) | 0 cross-holdout pairs |
| determinism | regenerate ⇒ identical manifest + checksums |
| duplicates | duplicate rate recorded; above the configured bound ⇒ fail |

## 6. Eval suite (design §33, machine-checkable at T-14)

E1 factual → exact fact; E2 retrieval → correct KO set; E3 query →
compilable aikoql query; E4 multi-hop → correct graph path; E5 temporal →
correct version; E6 provenance → correct evidence; E7 unknown → refusal;
E8 authorization → no sensitive context; E9 contradiction → conflict-aware
result. Each is a dataset-level check + a scorecard metric at T-15.

## 7. CI legs (T-12, when green)

| leg | content |
|---|---|
| unit | `pytest` on `training/**` (fast-exit elsewhere, CI-03 pattern) |
| fuzz-estate | hypothesis slices with corpus pins, derandomized |
| determinism | regenerate ⇒ identical (small fixture KB) |
| integration | embedded-server scenarios + one MCP full-path cell |
| gate teeth | dataset-validator mutation smoke |

Arch-gate workflow pins register in the same commit set as the workflow
(CI-04: a red gate never enters CI).

## 8. Laptop vs CI

Laptop runs quick cells only (≤2K examples, single seed): unit, fuzz,
determinism, one embedded integration cell. The 10K corpus, the seed sweep
and the full E1–E9 battery ride CI/nightly once T-12 lands. No absolute
timing pins on shared runners; throughput claims cite the committed quick
cells.
