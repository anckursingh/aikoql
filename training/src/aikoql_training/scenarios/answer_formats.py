"""Machine-readable answer formats for uncertainty scenarios (design
Phase 8, T-09).

Unknown answers refuse with the UNKNOWN: prefix; ambiguous answers
enumerate every candidate as `koid -> value` entries sorted by koid;
contradicted answers enumerate both sides AND preserve the Conflict
metadata — the conflict koid and its resolution state verbatim, the
generator never picks a side. The value parsers are shared with
validation/grounding.py so the validator reads exactly what the
generators write. No local imports: used by scenarios, generators and
validation alike.
"""

from __future__ import annotations

import re
from typing import List, Tuple

UNKNOWN_PREFIX = "UNKNOWN:"
AMBIGUOUS_PREFIX = "AMBIGUOUS ("
CONTRADICTED_PREFIX = "CONTRADICTED:"

# Fail-closed shape: the whole answer matches or it parses to nothing.
_CONTRADICTED = re.compile(
    r"^CONTRADICTED: (.*?) \(claim [0-9a-f]{8}\) vs (.*?) \(claim "
    r"[0-9a-f]{8}\); conflict [0-9a-f]{8}, resolution .*$"
)


def unknown_answer(reason: str) -> str:
    return f"{UNKNOWN_PREFIX} {reason}"


def ambiguity_answer(candidates: Tuple[Tuple[str, str], ...]) -> str:
    entries = "; ".join(f"{koid[:8]} -> {value}" for koid, value in candidates)
    return f"AMBIGUOUS ({len(candidates)} candidates): {entries}"


def ambiguity_values(answer: str) -> List[str]:
    """The candidate values of an AMBIGUOUS answer, or [] when the
    answer is not in the machine-readable shape."""
    prefix = " candidates): "
    if not answer.startswith(AMBIGUOUS_PREFIX):
        return []
    marker = answer.find(prefix)
    if marker < 0:
        return []
    body = answer[marker + len(prefix):]
    return [entry.rsplit(" -> ", 1)[1] for entry in body.split("; ")]


def contradiction_answer(
    candidates: Tuple[Tuple[str, str], ...], conflict_koid: str, resolution: str
) -> str:
    (a_koid, a_value), (b_koid, b_value) = candidates
    return (
        f"CONTRADICTED: {a_value} (claim {a_koid[:8]}) vs "
        f"{b_value} (claim {b_koid[:8]}); conflict {conflict_koid[:8]}, "
        f"resolution {resolution}"
    )


def contradiction_values(answer: str) -> List[str]:
    match = _CONTRADICTED.match(answer)
    return [match.group(1), match.group(2)] if match else []
