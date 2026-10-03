"""T-04 RED — multi-hop scenario generator (design Phase 5).

A two-edge path A --r1--> B --r2--> C yields one scenario carrying the
EXACT path walked; the intermediate KO is the answer. The generator
builds only from the edges it is given (the real scanned graph): a
fabricated path never generates a scenario, and every edge in every
expected_path is present in the input.
"""

from aikoql import Agent

from aikoql_training.client import scan_edges
from aikoql_training.scenarios import multi_hop_scenarios
from conftest import make_example


def ko(koid, type_name="service", **props):
    return {"koid": koid, "type_name": type_name, "properties": dict(props)}


def test_two_hop_path_exact_and_grounded():
    a, b, c = "a" * 32, "b" * 32, "c" * 32
    edges = [
        {"from": a, "rel": "OWNS", "to": b},
        {"from": b, "rel": "DEPENDS_ON", "to": c},
    ]
    scenarios = multi_hop_scenarios(
        edges, [ko(a, name="Acme"), ko(b, name="api"), ko(c, name="settlement")]
    )
    assert len(scenarios) == 1
    s = scenarios[0]
    assert s.expected_path == ((a, "OWNS", b), (b, "DEPENDS_ON", c))
    assert s.koids == (a, b, c)
    assert s.difficulty == "multi_hop"
    assert s.question == (
        "What does service 'Acme' own that depends on service 'settlement'?"
    )
    assert s.expected_answer == "service 'api'"
    assert s.scenario_id == f"multi_hop:OWNS:{a}:{b}:DEPENDS_ON:{c}"


def test_no_fabricated_paths():
    a, b, c = "a" * 32, "b" * 32, "c" * 32
    edges = [
        {"from": a, "rel": "OWNS", "to": b},
        {"from": b, "rel": "DEPENDS_ON", "to": c},
        {"from": b, "rel": "MENTIONS", "to": "f" * 32},  # dangling tail
    ]
    kos = [ko(a), ko(b), ko(c)]
    input_edges = {(e["from"], e["rel"], e["to"]) for e in edges}
    scenarios = multi_hop_scenarios(edges, kos)
    # every edge of every scenario exists verbatim in the input graph
    assert all(all(path in input_edges for path in s.expected_path)
               for s in scenarios)
    # the dangling third edge must not fabricate a scenario
    assert all("f" * 32 not in s.koids for s in scenarios)


def test_chained_edges_that_share_no_node_generate_nothing():
    a, b, c, d = "a" * 32, "b" * 32, "c" * 32, "d" * 32
    edges = [
        {"from": a, "rel": "OWNS", "to": b},
        {"from": c, "rel": "DEPENDS_ON", "to": d},  # disjoint component
    ]
    assert multi_hop_scenarios(edges, [ko(x) for x in (a, b, c, d)]) == []


def test_single_edge_generates_nothing():
    a, b = "a" * 32, "b" * 32
    edges = [{"from": a, "rel": "OWNS", "to": b}]
    assert multi_hop_scenarios(edges, [ko(a), ko(b)]) == []


def test_dangling_path_generates_nothing():
    a, b = "a" * 32, "b" * 32
    c = "c" * 32  # not among the known KOs: the tail's data is unknown
    edges = [
        {"from": a, "rel": "OWNS", "to": b},
        {"from": b, "rel": "DEPENDS_ON", "to": c},
    ]
    assert multi_hop_scenarios(edges, [ko(a), ko(b)]) == []


def test_branching_intermediate_yields_one_scenario_per_path():
    a, b, c, d = "a" * 32, "b" * 32, "c" * 32, "d" * 32
    edges = [
        {"from": a, "rel": "OWNS", "to": b},
        {"from": b, "rel": "DEPENDS_ON", "to": c},
        {"from": b, "rel": "DEPENDS_ON", "to": d},
    ]
    kos = [ko(x) for x in (a, b, c, d)]
    paths = {s.expected_path for s in multi_hop_scenarios(edges, kos)}
    assert paths == {
        ((a, "OWNS", b), (b, "DEPENDS_ON", c)),
        ((a, "OWNS", b), (b, "DEPENDS_ON", d)),
    }


def test_multi_hop_deterministic():
    a, b, c = "a" * 32, "b" * 32, "c" * 32
    edges = [
        {"from": a, "rel": "OWNS", "to": b},
        {"from": b, "rel": "DEPENDS_ON", "to": c},
    ]
    kos = [ko(a), ko(b), ko(c)]
    assert multi_hop_scenarios(edges, kos) == multi_hop_scenarios(
        list(reversed(edges)), kos
    )


def test_scenario_feeds_example_schema():
    a, b, c = "a" * 32, "b" * 32, "c" * 32
    edges = [
        {"from": a, "rel": "OWNS", "to": b},
        {"from": b, "rel": "DEPENDS_ON", "to": c},
    ]
    s = multi_hop_scenarios(edges, [ko(a), ko(b), ko(c)])[0]
    example = make_example(
        source={**make_example()["source"], "scenario_id": s.scenario_id},
        input={"question": s.question},
        expected={"answer": s.expected_answer, "koids": list(s.koids),
                  "evidence_ids": []},
        task={"type": s.task_type, "difficulty": s.difficulty, "requires": []},
    )
    from aikoql_training.models import validate

    validate(example)


# -- live acceptance ---------------------------------------------------

def test_live_multi_hop_verifies_against_database(mcp_server):
    """The path exists in the database: settlement -> checkout -> gateway,
    recovered via traverse and re-verified edge by edge."""
    host, token = mcp_server
    with Agent.connect(host, token=token) as db:
        a = db.remember("service", {"name": "settlement"})["koid"]
        b = db.remember("service", {"name": "checkout"})["koid"]
        c = db.remember("service", {"name": "gateway"})["koid"]
        db.relate(a, b, "DEPENDS_ON")
        db.relate(b, c, "DEPENDS_ON")
        kos = [db.get(a), db.get(b), db.get(c)]
        edges = scan_edges(db, [a, b, c])

        scenarios = multi_hop_scenarios(edges, kos)
        assert len(scenarios) == 1
        s = scenarios[0]
        assert s.expected_path == ((a, "DEPENDS_ON", b), (b, "DEPENDS_ON", c))
        assert s.expected_answer == "service 'checkout'"
        live_edges = {(e["from"], e["rel"], e["to"]) for e in edges}
        assert all(path in live_edges for path in s.expected_path)
