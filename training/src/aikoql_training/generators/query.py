"""Query builder (design Phase 10): Scenario -> TEXT aikoql.

Emits queries against the compiler's real text grammar, pinned by the
T-05 recon (crates/compiler/src/parser + crates/runtime):

- string literals are double-quoted with NO escape mechanism — a value
  containing '"' is unrepresentable
- MATCH predicates address properties, never the KOID
- TRAVERSE is outbound-only, one rel_type per clause, DEPTH optional —
  a same-rel path compiles as one DEPTH-n query, a mixed-rel path
  chains one query per hop (each anchored on the previous hop's target)
- a traverse query must project a field: RETURN * after TRAVERSE comes
  back as {"results": []} through tool_aikoql
- integral literals lower to Value::Int (kq010) and cross-type
  comparison is fail-closed, so ints render as integers and floats as
  decimals; negative numbers and scientific notation do not lex

Unrepresentable input never emits a bad query: the scenario is skipped
([]) and the compile gate stays green.
"""

from __future__ import annotations

import re
from typing import List, Optional, Tuple

from aikoql_training.scenarios.scenario import Scenario

# The lexer promotes these to tokens — never usable as idents.
_KEYWORDS = frozenset({
    "MATCH", "WHERE", "AND", "OR", "RETURN", "SIMILAR", "TO", "SCORE",
    "BM25", "USING", "EMBEDDING", "TRAVERSE", "DEPTH", "ORDER", "BY",
    "GROUP", "LEFT", "JOIN", "ON", "ASC", "DESC", "COUNT", "SUM", "AVG",
    "MIN", "MAX", "CREATE", "UPDATE", "DELETE", "INGEST", "EXTRACT",
    "TABLES", "ENTITIES", "BUILD", "RELATIONSHIPS", "COMMIT", "EXPLAIN",
    "AS_OF", "BETWEEN", "HISTORICAL", "EPISTEMIC", "SOURCE", "LIMIT",
    "OFFSET",
})

_IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_:]*")
_DECIMAL = frozenset("0123456789.")


def _render(value) -> Optional[str]:
    """The value as a text literal, or None when it cannot be expressed."""
    if isinstance(value, bool):  # before int — bool is an int subclass
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value) if value >= 0 else None
    if isinstance(value, float):
        if value < 0 or value != value or value == float("inf"):
            return None
        s = repr(value)
        # '-' and 'e' (scientific notation) do not lex as numbers
        return s if all(c in _DECIMAL for c in s) else None
    if isinstance(value, str):
        if '"' in value or not value.strip():
            return None
        return f'"{value}"'
    return None


def _ident_ok(name: str) -> bool:
    return bool(_IDENT.fullmatch(name)) and name.upper() not in _KEYWORDS


def _anchor(ko: dict) -> Optional[Tuple[str, str]]:
    """The first sorted scalar property usable as an identifying anchor."""
    for prop in sorted(ko["properties"]):
        rendered = _render(ko["properties"][prop])
        if rendered is not None and _ident_ok(prop):
            return prop, rendered
    return None


def _match(
    type_name: str,
    prop: str,
    rendered: str,
    rel: Optional[str],
    depth: int,
    ret: str,
) -> Optional[str]:
    if not _ident_ok(type_name) or not _ident_ok(prop) or not _ident_ok(ret):
        return None
    if rel is not None and not _ident_ok(rel):
        return None
    query = f"MATCH {type_name} WHERE {prop} == {rendered}"
    if rel is not None:
        query += f" TRAVERSE {rel} DEPTH {depth}"
    return f"{query} RETURN {ret}"


def _uncertainty_query(scenario: Scenario, by_koid: dict) -> List[str]:
    """Unknown/ambiguity/contradiction: one anchored MATCH whose result
    set proves the uncertainty — no row carrying the property (unknown)
    or both sides (ambiguity/contradiction)."""
    type_name = scenario.type_name
    if scenario.koids and scenario.koids[0] in by_koid:
        type_name = by_koid[scenario.koids[0]]["type_name"]
    if (
        type_name is None
        or scenario.property is None
        or scenario.anchor_prop is None
        or scenario.anchor_value is None
        or not _ident_ok(scenario.property)
    ):
        return []
    rendered = _render(scenario.anchor_value)
    if rendered is None:
        return []
    q = _match(
        type_name, scenario.anchor_prop, rendered, None, 0, scenario.property
    )
    return [q] if q is not None else []


def build_queries(scenario: Scenario, kos: List[dict]) -> List[str]:
    by_koid = {k["koid"]: k for k in kos}
    if scenario.task_type in ("unknown", "ambiguity", "contradiction"):
        # unknown entities carry no KO — the branch precedes the
        # koids-present check
        return _uncertainty_query(scenario, by_koid)
    if not scenario.koids or any(k not in by_koid for k in scenario.koids):
        return []  # dangling grounding: no honest query exists
    start = by_koid[scenario.koids[0]]
    type_name = start["type_name"]

    if scenario.task_type == "temporal":
        if (
            scenario.as_of is None
            or scenario.as_of < 0
            or scenario.property is None
            or not _ident_ok(type_name)
            or not _ident_ok(scenario.property)
        ):
            return []
        return [
            f"MATCH {type_name} AS_OF {scenario.as_of}"
            f" RETURN {scenario.property}"
        ]

    if scenario.difficulty == "factual":
        if scenario.property is None:
            return []
        rendered = _render(start["properties"].get(scenario.property))
        if (
            rendered is None
            or not _ident_ok(type_name)
            or not _ident_ok(scenario.property)
        ):
            return []
        return [
            f"MATCH {type_name} WHERE {scenario.property} == {rendered}"
            f" RETURN {scenario.property}"
        ]

    hops = list(scenario.expected_path)
    if not hops:
        return []  # no path: nothing to verify against the graph
    first = _anchor(start)
    if first is None:
        return []

    if len({rel for _, rel, _ in hops}) == 1:
        # Same-rel path: one set-based TRAVERSE with DEPTH n.
        tail_anchor = _anchor(by_koid[hops[-1][2]])
        if tail_anchor is None:
            return []
        q = _match(type_name, first[0], first[1], hops[0][1], len(hops), tail_anchor[0])
        return [q] if q is not None else []

    # Mixed-rel path: one DEPTH-1 query per hop, each anchored on the
    # previous hop's target (the grammar fixes one rel_type per clause).
    queries = []
    current = start
    for _, rel, target in hops:
        anc = _anchor(current)
        tail_anchor = _anchor(by_koid[target])
        if anc is None or tail_anchor is None:
            return []
        q = _match(current["type_name"], anc[0], anc[1], rel, 1, tail_anchor[0])
        if q is None:
            return []
        queries.append(q)
        current = by_koid[target]
    return queries
