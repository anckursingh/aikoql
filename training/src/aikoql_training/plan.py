"""Logical knowledge plan derivation (design §41, PR #9 review P0.2).

plan_of turns the Scenario record — the generator's validated seed —
into the semantic target the model must learn: intent (the task type),
the path-positioned entities, the requirements the question demands,
and the ordered plan steps. Nothing is invented: every koid in the
plan comes from the scenario (training/tests/test_plan.py pins the
contract). policy_of derives the policy section the same way.
"""
from __future__ import annotations

from typing import Any, Dict, List, Tuple

from aikoql_training.scenarios.scenario import Scenario


def plan_of(scenario: Scenario) -> Tuple[str, List[Dict[str, str]],
                                        List[str], Dict[str, Any]]:
    """Derive (intent, entities, requirements, plan) from a Scenario."""
    path = scenario.expected_path
    koids = list(scenario.koids)

    # Path roles: first path node = subject, last = target, the nodes
    # between the hops = intermediate. Without a path the anchor koid is
    # the subject and every other koid is a candidate (ambiguity family).
    roles: Dict[str, str] = {}
    if path:
        roles[path[0][0]] = "subject"
        for edge in path[1:]:
            roles.setdefault(edge[0], "intermediate")
        roles[path[-1][2]] = "target"
    elif koids:
        roles[koids[0]] = "subject"
    for k in koids:
        roles.setdefault(k, "candidate")  # non-path koids: candidates
    entities = [{"koid": k, "role": roles[k]} for k in koids]

    requirements = sorted(
        {r for r in (scenario.property, scenario.anchor_prop) if r}
        | {edge[1] for edge in path})

    steps: List[Dict[str, Any]] = []
    if koids:
        steps.append({"op": "resolve_entity", "koid": koids[0]})
    for edge in path:
        steps.append({"op": "traverse", "from": edge[0],
                      "relation": edge[1], "to": edge[2]})
    if scenario.property and not path:
        steps.append({"op": "project", "properties": [scenario.property]})

    plan: Dict[str, Any] = {"steps": steps}
    if scenario.as_of is not None:
        plan["temporal"] = {"as_of": scenario.as_of}
    return scenario.task_type, entities, requirements, plan


def policy_of(scenario: Scenario) -> Dict[str, Any]:
    """policy section: authorization scenarios carry the ACL principal
    and action; everything else is authorization-free. A missing
    subject/action on an authorization scenario fails models.validate
    (authorization_required demands both) rather than silently
    downgrading the example."""
    if scenario.task_type == "authorization":
        return {"authorization_required": True,
                "subject": scenario.subject, "action": scenario.action}
    return {"authorization_required": False}
