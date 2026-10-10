# aikoql-training

Training Data Engine for AikoQL — deterministic, validated
training/evaluation dataset generation for fine-tuning knowledge-graph
retrieval models against an [AikoQL](https://pypi.org/project/aikoql/)
knowledge store.

The engine seeds synthetic corpora (AcmePay, NovaEnergy), derives
semantic plans (`plan_of`/`policy_of`), renders them into aikoql query
text, certifies grounded answers with evidence, splits
leakage-checked train/val/test sets, and scores fine-tuned models —
with refusal metrics, per-capability scorecards, and grounding
mutation fuzzing. Every step is deterministic and gate-validated
(fail-closed).

## Install

```sh
pip install aikoql-training
```

Requires Python 3.9+ and a running aikoql server (the `aikoql`
package, installed automatically).

## CLI

```sh
aikoql-training --help        # snapshot / generate / validate / stats / eval / export
```

The dataset pipeline and its contract are documented in the repo:
`docs/IMPLEMENTATION-PLAN-TRAINING-DATA.md` (plan) and
`docs/training-data-architecture.md` (architecture, sections 1-35).

## License

Apache-2.0.
