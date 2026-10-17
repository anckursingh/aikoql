"""T-03 RED — factual + relation scenario generators (design Phase 3+4, §11).

Scenarios operate on ACTUAL knowledge: a generator consumes KO/edge data
read from the database and produces question/expected-answer pairs; it
never invents facts (no fabricated edges, no made-up answers).

RED (design's Phase 3+4 lists):
  factual  — one KO, one property, one question, one expected answer;
             deterministic ids/questions; scalar properties only
  relation — one-hop, inverse, relation filtering, missing relation;
             the expected path is stored and validated
  acceptance — 100% of generated scenarios verify against the live
             database (factual answers re-read via get, relation paths
             recovered via traverse)
"""

from aikoql import Agent

from aikoql_training.client import scan_edges
from aikoql_training.scenarios import (
    Scenario,
    factual_scenarios,
    relation_scenarios,
)
from conftest import make_example


def ko(koid, type_name="service", **props):
    return {"koid": koid, "type_name": type_name, "properties": dict(props)}


# -- factual -----------------------------------------------------------

def test_one_ko_one_property_one_question_one_answer():
    scenarios = factual_scenarios([ko("a" * 32, owner="Payments Team")])
    assert len(scenarios) == 1
    s = scenarios[0]
    assert s.question == "What is the owner of service aaaaaaaa?"
    assert s.expected_answer == "Payments Team"
    assert s.scenario_id == f"factual:service:owner:{'a' * 32}"


def test_factual_prefers_name_in_question():
    scenarios = factual_scenarios(
        [ko("a" * 32, name="settlement", owner="Payments Team")]
    )
    by_id = {s.scenario_id: s for s in scenarios}
    s = by_id[f"factual:service:owner:{'a' * 32}"]
    assert s.question == "What is the owner of service 'settlement'?"


def test_factual_one_scenario_per_scalar_property_sorted():
    scenarios = factual_scenarios(
        [ko("a" * 32, name="svc", tier=1, owner="Payments Team", active=True)]
    )
    assert [s.scenario_id.split(":")[2] for s in scenarios] == [
        "active", "name", "owner", "tier",
    ]


def test_factual_skips_nested_empty_and_none():
    scenarios = factual_scenarios(
        [ko("a" * 32, nested={"x": 1}, items=[1, 2], blank="", missing=None,
            ok="fine")]
    )
    assert [s.scenario_id.split(":")[2] for s in scenarios] == ["ok"]


def test_factual_deterministic():
    kos = [ko("b" * 32, owner="B"), ko("a" * 32, owner="A", tier=2)]
    first = factual_scenarios(kos)
    second = factual_scenarios(list(reversed(kos)))
    assert first == second  # sorted by koid, property order stable


def test_factual_scenario_shape():
    scenarios = factual_scenarios([ko("a" * 32, owner="Payments Team")])
    s = scenarios[0]
    assert s.task_type == "grounded_qa"
    assert s.difficulty == "factual"
    assert s.koids == ("a" * 32,)
    assert s.property == "owner"
    assert s.expected_path == ()


# -- relation ----------------------------------------------------------

def test_one_hop_forward_and_inverse():
    a, b = "a" * 32, "b" * 32
    edges = [{"from": a, "rel": "OWNS", "to": b}]
    scenarios = relation_scenarios(edges, [ko(a, name="Acme"), ko(b, name="api")], "OWNS")
    by_id = {s.scenario_id: s for s in scenarios}
    fwd = by_id[f"relation:OWNS:{a}:{b}:forward"]
    inv = by_id[f"relation:OWNS:{a}:{b}:inverse"]
    assert fwd.question == "Who owns service 'api'?"
    assert fwd.expected_answer == "service 'Acme'"
    assert inv.question == "What does service 'Acme' own?"
    assert inv.expected_answer == "service 'api'"


def test_relation_expected_path_stored():
    a, b = "a" * 32, "b" * 32
    edges = [{"from": a, "rel": "OWNS", "to": b}]
    scenarios = relation_scenarios(edges, [ko(a), ko(b)], "OWNS")
    assert all(s.expected_path == ((a, "OWNS", b),) for s in scenarios)
    assert all(s.difficulty == "one_hop" for s in scenarios)
    assert all(s.koids == (a, b) for s in scenarios)


def test_relation_filtering_by_rel_type():
    a, b, c = "a" * 32, "b" * 32, "c" * 32
    edges = [
        {"from": a, "rel": "OWNS", "to": b},
        {"from": b, "rel": "DEPENDS_ON", "to": c},
    ]
    scenarios = relation_scenarios(edges, [ko(a), ko(b), ko(c)], "OWNS")
    assert len(scenarios) == 2  # forward + inverse for the OWNS edge only


def test_missing_relation_generates_nothing():
    a, b = "a" * 32, "b" * 32
    assert relation_scenarios([], [ko(a), ko(b)], "OWNS") == []


def test_dangling_edge_is_not_fabricated():
    a, b = "a" * 32, "b" * 32
    edges = [{"from": a, "rel": "OWNS", "to": b}]
    # b is not among the known KOs: the generator must not invent its data.
    assert relation_scenarios(edges, [ko(a)], "OWNS") == []


def test_relation_deterministic():
    a, b, c = "a" * 32, "b" * 32, "c" * 32
    edges = [
        {"from": b, "rel": "OWNS", "to": c},
        {"from": a, "rel": "OWNS", "to": b},
    ]
    kos = [ko(a), ko(b), ko(c)]
    assert relation_scenarios(edges, kos, "OWNS") == relation_scenarios(
        list(reversed(edges)), kos, "OWNS"
    )


def test_scenario_feeds_example_schema():
    s = factual_scenarios([ko("a" * 32, owner="Payments Team")])[0]
    example = make_example(
        source={**make_example()["source"], "scenario_id": s.scenario_id},
        input={"question": s.question},
        expected={"answer": s.expected_answer, "koids": list(s.koids), "evidence_ids": []},
        task={"type": s.task_type, "difficulty": s.difficulty, "requires": []},
    )
    from aikoql_training.models import validate

    validate(example)


# -- live acceptance ---------------------------------------------------

def test_live_scenarios_verify_against_database(mcp_server):
    """100% acceptance: every generated scenario re-verifies against the
    live database — factual answers via get(), relation paths via
    traverse() through the public surface."""
    host, token = mcp_server
    with Agent.connect(host, token=token) as db:
        a = db.remember("service", {"name": "settlement", "owner": "Payments Team",
                                    "tier": 1})["koid"]
        b = db.remember("service", {"name": "checkout", "owner": "Payments Team"})["koid"]
        db.relate(a, b, "DEPENDS_ON")
        kos = [db.get(a), db.get(b)]
        edges = scan_edges(db, [a, b])

        factual = factual_scenarios(kos)
        assert len(factual) == 5  # settlement: name, owner, tier; checkout: name, owner
        for s in factual:
            live = next(k for k in kos if k["koid"] == s.koids[0])
            assert s.expected_answer == str(live["properties"][s.property])

        live_edges = {(e["from"], e["rel"], e["to"]) for e in edges}
        relation = relation_scenarios(edges, kos, "DEPENDS_ON")
        assert len(relation) == 2  # forward + inverse
        for s in relation:
            assert all(path in live_edges for path in s.expected_path)
