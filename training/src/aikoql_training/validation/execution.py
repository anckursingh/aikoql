"""The oracle (design Phase 10): prove a scenario's queries against the
live database.

Every generated query must pass compile -> plan -> execute ->
scenario-match (the design's 6-point acceptance; auth and evidence
join in T-08/T-10). The verifier drives the public tool_aikoql surface
— it never re-implements the grammar — and reports failures instead of
raising, because T-12's gates count them.

scenario-match = every hop target in expected_path reappears in its
query's result set, and (factual scenarios) the anchor KO's live
property value equals the expected answer. For a forward relation
scenario the answer IS the scan anchor: its existence is proven by the
anchor predicate matching, the path by the traverse result.
"""

from __future__ import annotations

from typing import Any, List

from aikoql_training.scenarios.scenario import Scenario


def verify_scenario(db: Any, scenario: Scenario, queries: List[str]) -> dict:
    errors: List[str] = []
    if not queries:
        return {"ok": False, "errors": ["no queries to verify"]}

    result_sets = []
    for i, q in enumerate(queries):
        try:
            env = db.aikoql(q)
            result_sets.append(env.get("results", []))
        except Exception as e:  # a compile/execute failure is a gate count
            return {"ok": False, "errors": [f"query {i} failed: {e}"]}

    for i, (_, _, target) in enumerate(scenario.expected_path):
        if not any(r.get("koid") == target for r in result_sets[i]):
            errors.append(f"hop {i}: target {target} not recovered")

    if scenario.property is not None:
        found = False
        for r in result_sets[0]:
            if r.get("koid") != scenario.koids[0]:
                continue
            live = r.get("properties", {}).get(scenario.property)
            if str(live) != scenario.expected_answer:
                errors.append(
                    f"factual: live {scenario.property}={live!r} != "
                    f"expected {scenario.expected_answer!r}"
                )
            found = True
        if not found:
            errors.append(f"factual: anchor {scenario.koids[0]} not in results")

    return {"ok": not errors, "errors": errors}
