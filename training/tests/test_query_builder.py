"""T-05 RED: query builder + oracle (design Phase 10).

The builder emits TEXT aikoql against the compiler's real grammar and
the oracle proves every generated query passes compile -> plan ->
execute -> scenario-match (the design's 6-point acceptance; auth and
evidence land in T-08/T-10). Every expected-query test below fails
against the current tree: `aikoql_training.generators` and
`aikoql_training.validation` do not exist.

Grammar pins (recon against crates/compiler/src/parser + crates/runtime
at the T-05 pre-fix head):

- string literals are double-quoted ONLY and have NO escape mechanism
  (lexer.rs read_string) — a value containing '"' is unrepresentable
- MATCH predicates address properties, never the KOID; TRAVERSE is
  outbound-only with one rel_type and optional DEPTH (default 1)
- a traverse query MUST project a field: RETURN * after TRAVERSE yields
  RowSet::Traversal, which tool_aikoql renders as {"results": []}
- integral literals lower to Value::Int (kq010) and cross-type
  comparison is fail-closed, so ints render as integers and floats as
  decimals; negative numbers and scientific notation do not lex
"""

from __future__ import annotations

import pytest
from hypothesis import given
from hypothesis import strategies as st

from aikoql import Agent

from aikoql_training.client import scan_edges
from aikoql_training.generators import build_queries
from aikoql_training.scenarios import (
    Scenario,
    factual_scenarios,
    multi_hop_scenarios,
    relation_scenarios,
)
from aikoql_training.validation import verify_scenario


def _ko(koid, type_name, props):
    return {"koid": koid, "type_name": type_name, "properties": props}


def _scen(difficulty, koids, path=(), prop=None):
    return Scenario(
        scenario_id="t",
        task_type="grounded_qa",
        difficulty=difficulty,
        question="q?",
        expected_answer="ans",
        koids=koids,
        expected_path=path,
        property=prop,
    )


# -- expected query strings ----------------------------------------------

def test_factual_string_query():
    ko = _ko("a", "service", {"name": "settlement"})
    s = _scen("factual", ("a",), prop="name")
    assert build_queries(s, [ko]) == [
        'MATCH service WHERE name == "settlement" RETURN name'
    ]


def test_factual_int_renders_integral_literal():
    ko = _ko("a", "service", {"port": 8443})
    s = _scen("factual", ("a",), prop="port")
    assert build_queries(s, [ko]) == [
        "MATCH service WHERE port == 8443 RETURN port"
    ]


def test_factual_float_renders_decimal_literal():
    ko = _ko("a", "service", {"uptime": 99.9})
    s = _scen("factual", ("a",), prop="uptime")
    assert build_queries(s, [ko]) == [
        "MATCH service WHERE uptime == 99.9 RETURN uptime"
    ]


def test_factual_bool_renders_keyword_literal():
    ko = _ko("a", "service", {"enabled": True})
    s = _scen("factual", ("a",), prop="enabled")
    assert build_queries(s, [ko]) == [
        "MATCH service WHERE enabled == true RETURN enabled"
    ]


def test_one_hop_query_traverses_depth_1():
    a = _ko("a", "service", {"name": "settlement"})
    b = _ko("b", "service", {"name": "checkout", "port": 9000})
    s = _scen("one_hop", ("a", "b"), path=(("a", "DEPENDS_ON", "b"),))
    assert build_queries(s, [a, b]) == [
        'MATCH service WHERE name == "settlement" TRAVERSE DEPENDS_ON DEPTH 1 RETURN name'
    ]


def test_multi_hop_same_rel_traverses_depth_2():
    a = _ko("a", "service", {"name": "settlement"})
    b = _ko("b", "service", {"name": "checkout"})
    c = _ko("c", "service", {"name": "gateway"})
    s = _scen(
        "multi_hop",
        ("a", "b", "c"),
        path=(("a", "DEPENDS_ON", "b"), ("b", "DEPENDS_ON", "c")),
    )
    assert build_queries(s, [a, b, c]) == [
        'MATCH service WHERE name == "settlement" TRAVERSE DEPENDS_ON DEPTH 2 RETURN name'
    ]


def test_multi_hop_mixed_rel_becomes_two_queries():
    # The text grammar fixes one rel_type per TRAVERSE — a mixed-rel path
    # composes as two chained queries, the second anchored on B.
    a = _ko("a", "service", {"name": "settlement"})
    b = _ko("b", "service", {"name": "checkout"})
    c = _ko("c", "service", {"name": "gateway"})
    s = _scen(
        "multi_hop",
        ("a", "b", "c"),
        path=(("a", "DEPENDS_ON", "b"), ("b", "USES", "c")),
    )
    assert build_queries(s, [a, b, c]) == [
        'MATCH service WHERE name == "settlement" TRAVERSE DEPENDS_ON DEPTH 1 RETURN name',
        'MATCH service WHERE name == "checkout" TRAVERSE USES DEPTH 1 RETURN name',
    ]


# -- fail-closed: unrepresentable input never emits a bad query ----------

def test_value_containing_double_quote_is_unrepresentable():
    ko = _ko("a", "service", {"name": 'o"brien'})
    s = _scen("factual", ("a",), prop="name")
    assert build_queries(s, [ko]) == []


def test_blank_string_value_is_unrepresentable():
    ko = _ko("a", "service", {"name": "   "})
    s = _scen("factual", ("a",), prop="name")
    assert build_queries(s, [ko]) == []


def test_ko_without_scalar_anchor_is_unrepresentable():
    a = _ko("a", "service", {"name": "settlement"})
    b = _ko("b", "service", {})
    s = _scen("one_hop", ("a", "b"), path=(("a", "DEPENDS_ON", "b"),))
    assert build_queries(s, [a, b]) == []


def test_negative_and_scientific_values_do_not_lex():
    for value in (-5, 1e20, float("inf")):
        ko = _ko("a", "service", {"p": value})
        s = _scen("factual", ("a",), prop="p")
        assert build_queries(s, [ko]) == [], f"{value!r} must be skipped"


def test_non_ident_or_keyword_names_are_unrepresentable():
    ko = _ko("a", "service", {"a b": "x"})
    s = _scen("factual", ("a",), prop="a b")
    assert build_queries(s, [ko]) == []  # property name is not an ident

    ko = _ko("a", "service", {"MATCH": "x"})
    s = _scen("factual", ("a",), prop="MATCH")
    assert build_queries(s, [ko]) == []  # keyword property name

    a = _ko("a", "service", {"name": "settlement"})
    b = _ko("b", "service", {"name": "checkout"})
    s = _scen("one_hop", ("a", "b"), path=(("a", "not-a-rel", "b"),))
    assert build_queries(s, [a, b]) == []  # rel type is not an ident


def test_build_queries_is_deterministic():
    a = _ko("a", "service", {"name": "settlement", "port": 8443})
    b = _ko("b", "service", {"name": "checkout"})
    s = _scen("one_hop", ("a", "b"), path=(("a", "DEPENDS_ON", "b"),))
    assert build_queries(s, [a, b]) == build_queries(s, [a, b])


@given(st.text(max_size=40))
def test_factual_value_round_trips_through_quotes(s):
    ko = _ko("a", "T", {"p": s})
    scen = _scen("factual", ("a",), prop="p")
    queries = build_queries(scen, [ko])
    if '"' in s or not s.strip():
        assert queries == []
    else:
        assert queries == [f'MATCH T WHERE p == "{s}" RETURN p']


# -- the oracle ----------------------------------------------------------

class _FakeDb:
    def __init__(self, envs):
        self._envs = list(envs)
        self.queries = []

    def aikoql(self, q):
        self.queries.append(q)
        return self._envs.pop(0)


def test_oracle_passes_when_factual_anchor_value_matches():
    ko = _ko("a", "service", {"name": "settlement"})
    s = _scen("factual", ("a",), prop="name")
    s = Scenario(**{**s.__dict__, "expected_answer": "settlement"})
    db = _FakeDb([{"results": [{"koid": "a", "properties": {"name": "settlement"}}]}])
    report = verify_scenario(db, s, ["q"], [ko])
    assert report["ok"] is True
    assert report["errors"] == []


def test_oracle_fails_when_anchor_ko_missing():
    ko = _ko("a", "service", {"name": "settlement"})
    s = _scen("factual", ("a",), prop="name")
    db = _FakeDb([{"results": []}])
    report = verify_scenario(db, s, ["q"], [ko])
    assert report["ok"] is False


def test_oracle_fails_when_hop_target_missing():
    a = _ko("a", "service", {"name": "settlement"})
    b = _ko("b", "service", {"name": "checkout"})
    s = _scen("one_hop", ("a", "b"), path=(("a", "DEPENDS_ON", "b"),))
    db = _FakeDb([{"results": [{"koid": "a"}]}])  # b never came back
    report = verify_scenario(db, s, ["q"], [a, b])
    assert report["ok"] is False


def test_oracle_passes_multi_hop_when_every_hop_recovers():
    a = _ko("a", "service", {"name": "settlement"})
    b = _ko("b", "service", {"name": "checkout"})
    c = _ko("c", "service", {"name": "gateway"})
    s = _scen(
        "multi_hop",
        ("a", "b", "c"),
        path=(("a", "DEPENDS_ON", "b"), ("b", "USES", "c")),
    )
    db = _FakeDb(
        [
            {"results": [{"koid": "b"}]},
            {"results": [{"koid": "c"}]},
        ]
    )
    report = verify_scenario(db, s, ["q1", "q2"], [a, b, c])
    assert report["ok"] is True
    assert db.queries == ["q1", "q2"]


def test_oracle_records_compile_failures():
    class _Boom:
        def aikoql(self, q):
            raise RuntimeError("AIKOQL1010 parse failure")

    ko = _ko("a", "service", {"name": "settlement"})
    s = _scen("factual", ("a",), prop="name")
    report = verify_scenario(_Boom(), s, ["MATCH ???"], [ko])
    assert report["ok"] is False
    assert any("parse failure" in e for e in report["errors"])


# -- live acceptance: every generated query compiles and matches ---------

def test_live_every_generated_query_passes_compile_to_scenario_match(mcp_server):
    """The 6-point acceptance minus auth/evidence (T-08/T-10): every
    generated query compiles, plans, executes and matches its scenario,
    with a 100% compile rate over the spawned MCP server."""
    host, token = mcp_server
    with Agent.connect(host, token=token) as db:
        a = db.remember("service", {"name": "settlement", "port": 8443})["koid"]
        b = db.remember("service", {"name": "checkout", "port": 9000})["koid"]
        c = db.remember("service", {"name": "gateway", "port": 443})["koid"]
        db.relate(a, b, "DEPENDS_ON")
        db.relate(b, c, "USES")
        kos = [db.get(a), db.get(b), db.get(c)]
        edges = scan_edges(db, [a, b, c])

        scenarios = (
            factual_scenarios(kos)
            + relation_scenarios(edges, kos, "DEPENDS_ON")
            + relation_scenarios(edges, kos, "USES")
            + multi_hop_scenarios(edges, kos)
        )
        assert scenarios

        compiled = 0
        for s in scenarios:
            queries = build_queries(s, kos)
            assert queries, f"unexpressible scenario: {s.scenario_id}"
            for q in queries:
                env = db.aikoql(q)  # compile -> plan -> execute over the wire
                assert isinstance(env.get("results"), list), q
                compiled += 1
            report = verify_scenario(db, s, queries, kos)
            assert report["ok"], (s.scenario_id, report["errors"])

        # 6 factual (3 KOs x 2 props) + 2+2 relation + 2 multi-hop queries
        assert compiled == 12
