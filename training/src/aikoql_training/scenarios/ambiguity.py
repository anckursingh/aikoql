"""Ambiguity scenario generator (design Phase 8): ambiguous pairs.

Two same-type KOs sharing a scalar property value cannot be told apart
by that value; a question anchored on it must enumerate BOTH candidates
machine-readably (AMBIGUOUS prefix, one `koid -> value` entry per
candidate sorted by koid, labels.ambiguous=True). The asked property is
the first sorted scalar property present in every candidate with
distinct values. A pair whose common properties all agree is not
ambiguous and is skipped, as is a candidate value containing an
enumeration delimiter ("; " or " -> ") — the parse must stay
unambiguous.
"""

from __future__ import annotations

from typing import Dict, List, Tuple

from aikoql_training.scenarios.answer_formats import ambiguity_answer
from aikoql_training.scenarios.factual import _eligible
from aikoql_training.scenarios.scenario import Scenario


def ambiguity_scenarios(kos: List[dict]) -> List[Scenario]:
    by_type: Dict[str, List[dict]] = {}
    for ko in kos:
        by_type.setdefault(ko.get("type_name"), []).append(ko)

    scenarios = []
    for type_name in sorted(by_type):
        group = by_type[type_name]
        # (prop, value) -> the KOs sharing that scalar value
        anchors: Dict[Tuple[str, object], List[dict]] = {}
        for ko in group:
            for prop in sorted(ko.get("properties", {})):
                value = ko["properties"][prop]
                if not _eligible(value):
                    continue
                anchors.setdefault((prop, value), []).append(ko)
        for anchor_prop, anchor_value in sorted(
            anchors, key=lambda pv: (pv[0], str(pv[1]))
        ):
            members = anchors[(anchor_prop, anchor_value)]
            if len(members) < 2:
                continue
            members.sort(key=lambda k: k["koid"])
            # the asked property: in every candidate, distinct values,
            # not the anchor itself
            common = set(members[0].get("properties", {}))
            for m in members[1:]:
                common &= set(m.get("properties", {}))
            asked = None
            for prop in sorted(common):
                if prop == anchor_prop:
                    continue
                values = [m["properties"][prop] for m in members]
                if any(not _eligible(v) for v in values):
                    continue
                if len({str(v) for v in values}) < 2:
                    continue
                asked = prop
                break
            if asked is None:
                continue
            candidates = tuple(
                (m["koid"], str(m["properties"][asked])) for m in members
            )
            if any("; " in v or " -> " in v for _, v in candidates):
                continue
            scenarios.append(
                Scenario(
                    scenario_id=(
                        f"ambiguity:{type_name}:{anchor_prop}:{asked}:"
                        f"{'+'.join(m['koid'] for m in members)}"
                    ),
                    task_type="ambiguity",
                    difficulty="factual",
                    question=(
                        f"What is the {asked} of the {type_name} whose "
                        f"{anchor_prop} is {anchor_value}?"
                    ),
                    expected_answer=ambiguity_answer(candidates),
                    koids=tuple(m["koid"] for m in members),
                    property=asked,
                    anchor_prop=anchor_prop,
                    anchor_value=anchor_value,
                    candidates=candidates,
                )
            )
    return scenarios
