"""The Scenario record — the deterministic seed of a training example.

A scenario is what a generator produces from ACTUAL knowledge: a
question, the machine-expected answer, the KOs the answer is grounded
in, and the exact path/edge used (design §11: "the generator must
store the exact path used"). The example builder (T-05+) consumes
these records; the generators never invent facts. Question phrasing
and ref rendering live in templates.py (FZ-T4).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Dict, Optional, Tuple


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
    as_of: Optional[int] = None  # temporal: the real commit_ts (epoch ms)
    evidence: Tuple[Dict[str, Any], ...] = ()  # provenance: the cited evidence
