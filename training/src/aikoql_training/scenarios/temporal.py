"""Temporal scenario generator (design Phase 6): version questions over
REAL version intervals.

Every committed version with a real commit_ts earns a question per
scalar property: "What was the X of A in <Month Year>?" answered by
THAT version's value. The month label comes from the version's own
commit_ts (UTC) and as_of IS the commit_ts — nothing is invented. The
first version carries every scalar property; later versions carry only
properties whose value CHANGED (an unchanged property repeats the
earlier answer). A month-label collision — a later version in the same
month would ask the same question again — is skipped, never emitted
(same question with a different answer is T-09's contradiction
territory; the first emission wins).
"""

from __future__ import annotations

import datetime
from typing import Any, Dict, List

from aikoql_training.scenarios.factual import _eligible
from aikoql_training.scenarios.scenario import Scenario
from aikoql_training.scenarios.templates import ref_of


def _month(commit_ts: int) -> str:
    """'March 2026' for the commit instant — the label comes from the
    version's real commit_ts, never from the question's design."""
    return datetime.datetime.fromtimestamp(
        commit_ts / 1000, tz=datetime.timezone.utc
    ).strftime("%B %Y")


def temporal_scenarios(histories: List[Dict[str, Any]]) -> List[Scenario]:
    """histories: scan rows enriched with `versions` —
    [{version, commit_ts, properties}], the trace()/AS_OF shape the
    T-08 live cell builds over the wire."""
    scenarios: List[Scenario] = []
    for record in sorted(histories, key=lambda h: h["koid"]):
        koid = record["koid"]
        type_name = record["type_name"]
        versions = [
            v for v in record.get("versions", [])
            if isinstance(v.get("properties"), dict)
            and isinstance(v.get("commit_ts"), int)
            and isinstance(v.get("version"), int)
        ]
        if not versions:
            continue
        versions.sort(key=lambda v: (v["commit_ts"], v["version"]))
        emitted = set()  # (prop, month label): first emission wins
        for i, v in enumerate(versions):
            prev = versions[i - 1]["properties"] if i else {}
            for prop in sorted(v["properties"]):
                value = v["properties"][prop]
                if not _eligible(value):
                    continue
                if i and prev.get(prop) == value:
                    continue  # unchanged: repeats the earlier answer
                label = _month(v["commit_ts"])
                if (prop, label) in emitted:
                    continue  # the question for this month already exists
                emitted.add((prop, label))
                ref = ref_of(koid, {koid: record})
                scenarios.append(Scenario(
                    scenario_id=(
                        f"temporal:{type_name}:{prop}:{koid}:v{v['version']}"
                    ),
                    task_type="temporal",
                    difficulty="factual",
                    question=f"What was the {prop} of {ref} in {label}?",
                    expected_answer=str(value),
                    koids=(koid,),
                    property=prop,
                    as_of=v["commit_ts"],
                ))
    return scenarios
