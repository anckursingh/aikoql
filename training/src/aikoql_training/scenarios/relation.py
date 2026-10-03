"""Relation scenario generator (design Phase 4, §11.2).

Edge A --OWNS--> B yields two scenarios: forward ("Who owns B?" ->
A) and inverse ("What does A own?" -> B). Each carries the exact
edge as expected_path — stored and validated, never invented:
edges to unknown KOs are skipped (a dangling reference has no
grounded answer), and a missing relation generates nothing.
"""

from __future__ import annotations

from typing import List

from aikoql_training.scenarios.scenario import Scenario, ref_of

# Third-person ("Who OWNS B?") and base ("What does A own?") verb forms
# for the POC's relation types. Fallback is the raw lowercased type —
# unmapped relations may read awkwardly; the FZ-T4 template engine
# (T-04+) owns question phrasing properly.
_REL_VERBS = {
    "OWNS": ("owns", "own"),
    "DEPENDS_ON": ("depends on", "depend on"),
    "MENTIONS": ("mentions", "mention"),
    "REPORTS_TO": ("reports to", "report to"),
    "CONTAINS": ("contains", "contain"),
    "USES": ("uses", "use"),
}


def _verbs(rel_type: str):
    third, base = _REL_VERBS.get(rel_type, (None, None))
    if third is None:
        return rel_type.lower(), rel_type.lower()
    return third, base


def relation_scenarios(edges: List[dict], kos: List[dict], rel_type: str) -> List[Scenario]:
    by_koid = {k["koid"]: k for k in kos}
    scenarios = []
    for edge in sorted(edges, key=lambda e: (e["from"], e["rel"], e["to"])):
        if edge["rel"] != rel_type:
            continue
        f, t = edge["from"], edge["to"]
        if f not in by_koid or t not in by_koid:
            continue  # dangling edge: no grounded answer exists
        path = ((f, rel_type, t),)
        third, base = _verbs(rel_type)
        scenarios.append(
            Scenario(
                scenario_id=f"relation:{rel_type}:{f}:{t}:forward",
                task_type="grounded_qa",
                difficulty="one_hop",
                question=f"Who {third} {ref_of(t, by_koid)}?",
                expected_answer=ref_of(f, by_koid),
                koids=(f, t),
                expected_path=path,
            )
        )
        scenarios.append(
            Scenario(
                scenario_id=f"relation:{rel_type}:{f}:{t}:inverse",
                task_type="grounded_qa",
                difficulty="one_hop",
                question=f"What does {ref_of(f, by_koid)} {base}?",
                expected_answer=ref_of(t, by_koid),
                koids=(f, t),
                expected_path=path,
            )
        )
    return scenarios
