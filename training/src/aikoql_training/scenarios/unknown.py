"""Unknown scenario generator (design Phase 8): questions the KB
cannot answer — a missing entity (name) or a missing property.

`missing` entries name what the operator knows is absent: a name no KO
of the type carries, or a property an existing KO does not have. The
answer refuses machine-readably (UNKNOWN: prefix), labels come out
grounded=False/answerable=False, and the oracle proves the absence by
query — the result carries no row with the asked property. Uncertainty
never becomes a false positive: a name that actually exists, or a
property the KO actually has, is skipped at generation.
"""

from __future__ import annotations

from typing import Any, Dict, List

from aikoql_training.scenarios.answer_formats import unknown_answer
from aikoql_training.scenarios.factual import _eligible
from aikoql_training.scenarios.scenario import Scenario
from aikoql_training.scenarios.templates import _escape, ref_of


def _anchor_pair(ko: dict):
    """The anchor for a missing-property refusal: the KO's name, else
    the first sorted scalar property."""
    props = ko.get("properties", {})
    name = props.get("name")
    if isinstance(name, str) and name.strip():
        return "name", name
    for prop in sorted(props):
        if _eligible(props[prop]):
            return prop, props[prop]
    return None, None


def unknown_scenarios(
    kos: List[Dict[str, Any]], missing: List[Dict[str, Any]]
) -> List[Scenario]:
    by_type: Dict[str, List[dict]] = {}
    by_koid = {}
    for ko in kos:
        by_type.setdefault(ko.get("type_name"), []).append(ko)
        by_koid[ko["koid"]] = ko

    scenarios = []
    key = lambda m: (  # noqa: E731
        str(m.get("type_name", "")),
        str(m.get("property", "")),
        str(m.get("name", m.get("koid", ""))),
    )
    for entry in sorted(missing, key=key):
        type_name = entry.get("type_name")
        prop = entry.get("property")
        if not isinstance(type_name, str) or not type_name.strip():
            continue
        if not isinstance(prop, str) or not prop.strip():
            continue
        name = entry.get("name")
        koid = entry.get("koid")
        if name is not None:
            if not isinstance(name, str) or not name.strip():
                continue
            # fail-closed: an existing name is never "unknown"
            if any(ko["properties"].get("name") == name
                   for ko in by_type.get(type_name, [])):
                continue
            reason = f"no {type_name} named '{_escape(name)}'"
            question = f"What is the {prop} of {type_name} '{_escape(name)}'?"
            koids = ()
            anchor_prop, anchor_value = "name", name
            scenario_id = f"unknown:{type_name}:{prop}:{name}"
        elif koid is not None:
            ko = by_koid.get(koid)
            if ko is None or ko.get("type_name") != type_name:
                continue
            if prop in ko.get("properties", {}):
                continue  # the KO has it — not unknown
            anchor_prop, anchor_value = _anchor_pair(ko)
            if anchor_prop is None:
                continue
            reason = f"{ref_of(koid, by_koid)} has no {prop}"
            question = f"What is the {prop} of {ref_of(koid, by_koid)}?"
            koids = (koid,)
            scenario_id = f"unknown:{type_name}:{prop}:{koid}"
        else:
            continue
        scenarios.append(
            Scenario(
                scenario_id=scenario_id,
                task_type="unknown",
                difficulty="factual",
                question=question,
                expected_answer=unknown_answer(reason),
                koids=koids,
                property=prop,
                anchor_prop=anchor_prop,
                anchor_value=anchor_value,
                type_name=type_name,
            )
        )
    return scenarios
