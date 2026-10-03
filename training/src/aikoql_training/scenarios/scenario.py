"""The Scenario record — the deterministic seed of a training example.

A scenario is what a generator produces from ACTUAL knowledge: a
question, the machine-expected answer, the KOs the answer is grounded
in, and the exact path/edge used (design §11: "the generator must
store the exact path used"). The example builder (T-05+) consumes
these records; the generators never invent facts.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Optional, Tuple


@dataclass(frozen=True)
class Scenario:
    scenario_id: str
    task_type: str
    difficulty: str
    question: str
    expected_answer: str
    koids: Tuple[str, ...]
    expected_path: Tuple[Tuple[str, str, str], ...] = ()
    property: Optional[str] = None  # the source property (factual scenarios)


def ref_of(koid: str, ko_by_koid: dict) -> str:
    """Human reference for a KO: its name property if set, else type +
    koid prefix. Deterministic — never a random label."""
    ko = ko_by_koid[koid]
    name = ko["properties"].get("name")
    if isinstance(name, str) and name.strip():
        return f"{ko['type_name']} '{name}'"
    return f"{ko['type_name']} {koid[:8]}"
