"""Answer generator (design Phase 12): certify the scenario's answer
against the compiled context.

The answer is grounded when it traces to a context fact statement AND
every supporting fact's evidence is present in the context's evidence
rows; the returned evidence_ids are those evidence entries' canonical
identities in package order. Anything short of a full trace is a
REFUSAL (None) — an unsupported claim is never emitted, so the §26
grounding gate (100% of accepted examples grounded) holds by
construction for everything the generator accepts.

The trace rule is shared with validation/grounding.py (evidence_id +
the same fact/evidence matching), so validate_grounding accepts
exactly what build_answer emits.
"""

from __future__ import annotations

from typing import Any, Dict, List, Optional

from aikoql_training.scenarios.answer_formats import UNKNOWN_PREFIX
from aikoql_training.scenarios.scenario import Scenario
from aikoql_training.validation.grounding import evidence_id

# The label truth table per task family (T-09: machine-readable labels;
# uncertainty never becomes a false positive).
_GROUNDED_LABELS = {"grounded": True, "answerable": True,
                    "ambiguous": False, "contradictory": False}
_UNKNOWN_LABELS = {"grounded": False, "answerable": False,
                   "ambiguous": False, "contradictory": False}
_AMBIGUOUS_LABELS = {"grounded": True, "answerable": True,
                     "ambiguous": True, "contradictory": False}
_CONTRADICTED_LABELS = {"grounded": True, "answerable": True,
                        "ambiguous": False, "contradictory": True}


def build_answer(scenario: Scenario, context: Dict[str, Any]) -> Optional[Dict[str, Any]]:
    """Ground `scenario.expected_answer` in `context` (the adapter's
    {entities, facts, relations, evidence} shape) and return
    {"answer", "evidence_ids", "labels"} — or None when the claim
    cannot be fully traced (no supporting fact, or required evidence
    absent)."""
    answer = scenario.expected_answer
    if not answer.strip():
        return None
    evidence_rows = context.get("evidence", [])

    if scenario.task_type == "provenance":
        # The answer IS the citation of the scenario's evidence: every
        # cited entry must appear in the compiled context, or refused.
        if not scenario.evidence:
            return None
        for ev in scenario.evidence:
            if not any(
                isinstance(e, dict) and evidence_id(e) == evidence_id(ev)
                for e in evidence_rows
            ):
                return None
        return {"answer": answer,
                "evidence_ids": [evidence_id(e) for e in scenario.evidence],
                "labels": dict(_GROUNDED_LABELS)}

    if scenario.task_type == "unknown":
        # The refusal IS the answer: grounded=False, no evidence.
        # Fail-closed on the premise — if the context actually knows
        # the missing name the question was never unknown.
        if not answer.startswith(UNKNOWN_PREFIX):
            return None
        if not scenario.koids and scenario.anchor_value is not None:
            needle = str(scenario.anchor_value)
            if any(isinstance(f.get("statement"), str) and needle in f["statement"]
                   for f in context.get("facts", [])):
                return None
        return {"answer": answer, "evidence_ids": [],
                "labels": dict(_UNKNOWN_LABELS)}

    if scenario.task_type in ("ambiguity", "contradiction"):
        # Every candidate value must trace, or the enumeration would be
        # a partial claim — refused. Each supporting fact's evidence
        # must be present in the context rows.
        if not scenario.candidates:
            return None
        ids: List[str] = []
        seen = set()
        for _, value in scenario.candidates:
            supporting = [
                f for f in context.get("facts", [])
                if isinstance(f.get("statement"), str) and value in f["statement"]
            ]
            if not supporting:
                return None
            for fact in supporting:
                ev = fact.get("evidence")
                if not isinstance(ev, dict) or not any(
                    isinstance(e, dict) and evidence_id(e) == evidence_id(ev)
                    for e in evidence_rows
                ):
                    return None
                key = evidence_id(ev)
                if key not in seen:
                    seen.add(key)
                    ids.append(key)
        labels = (_AMBIGUOUS_LABELS if scenario.task_type == "ambiguity"
                  else _CONTRADICTED_LABELS)
        return {"answer": answer, "evidence_ids": ids, "labels": dict(labels)}

    supporting = [
        f for f in context.get("facts", [])
        if isinstance(f.get("statement"), str) and answer in f["statement"]
    ]
    if not supporting:
        return None

    ids: List[str] = []
    seen = set()
    for fact in supporting:
        ev = fact.get("evidence")
        if not isinstance(ev, dict) or not any(
            isinstance(e, dict) and evidence_id(e) == evidence_id(ev)
            for e in evidence_rows
        ):
            return None  # required evidence absent -> the example is refused
        key = evidence_id(ev)
        if key not in seen:
            seen.add(key)
            ids.append(key)

    return {"answer": answer, "evidence_ids": ids,
            "labels": dict(_GROUNDED_LABELS)}
