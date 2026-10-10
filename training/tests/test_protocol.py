"""T-20 RED: the structured JSON model protocol (PR9, the T-15 seam).

The §40 seam speaks to the model in marker prefixes — QUERY: /
ANSWER: — and the pipeline splits replies on them: prose the parser
sniffs for. PR9: the model must output structured JSON with a
schema-validated parser; refusal and grounding are FIELDS, not
prose. The contract pinned here:

- `parse_query_reply`: exactly {"query": str|null,
  "refusal_reason": str|null} with EXACTLY ONE of the two set;
  unknown fields, wrong types and a missing JSON object raise
  ModelOutputError (fail-closed).
- `parse_answer_reply`: exactly {"answer", "grounded", "claims",
  "refusal_reason"}; claims are {"statement", "evidence_ids"} with
  non-empty strings; the cross-constraints hold both directions —
  refusal_reason set  <=>  UNKNOWN: answer with no claims and
  grounded=false; grounded  <=>  claims non-empty.
- prose wrappers are tolerated (the FIRST JSON object in the reply
  is decoded — stdlib raw_decode), anything else fails closed.
- the prompts demand the JSON shapes, refusal included.

Every test below fails against the current tree: only
parse_model_reply exists.
"""

from __future__ import annotations

import json

import pytest

from aikoql_training.errors import ModelOutputError
from aikoql_training.inference import (
    build_answer_prompt,
    build_query_prompt,
    parse_answer_reply,
    parse_query_reply,
)

_QUERY = "MATCH Cardholder WHERE name == \"Alice\" RETURN name"
_CLAIM = {"statement": "Alice Chen holds card 4111",
          "evidence_ids": ["e1"]}


def _query_reply(query=_QUERY):
    return json.dumps({"query": query, "refusal_reason": None})


def _answer_reply(answer="Alice Chen"):
    return json.dumps({"answer": answer, "grounded": True,
                       "claims": [_CLAIM], "refusal_reason": None})


def _refusal_reply(reason="no such record"):
    return json.dumps({"answer": f"UNKNOWN: {reason}", "grounded": False,
                       "claims": [], "refusal_reason": reason})


# -- query replies -----------------------------------------------------------

def test_query_reply_parses():
    assert parse_query_reply(_query_reply()) == {
        "query": _QUERY, "refusal_reason": None}


def test_query_reply_refusal_parses():
    out = parse_query_reply(
        '{"query": null, "refusal_reason": "no graph access"}')
    assert out == {"query": None, "refusal_reason": "no graph access"}


def test_query_reply_prose_wrapper_tolerated():
    out = parse_query_reply(f"Sure, here you go:\n{_query_reply()}\n")
    assert out == {"query": _QUERY, "refusal_reason": None}


def test_query_reply_requires_exactly_one_of_query_and_refusal():
    for payload in (
        '{"query": null, "refusal_reason": null}',
        json.dumps({"query": _QUERY, "refusal_reason": "also"}),
    ):
        with pytest.raises(ModelOutputError):
            parse_query_reply(payload)


def test_query_reply_rejects_unknown_fields():
    with pytest.raises(ModelOutputError):
        parse_query_reply(json.dumps(
            {"query": _QUERY, "refusal_reason": None, "confidence": 0.9}))


def test_query_reply_rejects_wrong_types():
    with pytest.raises(ModelOutputError):
        parse_query_reply('{"query": 42, "refusal_reason": null}')


def test_query_reply_rejects_prose_without_json():
    with pytest.raises(ModelOutputError):
        parse_query_reply("I cannot help with that.")


# -- answer replies ----------------------------------------------------------

def test_answer_reply_parses():
    assert parse_answer_reply(_answer_reply()) == {
        "answer": "Alice Chen", "grounded": True,
        "claims": [_CLAIM], "refusal_reason": None}


def test_answer_reply_refusal_parses():
    out = parse_answer_reply(_refusal_reply())
    assert out == {"answer": "UNKNOWN: no such record", "grounded": False,
                   "claims": [], "refusal_reason": "no such record"}


def test_answer_reply_prose_wrapper_tolerated():
    out = parse_answer_reply(f"Answer:\n{_answer_reply()}\n")
    assert out["answer"] == "Alice Chen"
    assert out["claims"] == [_CLAIM]


def test_answer_reply_grounding_must_agree_with_claims():
    for grounded, claims in ((True, []), (False, [_CLAIM])):
        with pytest.raises(ModelOutputError):
            parse_answer_reply(json.dumps(
                {"answer": "Alice Chen", "grounded": grounded,
                 "claims": claims, "refusal_reason": None}))


def test_answer_reply_refusal_must_be_an_unknown_answer():
    with pytest.raises(ModelOutputError):
        parse_answer_reply(json.dumps(
            {"answer": "Alice Chen", "grounded": False, "claims": [],
             "refusal_reason": "no such record"}))


def test_answer_reply_refusal_reason_with_claims_fails():
    with pytest.raises(ModelOutputError):
        parse_answer_reply(json.dumps(
            {"answer": "UNKNOWN: no such record", "grounded": True,
             "claims": [_CLAIM], "refusal_reason": "no such record"}))


def test_answer_reply_rejects_unknown_fields():
    with pytest.raises(ModelOutputError):
        parse_answer_reply(json.dumps(
            {"answer": "Alice Chen", "grounded": True, "claims": [_CLAIM],
             "refusal_reason": None, "score": 1.0}))


def test_answer_reply_rejects_malformed_claims():
    for claim in (
        {"statement": "x"},  # evidence_ids missing
        {"statement": "x", "evidence_ids": [42]},  # non-string id
        {"statement": "x", "evidence_ids": [], "extra": 1},  # unknown key
    ):
        with pytest.raises(ModelOutputError):
            parse_answer_reply(json.dumps(
                {"answer": "Alice Chen", "grounded": True,
                 "claims": [claim], "refusal_reason": None}))


def test_answer_reply_rejects_prose_without_json():
    with pytest.raises(ModelOutputError):
        parse_answer_reply("The answer is Alice Chen.")


# -- prompts demand the JSON contract ----------------------------------------

def test_query_prompt_demands_the_json_shape():
    prompt = build_query_prompt("Who holds card 4111?")
    assert '"query"' in prompt and '"refusal_reason"' in prompt
    assert "JSON" in prompt


def test_answer_prompt_demands_claims_and_refusal_fields():
    prompt = build_answer_prompt("Who holds card 4111?", ["a fact"])
    for field in ('"answer"', '"grounded"', '"claims"',
                  '"evidence_ids"', '"refusal_reason"'):
        assert field in prompt
    assert "UNKNOWN:" in prompt  # the refusal form is taught inline
