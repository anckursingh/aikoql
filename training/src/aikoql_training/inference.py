"""Model I/O helpers (design §40 + T-20): the prompt/parse contract
between the fine-tuned model and the pipeline.

T-20 replaces the §40 marker protocol (QUERY:/ANSWER: prose prefixes)
with a schema-validated JSON protocol: refusal and grounding are
FIELDS of the reply, not prose the pipeline sniffs for. Each parser
locates the first JSON object in the reply (stdlib raw_decode, so a
prose wrapper around the object is tolerated), then validates it
strictly: unknown fields, wrong types, or a cross-constraint
violation raise ModelOutputError and the caller fails closed.

Two skills, two shapes:

- query:  {"query": "<aikoql>"|null, "refusal_reason": str|null}
  with EXACTLY ONE of the two set.
- answer: {"answer": str, "grounded": bool,
           "claims": [{"statement": str, "evidence_ids": [str]}],
           "refusal_reason": str|null}
  with the cross-constraints pinned both directions: a refusal is an
  UNKNOWN: answer with no claims and grounded=false; a grounded
  answer carries claims. The answer itself keeps the T-09
  machine-readable UNKNOWN: format.

Missing fields, duplicate keys (a model emitting dupes is emitting
garbage, not a vote the parser resolves) and JSON too deeply nested
for the decoder all raise ModelOutputError — the caller refuses
rather than guessing.
"""

from __future__ import annotations

import json
from typing import Dict, List

from aikoql_training.errors import ModelOutputError
from aikoql_training.scenarios.answer_formats import UNKNOWN_PREFIX


def _strict_pairs(pairs: List) -> Dict:
    """object_pairs_hook: a duplicate key anywhere in the reply (the
    top level or a nested claim) is invalid protocol, not a silent
    last-wins tie the decoder breaks for the model."""
    obj: Dict = {}
    for key, value in pairs:
        if key in obj:
            raise _fail(f"duplicate key in model reply: {key!r}")
        obj[key] = value
    return obj


_DECODER = json.JSONDecoder(object_pairs_hook=_strict_pairs)


def _json_object(text: str) -> Dict:
    """The first JSON object anywhere in the reply; ModelOutputError
    when nothing decodes (a bare prose reply is a refusal, not an
    answer the pipeline guesses at)."""
    text = (text or "").strip()
    if not text:
        raise ModelOutputError("empty model reply", stage="inference",
                               code="MODEL_OUTPUT")
    start = text.find("{")
    if start == -1:
        raise ModelOutputError("model reply carries no JSON object",
                               stage="inference", code="MODEL_OUTPUT")
    try:
        obj, _ = _DECODER.raw_decode(text, start)
    except (ValueError, RecursionError) as e:
        raise ModelOutputError(f"model reply is not valid JSON: {e!r}",
                               stage="inference", code="MODEL_OUTPUT")
    if not isinstance(obj, dict):
        raise ModelOutputError("model reply is not a JSON object",
                               stage="inference", code="MODEL_OUTPUT")
    return obj


def _fail(message: str) -> ModelOutputError:
    return ModelOutputError(message, stage="inference", code="MODEL_OUTPUT")


def build_query_prompt(question: str) -> str:
    return (
        f"Question: {question}\n"
        "Reply with exactly one JSON object, no other text:\n"
        '{"query": "<aikoql query>"}\n'
        "If the question cannot be answered from a knowledge graph, "
        "reply:\n"
        '{"query": null, "refusal_reason": "<short reason>"}\n'
    )


def build_answer_prompt(question: str, statements: List[str]) -> str:
    lines = "\n".join(f"- {s}" for s in statements)
    return (
        f"Question: {question}\n\n"
        f"Knowledge context:\n{lines}\n\n"
        "Answer the question from the context above. Reply with "
        "exactly one JSON object, no other text:\n"
        '{"answer": "<answer>", "grounded": true,\n'
        ' "claims": [{"statement": "<a context statement supporting '
        'the answer>", "evidence_ids": ["<evidence id>"]}],\n'
        ' "refusal_reason": null}\n'
        "If the context does not contain the answer, reply:\n"
        '{"answer": "UNKNOWN: <short reason>", "grounded": false, '
        '"claims": [], "refusal_reason": "<short reason>"}\n'
    )


def parse_query_reply(text: str) -> Dict:
    """{"query": str|null, "refusal_reason": str|null} with exactly
    one of the two set. Returns the dict; raises ModelOutputError on
    any deviation (fail-closed: a broken reply is a refusal upstream,
    never a silently empty query)."""
    obj = _json_object(text)
    unknown = set(obj) - {"query", "refusal_reason"}
    if unknown:
        raise _fail(f"unknown fields in query reply: {sorted(unknown)!r}")
    missing = {"query", "refusal_reason"} - set(obj)
    if missing:
        raise _fail(f"missing fields in query reply: {sorted(missing)!r}")
    query, reason = obj["query"], obj["refusal_reason"]
    if not isinstance(query, (str, type(None))):
        raise _fail("query must be a string or null")
    if reason is not None and not isinstance(reason, str):
        raise _fail("refusal_reason must be a string or null")
    if (query is None) == (reason is None):
        raise _fail("exactly one of query and refusal_reason must be set")
    if query is not None and not query.strip():
        raise _fail("query must not be empty")
    if reason is not None and not reason.strip():
        raise _fail("refusal_reason must not be empty")
    return {"query": query, "refusal_reason": reason}


def parse_answer_reply(text: str) -> Dict:
    """The answer shape with refusal/grounding as fields; every
    cross-constraint holds in BOTH directions (a refusal that claims
    to be grounded is as broken as a grounded answer without claims).
    Returns the dict; raises ModelOutputError on any deviation."""
    obj = _json_object(text)
    unknown = set(obj) - {"answer", "grounded", "claims", "refusal_reason"}
    if unknown:
        raise _fail(f"unknown fields in answer reply: {sorted(unknown)!r}")
    missing = {"answer", "grounded", "claims", "refusal_reason"} - set(obj)
    if missing:
        raise _fail(f"missing fields in answer reply: {sorted(missing)!r}")
    answer, grounded = obj["answer"], obj["grounded"]
    claims, reason = obj["claims"], obj["refusal_reason"]
    if not isinstance(answer, str) or not answer.strip():
        raise _fail("answer must be a non-empty string")
    if not isinstance(grounded, bool):
        raise _fail("grounded must be a boolean")
    if not isinstance(claims, list):
        raise _fail("claims must be a list")
    for claim in claims:
        if not isinstance(claim, dict) or set(claim) != {
                "statement", "evidence_ids"}:
            raise _fail(f"malformed claim: {claim!r}")
        if not isinstance(claim["statement"], str) \
                or not claim["statement"].strip():
            raise _fail("claim statement must be a non-empty string")
        ids = claim["evidence_ids"]
        if not isinstance(ids, list) or any(
                not isinstance(i, str) or not i.strip() for i in ids):
            raise _fail("claim evidence_ids must be non-empty strings")
    if reason is not None and (not isinstance(reason, str)
                               or not reason.strip()):
        raise _fail("refusal_reason must be a non-empty string or null")
    is_refusal = reason is not None
    if is_refusal != answer.startswith(UNKNOWN_PREFIX):
        raise _fail("refusal_reason and the UNKNOWN: answer must agree")
    if is_refusal and grounded:
        raise _fail("a refusal cannot be grounded")
    if grounded != (len(claims) > 0):
        raise _fail("grounded must agree with the claims")
    return {"answer": answer, "grounded": grounded, "claims": claims,
            "refusal_reason": reason}
