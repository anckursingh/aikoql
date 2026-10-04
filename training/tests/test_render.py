"""Plan -> renderer seam (PR #9 review, T-18): the query text is a
function of the derived plan, never a second walk of the scenario.

plan_of derives the semantic plan (T-17); render_queries turns that
plan into TEXT aikoql. The seam pins here:

- through-the-seam rendering reproduces the pinned query strings for
  every family (factual, one-hop, multi-hop same/mixed rel, temporal,
  the anchor-probe families),
- the renderer follows the PLAN's steps even when a hand-built plan
  disagrees with the scenario — the plan is the single source of truth
  for path, property and as_of,
- a path-family plan without steps never renders a query (fail-closed).

The anchored families (unknown/ambiguity/contradiction/authorization)
have no knowledge path: their plan is legitimately empty of path
steps, and the renderer emits the anchor probe from the scenario's
anchor fields.
"""

from __future__ import annotations

from aikoql_training.generators.query import render_queries
from aikoql_training.plan import plan_of
from aikoql_training.scenarios.scenario import Scenario

_A = "a" * 32
_B = "b" * 32
_C = "c" * 32


def _ko(koid, type_name, props):
    return {"koid": koid, "type_name": type_name, "properties": props}


def _scen(**kw):
    base = dict(
        scenario_id="s:1",
        task_type="grounded_qa",
        difficulty="factual",
        question="q?",
        expected_answer="ans",
        koids=(_A,),
    )
    base.update(kw)
    return Scenario(**base)


# -- through the seam: plan_of -> render reproduces the pinned queries -----

def test_render_factual_through_the_seam():
    ko = _ko(_A, "service", {"name": "settlement"})
    s = _scen(property="name")
    assert render_queries(plan_of(s)[3], s, [ko]) == [
        'MATCH service WHERE name == "settlement" RETURN name'
    ]


def test_render_one_hop_through_the_seam():
    a = _ko(_A, "service", {"name": "settlement"})
    b = _ko(_B, "service", {"name": "checkout"})
    s = _scen(difficulty="one_hop", koids=(_A, _B),
              expected_path=((_A, "DEPENDS_ON", _B),))
    assert render_queries(plan_of(s)[3], s, [a, b]) == [
        'MATCH service WHERE name == "settlement" TRAVERSE DEPENDS_ON DEPTH 1 RETURN name'
    ]


def test_render_multi_hop_same_rel_through_the_seam():
    a = _ko(_A, "service", {"name": "settlement"})
    b = _ko(_B, "service", {"name": "checkout"})
    c = _ko(_C, "service", {"name": "gateway"})
    s = _scen(difficulty="multi_hop", koids=(_A, _B, _C),
              expected_path=((_A, "DEPENDS_ON", _B), (_B, "DEPENDS_ON", _C)))
    assert render_queries(plan_of(s)[3], s, [a, b, c]) == [
        'MATCH service WHERE name == "settlement" TRAVERSE DEPENDS_ON DEPTH 2 RETURN name'
    ]


def test_render_multi_hop_mixed_rel_through_the_seam():
    a = _ko(_A, "service", {"name": "settlement"})
    b = _ko(_B, "service", {"name": "checkout"})
    c = _ko(_C, "service", {"name": "gateway"})
    s = _scen(difficulty="multi_hop", koids=(_A, _B, _C),
              expected_path=((_A, "DEPENDS_ON", _B), (_B, "USES", _C)))
    assert render_queries(plan_of(s)[3], s, [a, b, c]) == [
        'MATCH service WHERE name == "settlement" TRAVERSE DEPENDS_ON DEPTH 1 RETURN name',
        'MATCH service WHERE name == "checkout" TRAVERSE USES DEPTH 1 RETURN name',
    ]


def test_render_temporal_through_the_seam():
    ko = _ko(_A, "service", {"owner": "Payments Team"})
    s = _scen(task_type="temporal", property="owner", as_of=1740000000000)
    assert render_queries(plan_of(s)[3], s, [ko]) == [
        "MATCH service AS_OF 1740000000000 RETURN owner"
    ]


def test_render_unknown_probe_through_the_seam():
    s = _scen(task_type="unknown", koids=(), property="owner",
              anchor_prop="name", anchor_value="checkout",
              type_name="service", expected_answer="UNKNOWN: no such service")
    assert render_queries(plan_of(s)[3], s, []) == [
        'MATCH service WHERE name == "checkout" RETURN owner'
    ]


def test_render_ambiguity_probe_through_the_seam():
    a = _ko(_A, "service", {"name": "gateway"})
    b = _ko(_B, "service", {"name": "gateway"})
    s = _scen(task_type="ambiguity", koids=(_A, _B), property="owner",
              anchor_prop="name", anchor_value="gateway")
    assert render_queries(plan_of(s)[3], s, [a, b]) == [
        'MATCH service WHERE name == "gateway" RETURN owner'
    ]


# -- the seam: a hand-built plan wins over the scenario fields -------------

def test_renderer_follows_the_plan_not_the_scenario_path():
    a = _ko(_A, "service", {"name": "settlement"})
    b = _ko(_B, "service", {"name": "checkout"})
    s = _scen(difficulty="one_hop", koids=(_A, _B),
              expected_path=((_A, "OWNS", _B),))
    plan = {"steps": [
        {"op": "resolve_entity", "koid": _A},
        {"op": "traverse", "from": _A, "relation": "DEPENDS_ON", "to": _B},
    ]}
    assert render_queries(plan, s, [a, b]) == [
        'MATCH service WHERE name == "settlement" TRAVERSE DEPENDS_ON DEPTH 1 RETURN name'
    ]


def test_renderer_projects_the_plan_property():
    ko = _ko(_A, "service", {"name": "settlement", "owner": "Payments Team"})
    s = _scen(property="name")
    plan = {"steps": [
        {"op": "resolve_entity", "koid": _A},
        {"op": "project", "properties": ["owner"]},
    ]}
    assert render_queries(plan, s, [ko]) == [
        'MATCH service WHERE owner == "Payments Team" RETURN owner'
    ]


def test_renderer_uses_the_plan_as_of():
    ko = _ko(_A, "service", {"owner": "Payments Team"})
    s = _scen(task_type="temporal", property="name", as_of=100)
    plan = {
        "steps": [
            {"op": "resolve_entity", "koid": _A},
            {"op": "project", "properties": ["owner"]},
        ],
        "temporal": {"as_of": 200},
    }
    assert render_queries(plan, s, [ko]) == [
        "MATCH service AS_OF 200 RETURN owner"
    ]


def test_renderer_refuses_a_path_family_plan_without_steps():
    a = _ko(_A, "service", {"name": "settlement"})
    b = _ko(_B, "service", {"name": "checkout"})
    s = _scen(difficulty="one_hop", koids=(_A, _B),
              expected_path=((_A, "DEPENDS_ON", _B),))
    assert render_queries({"steps": []}, s, [a, b]) == []


def test_build_queries_is_exactly_render_of_the_plan():
    # the T-05 entry point must be render(plan_of(s)) — the example's
    # query_target and its semantic_target share one source of truth.
    from aikoql_training.generators import build_queries

    a = _ko(_A, "service", {"name": "settlement"})
    b = _ko(_B, "service", {"name": "checkout"})
    s = _scen(difficulty="one_hop", koids=(_A, _B),
              expected_path=((_A, "DEPENDS_ON", _B),))
    assert build_queries(s, [a, b]) == render_queries(plan_of(s)[3], s, [a, b])
