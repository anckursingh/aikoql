"""T-14 RED: the machine-checkable eval set (design §33, E1–E9).

Each E-case is a dataset-level check over the corpus artifact —
consistency properties the scorecard (T-15) will also measure on the
model. `eval_dataset` reports `{E1..E9: {ok, count, detail}}`; ok iff
zero violating examples.

E1 factual → exact fact: every factual example's expected answer is
  stated verbatim by a context fact.
E2 retrieval → correct KO set: every context fact's entities are all
  present in the context's entity mentions.
E3 query → compilable aikoql: every query_target.query starts with
  the grammar's MATCH/TRAVERSE head and is non-empty.
E4 multi-hop → correct graph path: every multi-hop example carries
  at least three koids (anchor + intermediate + target).
E5 temporal → correct version: every temporal example's query
  carries AS_OF and its question names a month.
E6 provenance → correct evidence: every provenance example's
  evidence_ids all point at context evidence rows.
E7 unknown → refusal: unknown examples are answerable=False and
  every other example is answerable=True.
E8 authorization → no sensitive context: authorization examples
  carry policy.authorization_required=True, and a DENIED example's
  context holds only its policy-decision fact.
E9 contradiction → conflict-aware: contradiction examples are
  labeled contradictory=True and their answer is in the
  CONTRADICTED machine-readable shape.

Every test below fails against the current tree:
`aikoql_training.validation.eval_set` does not exist.
"""

from __future__ import annotations

import json

import pytest

from aikoql_training.dataset.writer import read_dataset
from aikoql_training.validation.eval_set import eval_dataset

_EV = {"document_id": "fixture.md", "extractor": "mock-v1",
       "confidence": 0.75}


def _write(tmp_path, example):
    from aikoql_training.dataset.writer import write_dataset
    write_dataset({"train": [example], "val": [], "test": []},
                  str(tmp_path), dataset_id="d",
                  seed=0, snapshot_id="s", configuration_hash="c",
                  created_at="2026-10-03T00:00:00Z")
    return read_dataset(str(tmp_path))


def _example(**overrides):
    from conftest import make_example
    return make_example(
        task={"type": "factual", "difficulty": "factual", "requires": []},
        input={"question": "What is the owner of the settlement service?"},
        query_target={"language": "aikoql",
                      "query": 'MATCH service WHERE name == "settlement" '
                               "RETURN name"},
        context={
            "entities": [{"name": "settlement", "type_hint": "service",
                          "mentions": ["settlement"], "confidence": 0.9,
                          "evidence": _EV}],
            "facts": [{"statement": "The settlement service is owned by "
                                   "the Payments Team",
                       "entities": ["settlement"], "confidence": 0.9,
                       "evidence": _EV}],
            "relations": [], "evidence": [_EV],
        },
        expected={"answer": "Payments Team", "koids": ["a" * 32],
                  "evidence_ids": []},
        policy={"authorization_required": False},
        labels={"grounded": True, "answerable": True, "ambiguous": False,
                "contradictory": False},
        split_key="k",
        **overrides,
    )


def _eval(tmp_path, example):
    ds = _write(tmp_path, example)
    return eval_dataset(ds)


# -- the nine cases, each with its poisoned fixture --------------------------

def test_E1_factual_answer_must_be_a_verbatim_context_fact(tmp_path):
    assert _eval(tmp_path, _example())["E1"]["ok"] is True
    poisoned = _example(expected={"answer": "Checkout Team",
                                  "koids": ["a" * 32],
                                  "evidence_ids": []})
    out = _eval(tmp_path, poisoned)
    assert out["E1"]["ok"] is False
    assert out["E1"]["count"] == 1


def test_E2_context_facts_must_cite_retrieved_entities(tmp_path):
    assert _eval(tmp_path, _example())["E2"]["ok"] is True
    bad = _example(context={
        "entities": [{"name": "checkout", "type_hint": "service",
                      "mentions": ["checkout"], "confidence": 0.9,
                      "evidence": _EV}],
        "facts": [{"statement": "The settlement service is owned by "
                               "the Payments Team",
                   "entities": ["settlement"], "confidence": 0.9,
                   "evidence": _EV}],
        "relations": [], "evidence": [_EV]})
    out = _eval(tmp_path, bad)
    assert out["E2"]["ok"] is False


def test_E3_queries_must_carry_the_grammar_head(tmp_path):
    assert _eval(tmp_path, _example())["E3"]["ok"] is True
    out = _eval(tmp_path, _example(query_target={"language": "aikoql",
                                                 "query": ""}))
    assert out["E3"]["ok"] is False


def test_E4_multi_hop_examples_carry_the_full_path(tmp_path):
    good = _example(task={"type": "multi_hop", "difficulty": "multi_hop",
                          "requires": []},
                    expected={"answer": "checkout", "koids": ["a" * 32,
                                                              "b" * 32,
                                                              "c" * 32],
                              "evidence_ids": []})
    assert _eval(tmp_path, good)["E4"]["ok"] is True
    bad = _example(task={"type": "multi_hop", "difficulty": "multi_hop",
                         "requires": []},
                   expected={"answer": "checkout", "koids": ["a" * 32,
                                                             "b" * 32],
                             "evidence_ids": []})
    out = _eval(tmp_path, bad)
    assert out["E4"]["ok"] is False


def test_E5_temporal_queries_carry_as_of_and_a_month(tmp_path):
    good = _example(task={"type": "temporal", "difficulty": "factual",
                          "requires": []},
                    input={"question": "What was the owner of the "
                                       "settlement service in March?"},
                    query_target={"language": "aikoql",
                                  "query": "MATCH service AS_OF 1700000000000 "
                                           "RETURN name"})
    assert _eval(tmp_path, good)["E5"]["ok"] is True
    bad = _example(task={"type": "temporal", "difficulty": "factual",
                         "requires": []},
                   input={"question": "What was the owner of the "
                                      "settlement service?"},
                   query_target={"language": "aikoql",
                                 "query": 'MATCH service WHERE name == '
                                          '"settlement" RETURN name'})
    out = _eval(tmp_path, bad)
    assert out["E5"]["ok"] is False


def test_E6_provenance_evidence_ids_point_at_context_rows(tmp_path):
    from aikoql_training.validation.grounding import evidence_id
    good = _example(task={"type": "provenance", "difficulty": "factual",
                          "requires": []},
                    expected={"answer": "Payments Team", "koids": ["a" * 32],
                              "evidence_ids": [evidence_id(_EV)]})
    assert _eval(tmp_path, good)["E6"]["ok"] is True
    bad = _example(task={"type": "provenance", "difficulty": "factual",
                         "requires": []},
                   expected={"answer": "Payments Team", "koids": ["a" * 32],
                             "evidence_ids": ["f" * 64]})
    out = _eval(tmp_path, bad)
    assert out["E6"]["ok"] is False


def test_E7_unknown_examples_are_refusals_and_only_them(tmp_path):
    good = _example(task={"type": "unknown", "difficulty": "factual",
                          "requires": []},
                    expected={"answer": "UNKNOWN: no such service",
                              "koids": [], "evidence_ids": []},
                    labels={"grounded": False, "answerable": False,
                            "ambiguous": False, "contradictory": False})
    assert _eval(tmp_path, good)["E7"]["ok"] is True
    bad = _example(task={"type": "unknown", "difficulty": "factual",
                         "requires": []},
                   labels={"grounded": False, "answerable": True,
                           "ambiguous": False, "contradictory": False})
    out = _eval(tmp_path, bad)
    assert out["E7"]["ok"] is False


def test_E8_denied_authorization_context_holds_only_the_decision(tmp_path):
    decision = "Policy decision: DENIED: read on service"
    good = _example(
        task={"type": "authorization", "difficulty": "factual",
              "requires": []},
        input={"question": "Can auditor read the settlement service?"},
        context={"entities": [], "relations": [], "evidence": [_EV],
                 "facts": [{"statement": decision, "entities": [],
                            "confidence": 0.9, "evidence": _EV}]},
        expected={"answer": "DENIED: read on service",
                  "koids": ["a" * 32], "evidence_ids": []},
        policy={"authorization_required": True},
        labels={"grounded": True, "answerable": True, "ambiguous": False,
                "contradictory": False})
    assert _eval(tmp_path, good)["E8"]["ok"] is True
    # the denied object's content leaks into the context — fail closed
    leak = _example(
        task={"type": "authorization", "difficulty": "factual",
              "requires": []},
        input={"question": "Can auditor read the settlement service?"},
        context={"entities": [], "relations": [], "evidence": [_EV],
                 "facts": [
                     {"statement": decision, "entities": [],
                      "confidence": 0.9, "evidence": _EV},
                     {"statement": "The settlement service is owned by "
                                  "the Payments Team",
                      "entities": ["settlement"], "confidence": 0.9,
                      "evidence": _EV}]},
        expected={"answer": "DENIED: read on service",
                  "koids": ["a" * 32], "evidence_ids": []},
        policy={"authorization_required": True},
        labels={"grounded": True, "answerable": True, "ambiguous": False,
                "contradictory": False})
    out = _eval(tmp_path, leak)
    assert out["E8"]["ok"] is False


def test_E9_contradiction_answers_preserve_conflict_metadata(tmp_path):
    answer = ("CONTRADICTED: Payments Team (claim aaaaaaaa) vs "
              "Checkout Team (claim bbbbbbbb); conflict cccccccc, "
              "resolution unresolved")
    good = _example(
        task={"type": "contradiction", "difficulty": "factual",
              "requires": []},
        input={"question": "Who owns the settlement service?"},
        expected={"answer": answer, "koids": ["a" * 32],
                  "evidence_ids": []},
        labels={"grounded": True, "answerable": True, "ambiguous": False,
                "contradictory": True})
    assert _eval(tmp_path, good)["E9"]["ok"] is True
    bad = _example(
        task={"type": "contradiction", "difficulty": "factual",
              "requires": []},
        input={"question": "Who owns the settlement service?"},
        expected={"answer": "Payments Team", "koids": ["a" * 32],
                  "evidence_ids": []},
        labels={"grounded": True, "answerable": True, "ambiguous": False,
                "contradictory": False})
    out = _eval(tmp_path, bad)
    assert out["E9"]["ok"] is False


# -- the whole battery over one artifact ------------------------------------

def test_eval_reports_all_nine_cases(tmp_path):
    out = _eval(tmp_path, _example())
    assert sorted(out) == [f"E{i}" for i in range(1, 10)]
    assert all(isinstance(v["ok"], bool) for v in out.values())


def test_eval_cli_exits_nonzero_on_a_poisoned_dataset(tmp_path):
    import os
    import subprocess
    import sys
    from pathlib import Path
    _write(tmp_path, _example(expected={"answer": "Checkout Team",
                                        "koids": ["a" * 32],
                                        "evidence_ids": []}))
    root = Path(__file__).parents[2]
    env = dict(os.environ)
    env["PYTHONPATH"] = str(root / "training" / "src") + os.pathsep + \
        env.get("PYTHONPATH", "")
    proc = subprocess.run(
        [sys.executable, "-m", "aikoql_training.cli", "eval", str(tmp_path)],
        env=env, capture_output=True, text=True, timeout=60)
    assert proc.returncode == 1
    report = json.loads(proc.stdout)
    assert report["publishable"] is False
