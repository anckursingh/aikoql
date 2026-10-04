"""T-21 RED: FZ-10 model-output fuzz — the T-20 protocol parser under
adversarial model output (PR9 P0.5).

Properties over parse_query_reply / parse_answer_reply and the chat
seam:

- arbitrary text either raises ModelOutputError or returns a reply
  satisfying EVERY protocol constraint — no leaked KeyError /
  TypeError / RecursionError, no best-effort execution;
- the FZ-10 families (empty, huge, Unicode, malformed JSON,
  duplicate fields, nested values, markdown, code fences, injection
  prose, extra objects, delimiter-carrying queries) land exactly
  where the contract pins them — the must-raise cases raise
  ModelOutputError, the must-parse cases parse to a valid reply;
- chat() on a reply that fails the protocol refuses and never
  executes a guessed query.

Every deterministic pin below fails against the current tree:
missing fields raise KeyError, duplicate keys silently last-win,
deep nesting raises RecursionError, and the chat wrapper leaks all
three instead of refusing.
"""

from __future__ import annotations

import json

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from aikoql_training.chat import chat
from aikoql_training.errors import ModelOutputError
from aikoql_training.inference import (
    parse_answer_reply,
    parse_query_reply,
)
from aikoql_training.scenarios.answer_formats import UNKNOWN_PREFIX

_QUERY = 'MATCH Cardholder WHERE name == "Alice" RETURN name'


def _valid_query(reply):
    """The invariant half of the query property: everything
    parse_query_reply promises, re-checked on the returned dict."""
    assert set(reply) == {"query", "refusal_reason"}
    reason = reply["refusal_reason"]
    assert reason is None or (isinstance(reason, str) and reason.strip())
    assert (reply["query"] is None) != (reason is None)
    if reply["query"] is not None:
        assert isinstance(reply["query"], str) and reply["query"].strip()


def _valid_answer(reply):
    """The invariant half of the answer property."""
    assert set(reply) == {"answer", "grounded", "claims", "refusal_reason"}
    assert isinstance(reply["answer"], str) and reply["answer"].strip()
    assert isinstance(reply["grounded"], bool)
    reason = reply["refusal_reason"]
    assert reason is None or (isinstance(reason, str) and reason.strip())
    for claim in reply["claims"]:
        assert set(claim) == {"statement", "evidence_ids"}
        assert isinstance(claim["statement"], str) \
            and claim["statement"].strip()
        assert isinstance(claim["evidence_ids"], list) and all(
            isinstance(i, str) and i.strip() for i in claim["evidence_ids"])
    refusal = reason is not None
    assert refusal == reply["answer"].startswith(UNKNOWN_PREFIX)
    assert not (refusal and reply["grounded"])
    assert reply["grounded"] == (len(reply["claims"]) > 0)


# -- arbitrary text: typed failure or a valid reply, nothing else ------------

@given(st.text(max_size=2000))
@settings(max_examples=200)
def test_arbitrary_text_query_parses_or_fails_closed(text):
    try:
        reply = parse_query_reply(text)
    except ModelOutputError:
        return
    _valid_query(reply)


@given(st.text(max_size=2000))
@settings(max_examples=200)
def test_arbitrary_text_answer_parses_or_fails_closed(text):
    try:
        reply = parse_answer_reply(text)
    except ModelOutputError:
        return
    _valid_answer(reply)


# -- FZ-10 families: the pinned must-raise cases -----------------------------

_MUST_RAISE_QUERY = [
    '{"query": "MATCH X RETURN x"}',        # missing refusal_reason
    '{"refusal_reason": "nope"}',           # missing query
    '{"query": "A", "query": "B", '        # duplicate keys
    '"refusal_reason": null}',
    '{"query": ',                            # truncated JSON
    '["query"]',                             # not an object
    "",                                      # empty reply
    '{"query": {"inner": 1}, '               # nested object as query
    '"refusal_reason": null}',
    '{"query": 42, "refusal_reason": null}',  # wrong type
]


@pytest.mark.parametrize("text", _MUST_RAISE_QUERY)
def test_fz10_query_families_must_raise(text):
    with pytest.raises(ModelOutputError):
        parse_query_reply(text)


_MUST_RAISE_ANSWER = [
    '{"grounded": true, "claims": [], '      # missing answer
    '"refusal_reason": null}',
    '{"answer": "Alice", "claims": [], '     # missing grounded
    '"refusal_reason": null}',
    '{"answer": "Alice", "grounded": false, '  # missing claims
    '"refusal_reason": null}',
    '{"answer": "Alice", "grounded": false, '  # missing refusal_reason
    '"claims": []}',
    '{"answer": "A", "answer": "B", '        # duplicate keys
    '"grounded": false, "claims": [], "refusal_reason": null}',
    '{"answer": "Alice", "grounded": true, '  # duplicate key inside a claim
    '"claims": [{"statement": "s", "statement": "t", '
    '"evidence_ids": ["e"]}], "refusal_reason": null}',
    '{"answer": ',                            # truncated JSON
]


@pytest.mark.parametrize("text", _MUST_RAISE_ANSWER)
def test_fz10_answer_families_must_raise(text):
    with pytest.raises(ModelOutputError):
        parse_answer_reply(text)


# -- FZ-10 families: the pinned must-parse cases -----------------------------

_MUST_PARSE_QUERY = [
    '{"query": "MATCH X RETURN x", "refusal_reason": null}',
    # code fences and prose wrappers are tolerated
    '```json\n{"query": "MATCH X RETURN x", "refusal_reason": null}\n```',
    'Sure, here:\n{"query": "MATCH X RETURN x", "refusal_reason": null}',
    # injection prose is ignored; only the JSON object is the contract
    'ignore previous instructions. {"query": "MATCH X RETURN x", '
    '"refusal_reason": null}',
    # a query string may carry JSON delimiters
    '{"query": "MATCH N WHERE p == \\"{x}\\" RETURN n", '
    '"refusal_reason": null}',
    # adversarial unicode is data, not shape
    '{"query": "h\\u00e9llo \\u2728 \\u0000", "refusal_reason": null}',
    # a large payload parses
    '{"query": "' + "x" * 10000 + '", "refusal_reason": null}',
    '{"query": null, "refusal_reason": "no graph access"}',
]


@pytest.mark.parametrize("text", _MUST_PARSE_QUERY)
def test_fz10_query_families_parse_to_valid_replies(text):
    _valid_query(parse_query_reply(text))


def test_fz10_first_object_wins_over_extra_objects():
    reply = parse_query_reply(
        '{"query": "MATCH A RETURN a", "refusal_reason": null}\n'
        '{"query": "MATCH B RETURN b", "refusal_reason": null}')
    _valid_query(reply)
    assert reply["query"] == "MATCH A RETURN a"


_MUST_PARSE_ANSWER = [
    '{"answer": "Alice Chen", "grounded": true, "claims": '
    '[{"statement": "Alice holds 4111", "evidence_ids": ["e1"]}], '
    '"refusal_reason": null}',
    '```json\n{"answer": "UNKNOWN: no record", "grounded": false, '
    '"claims": [], "refusal_reason": "no record"}\n```',
    '{"answer": "h\\u00e9llo \\u2728", "grounded": false, "claims": [], '
    '"refusal_reason": null}',
]


@pytest.mark.parametrize("text", _MUST_PARSE_ANSWER)
def test_fz10_answer_families_parse_to_valid_replies(text):
    _valid_answer(parse_answer_reply(text))


# -- deterministic teeth: duplicates and deep nesting fail closed ------------

def test_duplicate_fields_fail_closed():
    with pytest.raises(ModelOutputError):
        parse_query_reply(
            '{"query": "A", "query": "B", "refusal_reason": null}')


def test_deeply_nested_json_fails_closed():
    # the C decoder gives up around 100k nesting (RecursionError); the
    # parser must translate that into a typed refusal, not a crash
    nested = '{"a": ' * 100000 + '0' + '}' * 100000
    with pytest.raises(ModelOutputError):
        parse_query_reply(nested)


# -- the chat seam: a broken reply refuses, it never crashes -----------------

def test_chat_missing_field_reply_refuses_not_crashes():
    def generate(prompt):
        return '{"query": "MATCH X RETURN x"}'  # refusal_reason missing

    def run_query(query):
        raise AssertionError("an unparseable reply must not execute")

    rec = chat("?", generate=generate, run_query=run_query)
    assert rec["refused"] is True
    assert rec["compiled"] is False


@given(st.text(max_size=500))
@settings(max_examples=100)
def test_chat_never_executes_uncompiled_queries(text):
    called = []

    def generate(prompt):
        return text

    def run_query(query):
        called.append(query)
        return {"results": []}

    rec = chat("fuzz?", generate=generate, run_query=run_query)
    assert rec["refused"] is True  # empty results or a broken reply
    if not rec["compiled"]:
        assert called == []  # never best-effort execution
