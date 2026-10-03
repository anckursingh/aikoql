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

    if scenario.task_type == "provenance":
        # The answer is the evidence citation, not a property value: the
        # oracle proves the anchor KO was recovered with its property.
        # Evidence realness is validate_grounding's job (evidence_ids).
        row = next(
            (r for r in result_sets[0] if r.get("koid") == scenario.koids[0]),
            None,
        )
        if row is None:
            errors.append(f"provenance: anchor {scenario.koids[0]} not in results")
        elif (
            scenario.property is not None
            and scenario.property not in row.get("properties", {})
        ):
            errors.append(
                f"provenance: property {scenario.property} missing from the anchor"
            )
        return {"ok": not errors, "errors": errors}

    if scenario.task_type == "unknown":
        # The refusal is true only if the DB really has nothing: no
        # result row may carry the asked property.
        for i, rows in enumerate(result_sets):
            for r in rows:
                props = r.get("properties") or {}
                if scenario.property in props and props[scenario.property] is not None:
                    errors.append(
                        f"unknown: row in query {i} carries {scenario.property}"
                    )
        return {"ok": not errors, "errors": errors}

    if scenario.task_type in ("ambiguity", "contradiction"):
        # Both sides must be retrievable — the uncertainty is real.
        for i, rows in enumerate(result_sets):
            found = {
                str(r.get("properties", {}).get(scenario.property))
                for r in rows
                if scenario.property in (r.get("properties") or {})
            }
            missing = [v for _, v in scenario.candidates if v not in found]
            if missing:
                errors.append(f"query {i}: candidates not recovered: {missing}")
        return {"ok": not errors, "errors": errors}

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
