"""Contradiction scenario generator (design Phase 8): conflicting
facts preserve Conflict metadata.

`conflicts` entries carry the kernel's real Conflict record — the
conflict KO (koid, resolution, properties.claim_a/claim_b) plus the two
claim KOs. One example per conflict: the question anchors on the
claims' shared scalar property, the answer enumerates both values with
their claim koids AND preserves the conflict koid + resolution state
verbatim (CONTRADICTED prefix, labels.contradictory=True). The claims
are ordered by the conflict record, never by the input list. Input
whose claims do not match the record, claims with no differing
property, claims of different types, or claims with no shared anchor
are skipped — nothing is fabricated and no side is ever picked.
"""

from __future__ import annotations

from typing import Any, Dict, List, Tuple

from aikoql_training.scenarios.answer_formats import contradiction_answer
from aikoql_training.scenarios.factual import _eligible
from aikoql_training.scenarios.scenario import Scenario


def contradiction_scenarios(conflicts: List[Dict[str, Any]]) -> List[Scenario]:
    scenarios = []
    key = lambda e: str((e.get("conflict") or {}).get("koid", ""))  # noqa: E731
    for entry in sorted(conflicts, key=key):
        conflict = entry.get("conflict")
        claims = entry.get("claims")
        if not isinstance(conflict, dict) or not isinstance(claims, list):
            continue
        if len(claims) < 2:
            continue
        by_koid = {
            c["koid"]: c
            for c in claims
            if isinstance(c, dict) and c.get("koid")
        }
        # the conflict record orders the sides; the input order is
        # irrelevant (deterministic)
        a = by_koid.get(conflict.get("properties", {}).get("claim_a"))
        b = by_koid.get(conflict.get("properties", {}).get("claim_b"))
        if a is None or b is None:
            continue
        if a.get("type_name") != b.get("type_name"):
            continue
        props_a = a.get("properties", {})
        props_b = b.get("properties", {})
        # the differing property: first sorted key with distinct values
        diff = None
        for prop in sorted(set(props_a) & set(props_b)):
            va, vb = props_a[prop], props_b[prop]
            if _eligible(va) and _eligible(vb) and va != vb:
                diff = prop
                break
        if diff is None:
            continue
        # the shared anchor: first sorted scalar prop with equal values
        anchor = None
        for prop in sorted(set(props_a) & set(props_b)):
            va, vb = props_a[prop], props_b[prop]
            if _eligible(va) and _eligible(vb) and va == vb:
                anchor = (prop, va)
                break
        if anchor is None:
            continue
        candidates = (
            (a["koid"], str(props_a[diff])),
            (b["koid"], str(props_b[diff])),
        )
        if any(" (claim " in v or "; " in v for _, v in candidates):
            continue
        conflict_koid = str(conflict.get("koid", ""))
        # the kernel nests the resolution under extensions on the live
        # Conflict KO; operator-shaped records carry it top-level
        resolution = str(
            conflict.get("resolution")
            or (conflict.get("extensions") or {}).get("resolution")
            or "unresolved"
        )
        scenarios.append(
            Scenario(
                scenario_id=f"contradiction:{diff}:{conflict_koid}",
                task_type="contradiction",
                difficulty="factual",
                question=(
                    f"What is the {diff} of the {a['type_name']} whose "
                    f"{anchor[0]} is {anchor[1]}?"
                ),
                expected_answer=contradiction_answer(
                    candidates, conflict_koid, resolution
                ),
                koids=(a["koid"], b["koid"], conflict_koid),
                property=diff,
                anchor_prop=anchor[0],
                anchor_value=anchor[1],
                candidates=candidates,
            )
        )
    return scenarios
