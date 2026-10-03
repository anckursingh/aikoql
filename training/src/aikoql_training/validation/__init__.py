"""Validation — T-05: the oracle (design Phase 10); T-07: the grounding
validator (design Phase 12). T-14: eval_set.py holds the E1-E9 dataset
checks — imported from the module directly (a __init__ re-export would
cycle: gates -> __init__ -> eval_set -> grounding mid-execution)."""

from aikoql_training.validation.execution import verify_scenario
from aikoql_training.validation.grounding import validate_grounding

__all__ = ["verify_scenario", "validate_grounding"]
