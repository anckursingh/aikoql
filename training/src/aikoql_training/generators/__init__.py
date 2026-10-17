"""Deterministic generators — T-05: the query builder (design Phase 10);
T-07: the grounded answer builder (design Phase 12)."""

from aikoql_training.generators.answer import build_answer
from aikoql_training.generators.query import build_queries

__all__ = ["build_queries", "build_answer"]
