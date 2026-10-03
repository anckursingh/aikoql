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
from typing import Any, Dict, List


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


def _in_context(evidence: dict, rows: List[Any]) -> bool:
    key = evidence_id(evidence)
    return any(isinstance(e, dict) and evidence_id(e) == key for e in rows)


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


def validate_grounding(example: Dict[str, Any]) -> dict:
    """Fail-closed grounding check. Returns {"ok", "errors"}; errors are
    strings, counted by T-12's gates — never raises on grounding
    violations."""
    if (example.get("task") or {}).get("type") == "provenance":
        return _validate_provenance(example)
    errors: List[str] = []
    expected = example["expected"]
    labels = example["labels"]
    context = example["context"]
    answer = expected["answer"]

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

    if labels["grounded"]:
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
        if sorted(expected["evidence_ids"]) != sorted(traced_ids):
            errors.append(
                "evidence_ids do not trace to the supporting facts' evidence"
            )
    else:
        if supporting:
            errors.append(
                "labels.grounded is false but the answer is supported by "
                "context facts"
            )
        if expected["evidence_ids"]:
            errors.append("ungrounded example carries evidence_ids")

    return {"ok": not errors, "errors": errors}
