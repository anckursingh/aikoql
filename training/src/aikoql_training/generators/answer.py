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

from aikoql_training.scenarios.scenario import Scenario
from aikoql_training.validation.grounding import evidence_id


def build_answer(scenario: Scenario, context: Dict[str, Any]) -> Optional[Dict[str, Any]]:
    """Ground `scenario.expected_answer` in `context` (the adapter's
    {entities, facts, relations, evidence} shape) and return
    {"answer", "evidence_ids"} — or None when the claim cannot be
    fully traced (no supporting fact, or required evidence absent)."""
    answer = scenario.expected_answer
    if not answer.strip():
        return None
    evidence_rows = context.get("evidence", [])
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

    return {"answer": answer, "evidence_ids": ids}
