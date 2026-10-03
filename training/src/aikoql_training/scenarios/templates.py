"""The question-template engine (FZ-T4, design §11).

Entity names are user content: refs escape quotes and backslashes and
strip control characters, so a rendered reference is always one quoted
token and nothing raw reaches the question. Relation phrasing lives
here too (third-person vs base verb forms), so factual, one-hop and
multi-hop questions all conjugate the same way.
"""

from __future__ import annotations


def _escape(name: str) -> str:
    cleaned = "".join(ch for ch in name if ord(ch) >= 32 and ch != "\x7f")
    return cleaned.replace("\\", "\\\\").replace("'", "\\'")


def ref_of(koid: str, ko_by_koid: dict) -> str:
    """Human reference for a KO: its name property if set, else type +
    koid prefix. Deterministic — user content is escaped (FZ-T4)."""
    ko = ko_by_koid[koid]
    name = ko["properties"].get("name")
    if isinstance(name, str) and name.strip():
        return f"{ko['type_name']} '{_escape(name)}'"
    return f"{ko['type_name']} {koid[:8]}"


# Third-person ("Who OWNS B?") and base ("What does A own?") verb forms
# for the POC's relation types. Fallback is the raw lowercased type —
# unmapped relations may read awkwardly but stay grounded.
_REL_VERBS = {
    "OWNS": ("owns", "own"),
    "DEPENDS_ON": ("depends on", "depend on"),
    "MENTIONS": ("mentions", "mention"),
    "REPORTS_TO": ("reports to", "report to"),
    "CONTAINS": ("contains", "contain"),
    "USES": ("uses", "use"),
}


def _verbs(rel_type: str):
    third, base = _REL_VERBS.get(rel_type, (None, None))
    if third is None:
        return rel_type.lower(), rel_type.lower()
    return third, base
