"""Authorization scenario generator (design Phase 9): verdict questions
through the real ACL path.

`decisions` are the kernel's own policy evaluations — records of
(principal, action, resource_type) with the live verdict (allowed bool)
and, for denials, the kernel's reason ("Denied by policy: <koid>").
Each decision pairs with every KO of its resource type: the question
names the object, the answer is the machine-readable verdict
(ALLOWED:/DENIED:) preserving the reason verbatim — the engine never
re-derives a verdict, the oracle re-checks it live
(evaluate_policies). Malformed records, unknown actions, denials
without a reason, decisions over types with no KOs and anchors that
would corrupt the question are skipped.
"""

from __future__ import annotations

from typing import Any, Dict, List

from aikoql_training.scenarios.scenario import Scenario
from aikoql_training.scenarios.unknown import _anchor_pair

# the kernel's policy action set (deployment.rs parse_action)
_ACTIONS = frozenset({"read", "write", "admin", "evolve", "delete"})


def authorization_scenarios(
    kos: List[Dict[str, Any]], decisions: List[Dict[str, Any]]
) -> List[Scenario]:
    by_type: Dict[str, List[dict]] = {}
    for ko in kos:
        by_type.setdefault(ko.get("type_name"), []).append(ko)

    scenarios = []
    key = lambda d: (  # noqa: E731
        str(d.get("principal", "")),
        str(d.get("action", "")),
        str(d.get("resource_type", "")),
        str(d.get("allowed")),
    )
    for decision in sorted(decisions, key=key):
        principal = decision.get("principal")
        action = decision.get("action")
        resource_type = decision.get("resource_type")
        allowed = decision.get("allowed")
        reason = decision.get("reason")
        if not isinstance(principal, str) or not principal.strip():
            continue
        if not isinstance(action, str) or action.lower() not in _ACTIONS:
            continue
        if not isinstance(resource_type, str) or not resource_type.strip():
            continue
        if not isinstance(allowed, bool):
            continue
        if not allowed and (not isinstance(reason, str) or not reason.strip()):
            continue
        group = by_type.get(resource_type, [])
        if not group:
            continue
        for ko in sorted(group, key=lambda k: k["koid"]):
            anchor_prop, anchor_value = _anchor_pair(ko)
            if anchor_prop is None:
                continue
            # '?' would corrupt the question; control chars corrupt the
            # record text
            if "?" in str(anchor_value) or any(
                ord(c) < 32 for c in str(anchor_value)
            ):
                continue
            ref = f"the {resource_type} whose {anchor_prop} is {anchor_value}"
            if allowed:
                verdict = f"ALLOWED: {principal} may {action} {ref}"
            else:
                verdict = (
                    f"DENIED: {principal} may not {action} {ref} ({reason})"
                )
            scenarios.append(
                Scenario(
                    scenario_id=(
                        f"authorization:{resource_type}:{action}:{principal}:"
                        f"{ko['koid']}:{'allow' if allowed else 'deny'}"
                    ),
                    task_type="authorization",
                    difficulty="factual",
                    question=f"May {principal} {action} {ref}?",
                    expected_answer=verdict,
                    koids=(ko["koid"],),
                    property=anchor_prop,
                    anchor_prop=anchor_prop,
                    anchor_value=anchor_value,
                    type_name=resource_type,
                    subject=principal,
                    action=action,
                    decision=allowed,
                    reason=reason,
                )
            )
    return scenarios
