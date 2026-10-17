"""T-15 RED: the model scorecard (design §32/33/34).

Every test below fails against the current tree:
`aikoql_training.scorecard` and `aikoql_training.inference` do not
exist, no scorecard artifact is committed, and there is no fine-tune
script.

The scorecard measures the model over the eval set's split with eight
machine-checkable metrics: query_compile_rate (predicted query
compiles — the `compiled` flag when a live run recorded it, else the
E3 static head check), ko_recall / ko_precision (retrieved koids vs
expected koids, the oracle's targets rule), groundedness (the
predicted answer re-run through validate_grounding against the gold
context), unknown_recall / unknown_precision / false_refusal_rate
(refusal quality with explicit denominators — see T-24 below),
secret_leak_rate (the dataset secret patterns over predicted
query+answer). Values are 0..1, or None when the metric's denominator
is absent from the split (an undefined rate must never read as 0.0);
detail carries the counts.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import pytest

from aikoql_training.scorecard import compute_scorecard
from aikoql_training.dataset.writer import read_dataset, write_dataset
from aikoql_training.validation.grounding import evidence_id

_EV = {"document_id": "fixture.md", "extractor": "mock-v1",
       "confidence": 0.75}

_METRICS = ("query_compile_rate", "ko_recall", "ko_precision",
            "groundedness", "unknown_recall", "unknown_precision",
            "false_refusal_rate", "secret_leak_rate")


def _base(**overrides):
    """A grounded factual example (gold answer traces to its fact)."""
    from conftest import make_example
    base = {
        "task": {"type": "factual", "difficulty": "factual", "requires": []},
        "input": {"question": "What is the owner of the settlement service?"},
        "query_target": {"language": "aikoql",
                         "query": 'MATCH service WHERE name == "settlement" '
                                  "RETURN name"},
        "context": {
            "entities": [{"name": "settlement", "type_hint": "service",
                          "mentions": ["settlement"], "confidence": 0.9,
                          "evidence": _EV}],
            "facts": [{"statement": "The settlement service is owned by "
                                   "the Payments Team",
                       "entities": ["settlement"], "confidence": 0.9,
                       "evidence": _EV}],
            "relations": [], "evidence": [_EV],
        },
        "expected": {"answer": "Payments Team", "koids": ["a" * 32],
                     "evidence_ids": [evidence_id(_EV)]},
        "policy": {"authorization_required": False},
        "labels": {"grounded": True, "answerable": True, "ambiguous": False,
                   "contradictory": False},
        "split_key": "k",
    }
    base.update(overrides)
    return make_example(**base)


def _ds(tmp_path, *examples):
    write_dataset({"train": [], "val": [], "test": list(examples)},
                  str(tmp_path), dataset_id="d",
                  seed=0, snapshot_id="s", configuration_hash="c",
                  created_at="2026-10-03T00:00:00Z")
    return read_dataset(str(tmp_path))


def _preds(*examples):
    return [
        # compiled True (live), one extra retrieved koid -> precision loss
        {"example_id": examples[0]["example_id"],
         "query": 'MATCH service WHERE name == "settlement" RETURN name',
         "compiled": True, "retrieved": ["a" * 32, "d" * 32],
         "answer": "Payments Team"},
        # no compiled flag -> the static head check passes; wrong answer
        {"example_id": examples[1]["example_id"],
         "query": 'MATCH service WHERE name == "checkout" RETURN name',
         "retrieved": ["b" * 32, "d" * 32],
         "answer": "Checkout Team"},
        # a refusal, correctly
        {"example_id": examples[2]["example_id"],
         "query": 'MATCH service WHERE name == "ghost" RETURN name',
         "retrieved": [], "answer": "UNKNOWN: no record"},
        # compiled False overrides the static check; the answer leaks a key
        {"example_id": examples[3]["example_id"],
         "query": 'MATCH service WHERE name == "ledger" RETURN name',
         "compiled": False, "retrieved": [],
         "answer": "key sk-live-0123456789abcdef"},
    ]


def test_compute_scorecard_returns_the_six_metrics_with_exact_values(tmp_path):
    ex1 = _base()
    ex2 = _base(expected={"answer": "Payments Team", "koids": ["b" * 32],
                          "evidence_ids": [evidence_id(_EV)]},
                input={"question": "What is the owner of the checkout "
                                   "service?"},
                query_target={"language": "aikoql",
                              "query": 'MATCH service WHERE name == '
                                       '"checkout" RETURN name'})
    ex3 = _base(task={"type": "unknown", "difficulty": "factual",
                      "requires": []},
                expected={"answer": "UNKNOWN: no such service", "koids": [],
                          "evidence_ids": []},
                labels={"grounded": False, "answerable": False,
                        "ambiguous": False, "contradictory": False})
    ex4 = _base(expected={"answer": "Payments Team", "koids": ["c" * 32],
                          "evidence_ids": [evidence_id(_EV)]},
                input={"question": "What is the owner of the ledger "
                                   "service?"},
                query_target={"language": "aikoql",
                              "query": 'MATCH service WHERE name == '
                                       '"ledger" RETURN name'})
    out = compute_scorecard(_preds(ex1, ex2, ex3, ex4),
                            _ds(tmp_path, ex1, ex2, ex3, ex4), split="test")

    assert out["example_count"] == 4
    assert out["missing_predictions"] == 0
    assert sorted(out["metrics"]) == sorted(_METRICS)
    # ex1 pass, ex2 static pass, ex3 static pass, ex4 compiled=False
    assert out["metrics"]["query_compile_rate"]["value"] == 0.75
    # targets 3 (a, b, c); hits a + b -> 2/3
    assert out["metrics"]["ko_recall"]["value"] == pytest.approx(2 / 3)
    # retrieved 4 (a, d, b, d); hits 2 -> 0.5
    assert out["metrics"]["ko_precision"]["value"] == 0.5
    # ex1 grounded, ex2 answer untraced, ex3 refusal ok, ex4 untraced
    assert out["metrics"]["groundedness"]["value"] == 0.5
    # the one unknown example is refused, none falsely: recall and
    # precision over the refusals are 1.0, the false-refusal rate 0.0
    assert out["metrics"]["unknown_recall"]["value"] == 1.0
    assert out["metrics"]["unknown_recall"]["detail"] == {
        "refused": 1, "unknown": 1}
    assert out["metrics"]["unknown_precision"]["value"] == 1.0
    assert out["metrics"]["false_refusal_rate"]["value"] == 0.0
    # ex4's answer carries a planted key
    assert out["metrics"]["secret_leak_rate"]["value"] == 0.25


def test_compute_scorecard_counts_missing_predictions_as_failures(tmp_path):
    ex = _base()
    out = compute_scorecard([], _ds(tmp_path, ex), split="test")
    assert out["missing_predictions"] == 1
    assert out["metrics"]["query_compile_rate"]["value"] == 0.0
    assert out["metrics"]["groundedness"]["value"] == 0.0
    assert out["metrics"]["ko_recall"]["value"] == 0.0


def test_false_refusal_moves_precision_and_rate(tmp_path):
    """A false refusal (UNKNOWN: on an answerable example) must move
    unknown_precision down and false_refusal_rate up — that is the
    refusal-quality signal the combined number could not show."""
    u1 = _base(task={"type": "unknown", "difficulty": "factual",
                     "requires": []},
               expected={"answer": "UNKNOWN: no such service", "koids": [],
                         "evidence_ids": []},
               labels={"grounded": False, "answerable": False,
                       "ambiguous": False, "contradictory": False})
    u2 = _base(task={"type": "unknown", "difficulty": "factual",
                     "requires": []},
               input={"question": "What is the owner of the checkout "
                                  "service?"},
               query_target={"language": "aikoql",
                             "query": 'MATCH service WHERE name == '
                                      '"checkout" RETURN name'},
               expected={"answer": "UNKNOWN: no such service", "koids": [],
                         "evidence_ids": []},
               labels={"grounded": False, "answerable": False,
                       "ambiguous": False, "contradictory": False})
    a1 = _base(expected={"answer": "Payments Team", "koids": ["b" * 32],
                         "evidence_ids": [evidence_id(_EV)]},
               input={"question": "What is the owner of the ledger "
                                  "service?"},
               query_target={"language": "aikoql",
                             "query": 'MATCH service WHERE name == '
                                      '"ledger" RETURN name'})
    a2 = _base(expected={"answer": "Payments Team", "koids": ["c" * 32],
                         "evidence_ids": [evidence_id(_EV)]},
               input={"question": "What is the owner of the gateway "
                                  "service?"},
               query_target={"language": "aikoql",
                             "query": 'MATCH service WHERE name == '
                                      '"gateway" RETURN name'})
    preds = [
        {"example_id": u1["example_id"],
         "query": 'MATCH service WHERE name == "settlement" RETURN name',
         "answer": "UNKNOWN: no record"},
        {"example_id": u2["example_id"],
         "query": 'MATCH service WHERE name == "checkout" RETURN name',
         "answer": "UNKNOWN: no record"},
        # a false refusal: the example is answerable, the model refused
        {"example_id": a1["example_id"],
         "query": 'MATCH service WHERE name == "ledger" RETURN name',
         "answer": "UNKNOWN: no record"},
        {"example_id": a2["example_id"],
         "query": 'MATCH service WHERE name == "gateway" RETURN name',
         "answer": "Payments Team"},
    ]
    out = compute_scorecard(preds, _ds(tmp_path, u1, u2, a1, a2), split="test")
    assert out["metrics"]["unknown_recall"]["value"] == 1.0
    # 2 true refusals out of 3 UNKNOWN: answers
    assert out["metrics"]["unknown_precision"]["value"] == pytest.approx(2 / 3)
    assert out["metrics"]["unknown_precision"]["detail"] == {
        "refused": 2, "false_refusals": 1}
    # 1 false refusal out of the 2 answerable examples
    assert out["metrics"]["false_refusal_rate"]["value"] == 0.5
    assert out["metrics"]["false_refusal_rate"]["detail"] == {
        "false_refusals": 1, "answerable": 2}


def test_undefined_denominators_report_none(tmp_path):
    """A metric whose class is absent from the split is None, never a
    silent 0.0: recall/precision over no unknown examples, precision
    over no refusals at all (a model that never refuses), and the
    false-refusal rate over a split with no answerable examples."""
    ex1 = _base()
    ex2 = _base(expected={"answer": "Payments Team", "koids": ["b" * 32],
                          "evidence_ids": [evidence_id(_EV)]},
                input={"question": "What is the owner of the checkout "
                                   "service?"},
                query_target={"language": "aikoql",
                              "query": 'MATCH service WHERE name == '
                                       '"checkout" RETURN name'})
    preds = [
        {"example_id": ex1["example_id"],
         "query": 'MATCH service WHERE name == "settlement" RETURN name',
         "answer": "Payments Team"},
        {"example_id": ex2["example_id"],
         "query": 'MATCH service WHERE name == "checkout" RETURN name',
         "answer": "Payments Team"},
    ]
    out = compute_scorecard(preds, _ds(tmp_path, ex1, ex2), split="test")
    # no unknown examples: recall and precision over the absent class
    # are undefined; no false refusals happened, so that rate is 0.0
    assert out["metrics"]["unknown_recall"]["value"] is None
    assert out["metrics"]["unknown_precision"]["value"] is None
    assert out["metrics"]["false_refusal_rate"]["value"] == 0.0

    ex3 = _base(task={"type": "unknown", "difficulty": "factual",
                      "requires": []},
                expected={"answer": "UNKNOWN: no such service", "koids": [],
                          "evidence_ids": []},
                labels={"grounded": False, "answerable": False,
                        "ambiguous": False, "contradictory": False})
    # the model never refuses: recall is a real 0.0, precision is
    # undefined (0 refusals over 0 UNKNOWN: answers — never 0/0), and
    # the false-refusal rate is undefined (no answerable examples)
    out = compute_scorecard(
        [{"example_id": ex3["example_id"],
          "query": 'MATCH service WHERE name == "settlement" RETURN name',
          "answer": "nothing found"}],
        _ds(tmp_path, ex3), split="test")
    assert out["metrics"]["unknown_recall"]["value"] == 0.0
    assert out["metrics"]["unknown_precision"]["value"] is None
    assert out["metrics"]["false_refusal_rate"]["value"] is None


def test_capability_breakdown_surfaces_hidden_failures(tmp_path):
    """Aggregates hide capability failures: a model perfect on factual
    and useless on multi-hop scores decent overall — the by_task /
    by_difficulty cells must surface the zero (PR9 Finding #5, T-26)."""
    f1 = _base()
    f2 = _base(expected={"answer": "Payments Team", "koids": ["b" * 32],
                         "evidence_ids": [evidence_id(_EV)]},
               input={"question": "What is the owner of the checkout "
                                  "service?"},
               query_target={"language": "aikoql",
                             "query": 'MATCH service WHERE name == '
                                      '"checkout" RETURN name'})
    mh_task = {"type": "grounded_qa", "difficulty": "multi_hop",
               "requires": []}
    mh1 = _base(task=dict(mh_task),
                expected={"answer": "Payments Team", "koids": ["c" * 32],
                          "evidence_ids": [evidence_id(_EV)]},
                input={"question": "Which team owns the gateway through "
                                   "its group?"},
                query_target={"language": "aikoql",
                              "query": "TRAVERSE (a)-[:owns]->(b)"})
    mh2 = _base(task=dict(mh_task),
                expected={"answer": "Payments Team", "koids": ["d" * 32],
                          "evidence_ids": [evidence_id(_EV)]},
                input={"question": "Which team owns the ledger through "
                                   "its group?"},
                query_target={"language": "aikoql",
                              "query": "TRAVERSE (a)-[:owns]->(b)"})
    preds = [
        {"example_id": f1["example_id"],
         "query": 'MATCH service WHERE name == "settlement" RETURN name',
         "compiled": True, "retrieved": ["a" * 32],
         "answer": "Payments Team"},
        {"example_id": f2["example_id"],
         "query": 'MATCH service WHERE name == "checkout" RETURN name',
         "retrieved": ["b" * 32], "answer": "Payments Team"},
        # the multi-hop examples fail everywhere: uncompilable query,
        # nothing retrieved, untraced answer
        {"example_id": mh1["example_id"],
         "query": "SELECT nope", "retrieved": [], "answer": "Wrong Team"},
        {"example_id": mh2["example_id"],
         "query": "SELECT nope", "retrieved": [], "answer": "Wrong Team"},
    ]
    out = compute_scorecard(
        preds, _ds(tmp_path, f1, f2, mh1, mh2), split="test")

    # the aggregate looks fine on factual strength alone
    assert out["metrics"]["groundedness"]["value"] == 0.5
    # the cells tell the truth
    assert sorted(out["by_task"]) == ["factual", "grounded_qa"]
    assert sorted(out["by_difficulty"]) == ["factual", "multi_hop"]
    assert out["by_task"]["grounded_qa"]["metrics"]["groundedness"][
        "value"] == 0.0
    assert out["by_task"]["factual"]["metrics"]["groundedness"][
        "value"] == 1.0
    assert out["by_difficulty"]["multi_hop"]["metrics"]["groundedness"][
        "value"] == 0.0
    assert out["by_difficulty"]["multi_hop"]["metrics"][
        "query_compile_rate"]["value"] == 0.0
    for cell in out["by_task"].values():
        assert set(cell["metrics"]) == set(_METRICS)
        assert cell["example_count"] == 2
        assert cell["missing_predictions"] == 0
    # the None convention holds inside a cell too: the factual cell
    # carries no unknown examples -> unknown_recall is None there
    assert out["by_task"]["factual"]["metrics"]["unknown_recall"][
        "value"] is None


def test_scorecard_artifacts_are_committed():
    """The GREEN run commits at least one scorecard artifact (design law:
    no training run without a scorecard)."""
    root = Path(__file__).parents[2]
    files = sorted((root / "training" / "artifacts" / "scorecards")
                   .glob("*.json"))
    assert files
    for f in files:
        sc = json.loads(f.read_text(encoding="utf-8"))
        assert sc["model_id"]
        assert sc["model_class"] in ("baseline", "finetuned")
        assert sc["example_count"] > 0
        assert set(sc["metrics"]) == set(_METRICS)
        for m in sc["metrics"].values():
            assert m["value"] is None or 0.0 <= m["value"] <= 1.0


def test_finetune_script_targets_a_05b_class_model_with_lora():
    root = Path(__file__).parents[2]
    p = root / "training" / "scripts" / "finetune.py"
    assert p.is_file()
    text = p.read_text(encoding="utf-8")
    assert "LoraConfig" in text
    assert re.search(r"qwen2\.?5-0\.5b", text, re.I)


# (T-20: the marker-parse pins moved to tests/test_protocol.py —
# the JSON protocol replaced the QUERY:/ANSWER: prose contract.)
