"""Grounding validator (design Phase 12): claim -> context -> evidence.

An accepted example's answer must trace to a context fact statement,
every supporting fact must carry evidence present in the context's
evidence rows, and expected.evidence_ids must trace exactly to that
supporting evidence (identity shared with the answer generator via
evidence_id). Fail-closed in both directions of labels.grounded: a
grounded example that does not trace, or an ungrounded one that does,
is a violation — 100% of accepted examples grounded (§26 gate).

ponytail: tracing is a substring match of the answer inside the fact
statement; semantic-equivalence grounding is the fine-tuned model's
job at T-15, not a Python re-implementation.
"""

from __future__ import annotations

import json
import re
from typing import Any, Dict, List, Tuple

from aikoql_training.scenarios.answer_formats import (
    AMBIGUOUS_PREFIX,
    CONTRADICTED_PREFIX,
    UNKNOWN_PREFIX,
    ambiguity_values,
    contradiction_values,
)


def evidence_id(evidence: Dict[str, Any]) -> str:
    """The content-derived identity of an evidence entry: its canonical
    (sort-keyed) JSON. The generator stamps these into
    expected.evidence_ids; the validator recomputes them from
    context.evidence, so the trace is checkable by identity."""
    return json.dumps(evidence, sort_keys=True)


def _supporting_facts(example: Dict[str, Any]) -> List[dict]:
    answer = example["expected"]["answer"]
    if not answer.strip():
        return []
    return [
        f for f in example["context"]["facts"]
        if isinstance(f.get("statement"), str) and answer in f["statement"]
    ]


def _trace(example: Dict[str, Any]) -> Tuple[List[dict], List[dict], List[str]]:
    """The grounded trace shared by the generic and authorization
    branches: the supporting facts, the unbacked ones, and the traced
    evidence ids (context order)."""
    context = example["context"]
    supporting = _supporting_facts(example)
    unbacked = [
        f for f in supporting
        if not isinstance(f.get("evidence"), dict)
        or not _in_context(f["evidence"], context["evidence"])
    ]
    traced_ids: List[str] = []
    seen = set()
    for f in supporting:
        ev = f.get("evidence")
        if isinstance(ev, dict) and _in_context(ev, context["evidence"]):
            key = evidence_id(ev)
            if key not in seen:
                seen.add(key)
                traced_ids.append(key)
    return supporting, unbacked, traced_ids


def _in_context(evidence: dict, rows: List[Any]) -> bool:
    key = evidence_id(evidence)
    return any(isinstance(e, dict) and evidence_id(e) == key for e in rows)


def _validate_claims(example: Dict[str, Any], errors: List[str],
                     supporting: List[dict]) -> None:
    """T-22 (TDD-07): the claim walk — every claim carries
    claim -> fact -> evidence, and the decomposition is complete.
    Any dangling claim (a statement that traces to no evidenced fact,
    a forged evidence id, no ids at all) or hidden supporting fact is
    a violation; the claims' evidence union must equal
    expected.evidence_ids exactly. Level-1 grounding: the claim text
    IS the fact statement (semantic paraphrase is the model's job,
    never the validator's)."""
    context = example["context"]
    expected = example["expected"]
    claims = expected.get("claims")
    if not isinstance(claims, list):
        errors.append("claims must be a list")
        return
    claim_ids: List[str] = []
    seen = set()
    for claim in claims:
        if not isinstance(claim, dict) or set(claim) != {
                "statement", "evidence_ids"}:
            errors.append(f"malformed claim: {claim!r}")
            continue
        statement = claim["statement"]
        ids = claim["evidence_ids"]
        if not isinstance(statement, str) or not statement.strip():
            errors.append("claim statement must be a non-empty string")
            continue
        if not isinstance(ids, list) or not ids:
            errors.append(f"claim {statement!r} carries no evidence ids")
            continue
        backing = [
            f for f in context["facts"]
            if isinstance(f.get("statement"), str)
            and f["statement"] == statement
            and isinstance(f.get("evidence"), dict)
            and _in_context(f["evidence"], context["evidence"])
        ]
        if not backing:
            errors.append(
                f"claim {statement!r} does not trace to an evidenced fact")
            continue
        backing_ids = {evidence_id(f["evidence"]) for f in backing}
        for i in ids:
            if not isinstance(i, str) or i not in backing_ids:
                errors.append(
                    f"claim {statement!r} cites a forged evidence id {i!r}")
                continue
            if i not in seen:
                seen.add(i)
                claim_ids.append(i)
    for fact in supporting:
        statement = fact.get("statement")
        if not any(isinstance(c, dict) and c.get("statement") == statement
                   for c in claims):
            errors.append(
                f"supporting fact {statement!r} is not covered by a claim")
    if sorted(expected["evidence_ids"]) != sorted(claim_ids):
        errors.append("evidence_ids do not trace to the claims' evidence")


def _validate_provenance(example: Dict[str, Any]) -> dict:
    """Provenance examples (task.type == "provenance"): the answer IS a
    citation — grounding means every evidence_id traces to a REAL
    context evidence row."""
    errors: List[str] = []
    expected = example["expected"]
    if not expected["answer"].strip():
        errors.append("grounded example has an empty answer")
    if not expected["evidence_ids"]:
        errors.append("provenance example carries no evidence")
    context_ids = {
        evidence_id(e)
        for e in example["context"]["evidence"]
        if isinstance(e, dict)
    }
    missing = [i for i in expected["evidence_ids"] if i not in context_ids]
    if missing:
        errors.append(f"{len(missing)} evidence_id(s) absent from context")
    if not example["labels"]["grounded"]:
        errors.append("labels.grounded is false but the example cites evidence")
    return {"ok": not errors, "errors": errors}


def _validate_unknown(example: Dict[str, Any]) -> dict:
    """Unknown examples are refusals: grounded=False, answerable=False,
    no evidence_ids, the answer carries the UNKNOWN: prefix. Anything
    else is a false positive."""
    errors: List[str] = []
    expected = example["expected"]
    labels = example["labels"]
    if not expected["answer"].startswith(UNKNOWN_PREFIX):
        errors.append("unknown answer must carry the UNKNOWN: prefix")
    if labels["grounded"]:
        errors.append("labels.grounded is true on an unknown example")
    if labels["answerable"]:
        errors.append("labels.answerable is true on an unknown example")
    if labels["ambiguous"] or labels["contradictory"]:
        errors.append("unknown example cannot be ambiguous or contradictory")
    if expected["evidence_ids"]:
        errors.append("unknown example carries evidence_ids")
    return {"ok": not errors, "errors": errors}


def _validate_enumeration(
    example: Dict[str, Any],
    prefix: str,
    label: str,
    word: str,
    values: List[str],
    no_values_error: str,
) -> dict:
    """Ambiguity/contradiction: the answer carries the family prefix and
    the family label, every parsed candidate value traces to a context
    fact whose evidence is present, and expected.evidence_ids traces
    exactly to that evidence."""
    errors: List[str] = []
    expected = example["expected"]
    labels = example["labels"]
    answer = expected["answer"]
    if not answer.startswith(prefix):
        errors.append(f"{word} answer must start with {prefix!r}")
    if not labels[label]:
        errors.append(f"labels.{label} is false on a {word} example")
    if not labels["grounded"]:
        errors.append(f"labels.grounded is false on a {word} example")
    if not values:
        errors.append(no_values_error)
        return {"ok": False, "errors": errors}
    traced_ids: List[str] = []
    seen = set()
    for value in values:
        supporting = [
            f for f in example["context"]["facts"]
            if isinstance(f.get("statement"), str) and value in f["statement"]
        ]
        if not supporting:
            errors.append(f"candidate value {value!r} does not trace to a fact")
            continue
        for fact in supporting:
            ev = fact.get("evidence")
            if not isinstance(ev, dict) or not _in_context(
                ev, example["context"]["evidence"]
            ):
                errors.append(
                    f"candidate value {value!r}: evidence absent from context"
                )
                continue
            key = evidence_id(ev)
            if key not in seen:
                seen.add(key)
                traced_ids.append(key)
    if sorted(expected["evidence_ids"]) != sorted(traced_ids):
        errors.append("evidence_ids do not trace to the candidates' evidence")
    return {"ok": not errors, "errors": errors}


def _validate_ambiguity(example: Dict[str, Any]) -> dict:
    return _validate_enumeration(
        example,
        AMBIGUOUS_PREFIX,
        "ambiguous",
        "ambiguity",
        ambiguity_values(example["expected"]["answer"]),
        "ambiguity answer parses to no candidate values",
    )


def _validate_contradiction(example: Dict[str, Any]) -> dict:
    return _validate_enumeration(
        example,
        CONTRADICTED_PREFIX,
        "contradictory",
        "contradiction",
        contradiction_values(example["expected"]["answer"]),
        "contradiction answer must preserve the conflict metadata",
    )


# "whose <prop> is <value>?" — the anchor of an authorization question
_ANCHOR_RE = re.compile(r"whose ([A-Za-z_][A-Za-z0-9_]*) is ([^?]+)\?")


def _validate_authorization(example: Dict[str, Any]) -> dict:
    """Authorization examples carry the kernel's verdict machine-readably
    (ALLOWED:/DENIED: prefix). The labels are verdict-shaped, the policy
    section flags the example, and the trace is the generic one.
    Fail-closed on leakage: a DENIED example's context may carry the
    decision fact and nothing else that names the denied object —
    unauthorized knowledge never reaches the dataset context."""
    errors: List[str] = []
    answer = example["expected"]["answer"]
    labels = example["labels"]
    denied = answer.startswith("DENIED:")
    if not denied and not answer.startswith("ALLOWED:"):
        errors.append(
            "authorization answer must carry the ALLOWED:/DENIED: verdict"
        )
    if not (example.get("policy") or {}).get("authorization_required"):
        errors.append(
            "policy.authorization_required is false on an authorization example"
        )
    # T-25 (P0.6): the verdict prefix must agree with the recorded
    # kernel decision, and a denial must carry the kernel reason —
    # preserved verbatim in the answer — or the example is invalid.
    policy = example.get("policy") or {}
    if denied != (policy.get("decision") is False):
        errors.append("verdict prefix disagrees with policy.decision")
    reason = policy.get("reason")
    if denied and (not isinstance(reason, str) or not reason.strip()):
        errors.append("denial carries no kernel reason in policy")
    elif denied and reason not in answer:
        errors.append("policy.reason does not appear in the denied answer")
    if not labels["grounded"]:
        errors.append("labels.grounded is false on an authorization example")
    if not labels["answerable"]:
        errors.append("labels.answerable is false on an authorization example")
    if labels["ambiguous"] or labels["contradictory"]:
        errors.append(
            "authorization example cannot be ambiguous or contradictory"
        )
    supporting, unbacked, traced_ids = _trace(example)
    if not answer.strip():
        errors.append("grounded example has an empty answer")
    if not supporting:
        errors.append(
            "grounding failure: answer is not grounded — it does not "
            "trace to any context fact"
        )
    if unbacked:
        errors.append(
            f"{len(unbacked)} supporting fact(s) lack evidence in context"
        )
    if sorted(example["expected"]["evidence_ids"]) != sorted(traced_ids):
        errors.append(
            "evidence_ids do not trace to the supporting facts' evidence"
        )
    if "claims" in example["expected"]:
        _validate_claims(example, errors, supporting)
    if denied:
        m = _ANCHOR_RE.search(example["input"]["question"])
        if m:
            anchor_value = m.group(2)
            leaks = [
                f for f in example["context"]["facts"]
                if isinstance(f.get("statement"), str)
                and anchor_value in f["statement"]
                and f not in supporting
            ]
            if leaks:
                errors.append(
                    f"{len(leaks)} context fact(s) leak the denied object"
                )
    return {"ok": not errors, "errors": errors}


def validate_grounding(example: Dict[str, Any]) -> dict:
    """Fail-closed grounding check. Returns {"ok", "errors"}; errors are
    strings, counted by T-12's gates — never raises on grounding
    violations."""
    task_type = (example.get("task") or {}).get("type")
    if task_type == "provenance":
        return _validate_provenance(example)
    if task_type == "unknown":
        return _validate_unknown(example)
    if task_type == "ambiguity":
        return _validate_ambiguity(example)
    if task_type == "contradiction":
        return _validate_contradiction(example)
    if task_type == "authorization":
        return _validate_authorization(example)
    errors: List[str] = []
    expected = example["expected"]
    labels = example["labels"]
    answer = expected["answer"]

    supporting, unbacked, traced_ids = _trace(example)

    if labels["grounded"]:
        if not answer.strip():
            errors.append("grounded example has an empty answer")
        if not labels["answerable"]:
            errors.append("labels.answerable is false on a grounded answer")
        if labels["ambiguous"] or labels["contradictory"]:
            errors.append("grounded answer cannot be ambiguous or contradictory")
        if not supporting:
            errors.append(
                "grounding failure: answer is not grounded — it does not "
                "trace to any context fact"
            )
        if unbacked:
            errors.append(
                f"{len(unbacked)} supporting fact(s) lack evidence in context"
            )
        if sorted(expected["evidence_ids"]) != sorted(traced_ids):
            errors.append(
                "evidence_ids do not trace to the supporting facts' evidence"
            )
        if "claims" in expected:
            _validate_claims(example, errors, supporting)
    else:
        if expected.get("claims"):
            errors.append("ungrounded example carries claims")
        if supporting:
            errors.append(
                "labels.grounded is false but the answer is supported by "
                "context facts"
            )
        if expected["evidence_ids"]:
            errors.append("ungrounded example carries evidence_ids")

    return {"ok": not errors, "errors": errors}
