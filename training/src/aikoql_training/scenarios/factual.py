"""Factual scenario generator (design Phase 3, §11.1).

KO A with scalar property X -> "What is the X of A?" -> expected X.
One scenario per scalar property, deterministic ordering (koid, then
property name — no RNG needed for this slice). Nested containers,
empty strings and None values are skipped: they have no factual
one-line answer.
"""

from __future__ import annotations

from typing import List

from aikoql_training.scenarios.scenario import Scenario, ref_of

_SCALAR = (str, int, float)  # bool passes via int


def _eligible(value) -> bool:
    return isinstance(value, _SCALAR) and not (
        isinstance(value, str) and not value.strip()
    )


def factual_scenarios(kos: List[dict]) -> List[Scenario]:
    scenarios = []
    for ko in sorted(kos, key=lambda k: k["koid"]):
        for prop in sorted(ko["properties"]):
            value = ko["properties"][prop]
            if not _eligible(value):
                continue
            scenarios.append(
                Scenario(
                    scenario_id=f"factual:{ko['type_name']}:{prop}:{ko['koid']}",
                    task_type="grounded_qa",
                    difficulty="factual",
                    question=f"What is the {prop} of {ref_of(ko['koid'], {ko['koid']: ko})}?",
                    expected_answer=str(value),
                    koids=(ko["koid"],),
                    property=prop,
                )
            )
    return scenarios
