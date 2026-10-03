"""T-15 RED: the model scorecard (design §32/33/34).

Every test below fails against the current tree:
`aikoql_training.scorecard` and `aikoql_training.inference` do not
exist, no scorecard artifact is committed, and there is no fine-tune
script.

The scorecard measures the model over the eval set's split with six
machine-checkable metrics: query_compile_rate (predicted query
compiles — the `compiled` flag when a live run recorded it, else the
E3 static head check), ko_recall / ko_precision (retrieved koids vs
expected koids, the oracle's targets rule), groundedness (the
predicted answer re-run through validate_grounding against the gold
context), refusal_rate (UNKNOWN: refusals on unknown examples),
secret_leak_rate (the dataset secret patterns over predicted
query+answer). Values are 0..1; detail carries the counts.
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
            "groundedness", "refusal_rate", "secret_leak_rate")


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
    # the one unknown example is refused
    assert out["metrics"]["refusal_rate"]["value"] == 1.0
    # ex4's answer carries a planted key
    assert out["metrics"]["secret_leak_rate"]["value"] == 0.25


def test_compute_scorecard_counts_missing_predictions_as_failures(tmp_path):
    ex = _base()
    out = compute_scorecard([], _ds(tmp_path, ex), split="test")
    assert out["missing_predictions"] == 1
    assert out["metrics"]["query_compile_rate"]["value"] == 0.0
    assert out["metrics"]["groundedness"]["value"] == 0.0
    assert out["metrics"]["ko_recall"]["value"] == 0.0


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
            assert 0.0 <= m["value"] <= 1.0


def test_finetune_script_targets_a_05b_class_model_with_lora():
    root = Path(__file__).parents[2]
    p = root / "training" / "scripts" / "finetune.py"
    assert p.is_file()
    text = p.read_text(encoding="utf-8")
    assert "LoraConfig" in text
    assert re.search(r"qwen2\.?5-0\.5b", text, re.I)


def test_inference_parse_extracts_query_and_answer_markers():
    from aikoql_training.inference import parse_model_reply
    query, answer = parse_model_reply(
        "QUERY: MATCH service RETURN name\nANSWER: Payments Team")
    assert query == "MATCH service RETURN name"
    assert answer == "Payments Team"


def test_inference_parse_falls_back_to_an_answer_only_reply():
    from aikoql_training.inference import parse_model_reply
    query, answer = parse_model_reply("UNKNOWN: no such record\n")
    assert query == ""
    assert answer == "UNKNOWN: no such record"
