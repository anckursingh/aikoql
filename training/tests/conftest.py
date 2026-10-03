"""Shared fixtures for aikoql-training tests.

make_example builds a valid canonical example (design §9 + the T-01
recon corrections) with its content-derived example_id filled in.
"""

from aikoql_training.models import SCHEMA_VERSION, compute_id

_BASE = {
    "schema_version": SCHEMA_VERSION,
    "generator_version": "0.1.0",
    "source": {
        "database_id": "acmepay",
        "snapshot_id": "snap-1",
        "knowledge_revision": "journal_seq=42;audit_hash=" + "0" * 64,
        "scenario_id": "factual:policy:p-01",
        "created_at": "2026-10-03T00:00:00Z",
    },
    "task": {"type": "grounded_qa", "difficulty": "factual", "requires": []},
    "input": {"question": "What is the owner of the settlement service?"},
    "semantic_target": {"operation": "query"},
    "query_target": {
        "language": "aikoql",
        "query": "MATCH Service WHERE name == 'settlement' RETURN owner",
    },
    "context": {"entities": [], "facts": [], "relations": [], "evidence": []},
    "expected": {"answer": "Payments Team", "koids": [], "evidence_ids": []},
    "policy": {"authorization_required": False},
    "labels": {
        "grounded": True,
        "answerable": True,
        "ambiguous": False,
        "contradictory": False,
    },
    "split_key": "policy:p-01",
}


def make_example(**overrides):
    """A valid example with overrides applied and a matching example_id."""
    example = {k: (dict(v) if isinstance(v, dict) else list(v) if isinstance(v, list) else v)
               for k, v in _BASE.items()}
    example.update(overrides)
    example["example_id"] = compute_id(example)
    return example
