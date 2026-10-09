"""T-16 RED: the inference wrapper (design §40 end-state).

Every test below fails against the current tree:
`aikoql_training.chat` does not exist and there is no chat script —
the question -> query -> context -> answer path has no wrapper.

The wrapper is thin and reuses the validated pipeline: the model's
two skills through the T-20 JSON protocol seam (build_query_prompt /
build_answer_prompt / parse_query_reply / parse_answer_reply) around
a live AIKOQL call.
Both the model calls (`generate`) and the query runner (`run_query`)
are injectable seams, so the path is tested without a model; the
refusals use the T-09 machine-readable UNKNOWN: format, and an
UNKNOWN: answer from the model itself passes through as a refusal.
The context statements come from the query results only — a wrapper
that reads them from the dataset would bypass the AIKOQL step.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import pytest

from aikoql_training.chat import chat
from aikoql_training.scenarios.answer_formats import UNKNOWN_PREFIX

_QUERY = 'MATCH Cardholder WHERE name == "Alice" RETURN name'
_STATEMENT = "Alice Chen holds card 4111"
_RESULTS = [{"koid": "b" * 32, "statement": _STATEMENT},
            {"koid": "c" * 32}]

_Q_REPLY = json.dumps({"query": _QUERY, "refusal_reason": None})
_A_REPLY = json.dumps({"answer": "Alice Chen", "grounded": True,
                       "claims": [{"statement": _STATEMENT,
                                   "evidence_ids": ["e1"]}],
                       "refusal_reason": None})
_REFUSAL_REPLY = json.dumps(
    {"answer": "UNKNOWN: the context lacks the account",
     "grounded": False, "claims": [],
     "refusal_reason": "the context lacks the account"})


def _generate(query_reply: str, answer_reply: str):
    """A two-skill stub: first call is the query step, then the answer."""
    calls = []

    def generate(text: str) -> str:
        calls.append(text)
        if len(calls) == 1:
            return query_reply
        return answer_reply

    return generate, calls


def _run_query(results):
    calls = []

    def run_query(query):
        calls.append(query)
        return {"results": results}

    return run_query, calls


def test_chat_happy_path_round_trip():
    generate, prompts = _generate(_Q_REPLY, _A_REPLY)
    run_query, queries = _run_query(_RESULTS)
    rec = chat("Who holds card 4111?", generate=generate,
               run_query=run_query)

    assert rec["question"] == "Who holds card 4111?"
    assert rec["query"] == _QUERY
    assert rec["compiled"] is True
    assert rec["retrieved"] == ["b" * 32, "c" * 32]
    assert rec["statements"] == [_STATEMENT, "c" * 32]
    assert rec["answer"] == "Alice Chen"
    assert rec["grounded"] is True
    assert rec["claims"] == [{"statement": _STATEMENT,
                              "evidence_ids": ["e1"]}]
    assert rec["refusal_reason"] is None
    assert rec["refused"] is False
    # the query step prompt demands the JSON protocol over the question
    assert '"query"' in prompts[0] and "Who holds card 4111?" in prompts[0]
    # the answer step prompt carries the question AND the context that
    # came from the query results, not from anywhere else
    assert "Who holds card 4111?" in prompts[1]
    assert _STATEMENT in prompts[1] and "c" * 32 in prompts[1]
    assert queries == [_QUERY]


def test_chat_no_query_refuses():
    def boom(_q):
        raise AssertionError("run_query must not be called without a query")

    generate, prompts = _generate("I cannot help with that.", "x")
    rec = chat("?", generate=generate, run_query=boom)

    assert rec["refused"] is True
    assert rec["answer"].startswith(UNKNOWN_PREFIX)
    assert rec["query"] == ""
    assert rec["compiled"] is False
    assert len(prompts) == 1  # the answer step never runs


def test_chat_model_query_refusal_field():
    """A refusal the MODEL declares, as a field: the reason is carried
    machine-readably instead of a prose sniff."""
    def boom(_q):
        raise AssertionError("run_query must not be called on a refusal")

    reply = json.dumps({"query": None,
                        "refusal_reason": "no graph access"})
    generate, prompts = _generate(reply, "x")
    rec = chat("?", generate=generate, run_query=boom)

    assert rec["refused"] is True
    assert rec["refusal_reason"] == "no graph access"
    assert rec["answer"].startswith(UNKNOWN_PREFIX)
    assert rec["query"] == ""
    assert len(prompts) == 1


def test_chat_query_failure_refuses():
    def run_query(query):
        raise RuntimeError("compile failed")

    generate, prompts = _generate(_Q_REPLY, "x")
    rec = chat("Who holds card 4111?", generate=generate,
               run_query=run_query)

    assert rec["refused"] is True
    assert rec["answer"].startswith(UNKNOWN_PREFIX)
    assert rec["compiled"] is False
    assert rec["retrieved"] == []
    assert len(prompts) == 1


def test_chat_empty_results_refuse():
    generate, prompts = _generate(_Q_REPLY, "x")
    run_query, queries = _run_query([])
    rec = chat("Who holds card 4111?", generate=generate,
               run_query=run_query)

    assert rec["refused"] is True
    assert rec["answer"].startswith(UNKNOWN_PREFIX)
    assert rec["compiled"] is True  # it compiled; nothing matched
    assert rec["retrieved"] == []
    assert len(prompts) == 1


def test_chat_unknown_answer_passthrough():
    generate, _ = _generate(_Q_REPLY, _REFUSAL_REPLY)
    run_query, _ = _run_query(_RESULTS)
    rec = chat("Who holds card 4111?", generate=generate,
               run_query=run_query)

    assert rec["refused"] is True
    assert rec["answer"] == "UNKNOWN: the context lacks the account"
    assert rec["refusal_reason"] == "the context lacks the account"
    assert rec["grounded"] is False
    assert rec["claims"] == []


def test_chat_bare_prose_answer_fails_closed():
    """T-20: a model reply that is not protocol JSON is a refusal —
    the T-16 bare-answer fallback accepted prose; the review kills it."""
    generate, _ = _generate(_Q_REPLY, "Alice Chen")
    run_query, _ = _run_query(_RESULTS)
    rec = chat("Who holds card 4111?", generate=generate,
               run_query=run_query)

    assert rec["refused"] is True
    assert rec["answer"].startswith(UNKNOWN_PREFIX)


def test_chat_eval_examples_route():
    """Grounded and unknown eval examples thread the full path: the
    wrapper routes a refusal exactly when the example is unanswerable
    (the model replies UNKNOWN:), and a grounded answer otherwise."""
    from conftest import make_example

    grounded = make_example(
        task={"type": "grounded_qa", "difficulty": "factual",
              "requires": []},
        input={"question": "Who holds card 4111?"},
        expected={"answer": "Alice Chen", "koids": [], "evidence_ids": []})
    unknown = make_example(
        task={"type": "unknown", "difficulty": "unknown", "requires": []},
        input={"question": "Who holds the unissued card?"},
        expected={"answer": "UNKNOWN: no such card", "koids": [],
                  "evidence_ids": []},
        labels={"grounded": False, "answerable": False, "ambiguous": False,
                "contradictory": False})

    for ex, reply in ((grounded, _A_REPLY),
                      (unknown, json.dumps(
                          {"answer": "UNKNOWN: no such card",
                           "grounded": False, "claims": [],
                           "refusal_reason": "no such card"}))):
        generate, prompts = _generate(_Q_REPLY, reply)
        run_query, queries = _run_query(_RESULTS)
        rec = chat(ex["input"]["question"], generate=generate,
                   run_query=run_query)
        assert rec["query"] == _QUERY
        assert rec["statements"] == [_STATEMENT, "c" * 32]
        assert rec["refused"] is (not ex["labels"]["answerable"])
        assert len(prompts) == 2 and len(queries) == 1


def test_chat_script_pin():
    """The POC chatbot script exists and wires the real seams."""
    script = (Path(__file__).parents[1] / "scripts" / "chat.py")
    assert script.is_file(), "training/scripts/chat.py does not exist"
    text = script.read_text(encoding="utf-8")
    assert "from aikoql_training.chat import" in text
    assert re.search(r"Agent\.connect", text)
    assert "--adapter" in text and "--device" in text
    assert "input(" in text or "stdin" in text  # the chatbot loop
