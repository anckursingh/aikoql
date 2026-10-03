"""The splitter (design Phase 13) — knowledge-level holdout assignment.

Every example is assigned to train/val/test by its `split_key` alone
(the holdout dimension the example belongs to, builder-stamped since
T-01): a seeded stable hash of the key. Consequences, by construction:

- Near-duplicate questions (template variants of the same fact) share
  a key, so they can never straddle a holdout — under ANY seed or
  input order (FZ-T7).
- Assignment is a pure function of (split_key, seed): reordering,
  re-shuffling or re-generating the example list cannot move an
  example (determinism law 3).

The splitter also reports cross-holdout violations: pairs of examples
in DIFFERENT splits that share any expected.koid leak knowledge across
the holdout boundary (recorded, not raised — the §26 leakage gate
counts them at dataset validation).
"""

from __future__ import annotations

import hashlib
from typing import Dict, List, Sequence, Tuple

from aikoql_training.errors import DatasetError

_SPLITS = ("train", "val", "test")

# Reported shape: (example_id_a, example_id_b, shared_koid)
Violation = Tuple[str, str, str]


def component_ids(edges: Sequence[dict]) -> Dict[str, str]:
    """Connected-component ids over `edges` (each with from/rel/to).

    Union-find with the root = the min koid, so a component's id is
    the component's lexicographically smallest koid — deterministic
    and independent of edge order. Every koid touched by an edge maps
    to its component root; the builder stamps that root as the
    example's split_key, so ALL examples touching a knowledge
    component (factual {s}, relation {s,c}, ...) share ONE key and
    can never straddle a holdout under any seed (the mixed-cardinality
    trap the leakage gate caught on the T-12 koid-set-join keys).
    """
    parent: Dict[str, str] = {}

    def find(x: str) -> str:
        parent.setdefault(x, x)
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    for edge in edges:
        a, b = find(edge["from"]), find(edge["to"])
        if a != b:
            parent[max(a, b)] = min(a, b)
    return {k: find(k) for k in list(parent)}


def assign_splits(
    examples: Sequence[dict],
    seed: int,
    ratios: Sequence[int] = (8, 1, 1),
) -> Tuple[Dict[str, List[dict]], List[Violation]]:
    """Assign examples to train/val/test by split_key hash buckets.

    `ratios` are integer weights for (train, val, test); a key hashes
    into train with probability ratios[0]/sum, and so on.
    """
    if len(ratios) != 3 or any(w <= 0 for w in ratios):
        raise DatasetError(
            f"ratios must be three positive weights, got {ratios!r}")
    total = sum(ratios)
    splits: Dict[str, List[dict]] = {name: [] for name in _SPLITS}
    homes: Dict[str, str] = {}
    for ex in examples:
        key = ex.get("split_key")
        if not key:
            raise DatasetError(
                f"example {ex.get('example_id', '?')} has no split_key")
        bucket = hashlib.sha256(
            f"{seed}:{key}".encode("utf-8")).digest()[0] % total
        # byte 0 is uniform: train covers [0, w0), val [w0, w0+w1), test the rest
        name = _SPLITS[0] if bucket < ratios[0] else (
            _SPLITS[1] if bucket < ratios[0] + ratios[1] else _SPLITS[2])
        splits[name].append(ex)
        homes[ex["example_id"]] = name

    violations: List[Violation] = []
    owners: Dict[str, Tuple[str, str]] = {}  # koid -> (example_id, home)
    for ex in examples:
        home = homes[ex["example_id"]]
        for koid in ex.get("expected", {}).get("koids", []):
            owner = owners.setdefault(koid, (ex["example_id"], home))
            if owner[1] != home:
                violations.append((ex["example_id"], owner[0], koid))
    return splits, violations
