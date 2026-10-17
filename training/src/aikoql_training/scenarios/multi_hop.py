"""Multi-hop scenario generator (design Phase 5).

A two-edge path A --r1--> B --r2--> C yields one scenario carrying the
EXACT path walked (stored in expected_path, never invented); the
intermediate KO is the answer. Paths are built only from the edges
given — the real scanned graph — and every hop must land on a known
KO. Deterministic: sources, adjacency lists and hops are all sorted.
"""

from __future__ import annotations

from typing import List

from aikoql_training.scenarios.scenario import Scenario
from aikoql_training.scenarios.templates import _verbs, ref_of


def multi_hop_scenarios(edges: List[dict], kos: List[dict]) -> List[Scenario]:
    by_koid = {k["koid"]: k for k in kos}
    outgoing = {}
    for e in edges:
        outgoing.setdefault(e["from"], []).append((e["rel"], e["to"]))
    scenarios = []
    for a in sorted(outgoing):
        for r1, b in sorted(outgoing[a]):
            for r2, c in sorted(outgoing.get(b, [])):
                if a not in by_koid or b not in by_koid or c not in by_koid:
                    continue  # dangling hop: no grounded answer exists
                scenarios.append(
                    Scenario(
                        scenario_id=f"multi_hop:{r1}:{a}:{b}:{r2}:{c}",
                        task_type="grounded_qa",
                        difficulty="multi_hop",
                        question=(
                            f"What does {ref_of(a, by_koid)} {_verbs(r1)[1]} "
                            f"that {_verbs(r2)[0]} {ref_of(c, by_koid)}?"
                        ),
                        expected_answer=ref_of(b, by_koid),
                        koids=(a, b, c),
                        expected_path=((a, r1, b), (b, r2, c)),
                    )
                )
    return scenarios
