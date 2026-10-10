"""Plan derivation (design §41, PR #9 review P0.2 / TDD-01): the
Scenario -> plan mapping that makes the logical knowledge plan a
first-class training artifact.

The contract pinned here: plan_of derives intent (the task type),
entities (path-position roles: subject, target, intermediate,
candidate), requirements (the properties/relations the question
demands) and plan steps (resolve_entity -> traverse* -> project) from
the scenario's validated fields — never invented. Every koid that
appears in the plan must come from the scenario.
"""

from __future__ import annotations

from aikoql_training.plan import plan_of
from aikoql_training.scenarios.scenario import Scenario

_A = "a" * 32
_B = "b" * 32
_C = "c" * 32


def _scenario(**kw):
    base = dict(
        scenario_id="s:1",
        task_type="grounded_qa",
        difficulty="factual",
        question="What is the owner of the settlement service?",
        expected_answer="Payments Team",
        koids=(_A,),
    )
    base.update(kw)
    return Scenario(**base)


def test_factual_plan_resolves_then_projects():
    intent, entities, requirements, plan = plan_of(
        _scenario(property="owner"))
    assert intent == "grounded_qa"
    assert entities == [{"koid": _A, "role": "subject"}]
    assert requirements == ["owner"]
    assert plan == {"steps": [
        {"op": "resolve_entity", "koid": _A},
        {"op": "project", "properties": ["owner"]},
    ]}


def test_relation_plan_traverses_the_stored_edge():
    intent, entities, requirements, plan = plan_of(
        _scenario(koids=(_A, _B), expected_path=((_A, "OWNS", _B),)))
    assert entities == [
        {"koid": _A, "role": "subject"},
        {"koid": _B, "role": "target"},
    ]
    assert requirements == ["OWNS"]
    assert plan == {"steps": [
        {"op": "resolve_entity", "koid": _A},
        {"op": "traverse", "from": _A, "relation": "OWNS", "to": _B},
    ]}


def test_multi_hop_plan_walks_both_edges_in_path_order():
    path = ((_A, "OWNS", _B), (_B, "DEPENDS_ON", _C))
    intent, entities, requirements, plan = plan_of(
        _scenario(koids=(_A, _B, _C), expected_path=path))
    assert entities == [
        {"koid": _A, "role": "subject"},
        {"koid": _B, "role": "intermediate"},
        {"koid": _C, "role": "target"},
    ]
    assert requirements == ["DEPENDS_ON", "OWNS"]
    assert plan == {"steps": [
        {"op": "resolve_entity", "koid": _A},
        {"op": "traverse", "from": _A, "relation": "OWNS", "to": _B},
        {"op": "traverse", "from": _B, "relation": "DEPENDS_ON", "to": _C},
    ]}


def test_temporal_plan_carries_the_as_of_constraint():
    intent, entities, requirements, plan = plan_of(
        _scenario(task_type="temporal", property="owner", as_of=1740000000000))
    assert plan == {
        "steps": [
            {"op": "resolve_entity", "koid": _A},
            {"op": "project", "properties": ["owner"]},
        ],
        "temporal": {"as_of": 1740000000000},
    }


def test_unknown_scenario_has_an_empty_plan():
    intent, entities, requirements, plan = plan_of(
        _scenario(task_type="unknown", koids=(), expected_answer="UNKNOWN: no such service",
                  anchor_prop="name", anchor_value="NoSuch"))
    assert entities == []
    assert requirements == ["name"]
    assert plan == {"steps": []}


def test_candidate_role_for_multi_entity_families():
    intent, entities, requirements, plan = plan_of(
        _scenario(task_type="ambiguity", koids=(_A, _B), anchor_prop="name",
                  anchor_value="pay"))
    assert entities == [
        {"koid": _A, "role": "subject"},
        {"koid": _B, "role": "candidate"},
    ]
    assert requirements == ["name"]


def test_plan_never_mentions_koids_outside_the_scenario():
    _, entities, _, plan = plan_of(
        _scenario(koids=(_A, _B, _C),
                  expected_path=((_A, "OWNS", _B), (_B, "DEPENDS_ON", _C))))
    mentioned = {e["koid"] for e in entities}
    for step in plan["steps"]:
        mentioned.update(
            k for k in (step.get("koid"), step.get("from"), step.get("to"))
            if k is not None
        )
    assert mentioned == {_A, _B, _C}


def test_plan_derivation_is_deterministic():
    kw = dict(koids=(_A, _B, _C),
              expected_path=((_A, "OWNS", _B), (_B, "DEPENDS_ON", _C)))
    assert plan_of(_scenario(**kw)) == plan_of(_scenario(**kw))
