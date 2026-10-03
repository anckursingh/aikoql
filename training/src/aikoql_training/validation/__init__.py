"""Validation — T-05: the oracle (design Phase 10); T-07: the grounding
validator (design Phase 12)."""

from aikoql_training.validation.execution import verify_scenario
from aikoql_training.validation.grounding import validate_grounding

__all__ = ["verify_scenario", "validate_grounding"]
