"""Deterministic scenario generators over actual AIKOQL knowledge.

T-03: factual (Phase 3) + relation (Phase 4). T-04: multi-hop (Phase
5) + the FZ-T4 template engine. T-08: temporal (Phase 6) + provenance
(Phase 7). Later milestones add contradiction, ambiguity,
authorization, unknown.
"""

from aikoql_training.scenarios.factual import factual_scenarios
from aikoql_training.scenarios.multi_hop import multi_hop_scenarios
from aikoql_training.scenarios.provenance import provenance_scenarios
from aikoql_training.scenarios.relation import relation_scenarios
from aikoql_training.scenarios.scenario import Scenario
from aikoql_training.scenarios.temporal import temporal_scenarios

__all__ = [
    "Scenario",
    "factual_scenarios",
    "relation_scenarios",
    "multi_hop_scenarios",
    "temporal_scenarios",
    "provenance_scenarios",
]
