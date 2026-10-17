"""T-09 RED: unknown scenarios (design Phase 8) — missing entities and
missing properties.

An unknown question is about what the KB does NOT know, and the example
says so, machine-readably: the answer starts with the UNKNOWN: prefix,
labels carry grounded=False/answerable=False, and the oracle proves the
absence — the query comes back with no row carrying the asked property.
Uncertainty never becomes a false positive: a name that actually
exists, or a property the KO actually has, is skipped at generation;
build_answer refuses when the context contradicts the premise; the
validator rejects any unknown example marked answerable or grounded.

Every test below fails against the current tree:
`aikoql_training.scenarios.unknown` does not exist.
"""

from __future__ import annotations

import aikoql

from aikoql_training.generators import build_answer, build_queries
from aikoql_training.scenarios.unknown import unknown_scenarios
from aikoql_training.validation import verify_scenario
from aikoql_training.validation.grounding import validate_grounding
from conftest import make_example

_K = "a" * 32

_REFUSAL = {"grounded": False, "answerable": False,
            "ambiguous": False, "contradictory": False}


def ko(koid, type_name="service", **props):
    return {"koid": koid, "type_name": type_name, "properties": dict(props)}


# -- the generator ----------------------------------------------------------

def test_missing_entity_scenario_is_a_refusal():
    [s] = unknown_scenarios([], [{"type_name": "service", "property": "owner",
                                  "name": "checkout"}])
    assert s.task_type == "unknown"
    assert s.difficulty == "factual"
    assert s.question == "What is the owner of service 'checkout'?"
    assert s.expected_answer == "UNKNOWN: no service named 'checkout'"
    assert s.koids == ()
    assert s.property == "owner"
    assert s.anchor_prop == "name"
    assert s.anchor_value == "checkout"


def test_missing_property_scenario_is_a_refusal():
    [s] = unknown_scenarios(
        [ko(_K, name="settlement", owner="Payments Team")],
        [{"type_name": "service", "property": "sla", "koid": _K}],
    )
    assert s.question == "What is the sla of service 'settlement'?"
    assert s.expected_answer == "UNKNOWN: service 'settlement' has no sla"
    assert s.koids == (_K,)
    assert s.anchor_prop == "name"
    assert s.anchor_value == "settlement"


def test_existing_name_is_not_unknown():
    kos = [ko(_K, name="checkout", owner="X")]
    out = unknown_scenarios(
        kos, [{"type_name": "service", "property": "owner", "name": "checkout"}])
    assert out == []


def test_existing_property_is_not_unknown():
    kos = [ko(_K, name="settlement", sla="99.9")]
    out = unknown_scenarios(
        kos, [{"type_name": "service", "property": "sla", "koid": _K}])
    assert out == []


def test_unknown_deterministic():
    missing = [
        {"type_name": "service", "property": "owner", "name": "checkout"},
        {"type_name": "service", "property": "sla", "koid": _K},
    ]
    kos = [ko(_K, name="settlement")]
    assert unknown_scenarios(kos, missing) == unknown_scenarios(
        kos, list(reversed(missing)))


# -- the query builder ------------------------------------------------------

def test_unknown_queries_prove_absence():
    [s] = unknown_scenarios([], [{"type_name": "service", "property": "owner",
                                  "name": "checkout"}])
    assert build_queries(s, []) == [
        'MATCH service WHERE name == "checkout" RETURN owner']
    [s2] = unknown_scenarios(
        [ko(_K, name="settlement")],
        [{"type_name": "service", "property": "sla", "koid": _K}],
    )
    assert build_queries(s2, [ko(_K, name="settlement")]) == [
        'MATCH service WHERE name == "settlement" RETURN sla']


# -- the answer generator ---------------------------------------------------

def test_build_answer_unknown_emits_refusal_labels():
    [s] = unknown_scenarios([], [{"type_name": "service", "property": "owner",
                                  "name": "checkout"}])
    ctx = {"entities": [], "facts": [], "relations": [], "evidence": []}
    assert build_answer(s, ctx) == {
        "answer": "UNKNOWN: no service named 'checkout'",
        "evidence_ids": [],
        "labels": _REFUSAL,
    }


def test_build_answer_refuses_when_context_knows_the_name():
    # the premise ("checkout is unknown") contradicts the context —
    # never emit a false positive
    [s] = unknown_scenarios([], [{"type_name": "service", "property": "owner",
                                  "name": "checkout"}])
    ctx = {"entities": [], "relations": [], "evidence": [],
           "facts": [{"statement": "The checkout service is owned by X",
                      "evidence": {}}]}
    assert build_answer(s, ctx) is None


# -- the validator ----------------------------------------------------------

def _example(labels=None, answer=None, evidence_ids=None):
    return make_example(
        task={"type": "unknown", "difficulty": "factual", "requires": []},
        input={"question": "What is the owner of service 'checkout'?"},
        expected={
            "answer": answer if answer is not None
            else "UNKNOWN: no service named 'checkout'",
            "koids": [],
            "evidence_ids": evidence_ids or [],
        },
        labels=labels or _REFUSAL,
    )


def test_validator_accepts_honest_unknown():
    assert validate_grounding(_example()) == {"ok": True, "errors": []}


def test_validator_rejects_unknown_marked_answerable():
    out = validate_grounding(_example(labels={**_REFUSAL, "answerable": True}))
    assert out["ok"] is False
    assert any("answerable" in e for e in out["errors"])


def test_validator_rejects_unknown_marked_grounded():
    out = validate_grounding(_example(labels={**_REFUSAL, "grounded": True}))
    assert out["ok"] is False
    assert any("grounded" in e for e in out["errors"])


def test_validator_rejects_unknown_with_evidence_ids():
    out = validate_grounding(_example(evidence_ids=["bogus"]))
    assert out["ok"] is False


def test_validator_rejects_non_refusal_answer():
    out = validate_grounding(_example(answer="Payments Team"))
    assert out["ok"] is False


# -- live: real absence over the wire ---------------------------------------

def test_live_unknown_proves_absence(mcp_server):
    """Real absence end to end: the missing-entity query returns no
    rows and the missing-property query returns no row carrying the
    property — the DB really has nothing, so the refusal is the only
    honest answer."""
    host, token = mcp_server
    with aikoql.Agent.connect(host, token=token) as db:
        koid = db.remember("service", {"name": "settlement",
                                       "owner": "Payments Team"})["koid"]
        kos = [db.get(koid)]
        missing = [
            {"type_name": "service", "property": "owner", "name": "checkout"},
            {"type_name": "service", "property": "sla", "koid": koid},
        ]
        scenarios = unknown_scenarios(kos, missing)
        assert scenarios
        for s in scenarios:
            queries = build_queries(s, kos)
            assert queries, s.scenario_id
            for q in queries:
                env = db.aikoql(q)
                rows = env.get("results", [])
                assert not any(
                    s.property in (r.get("properties") or {}) for r in rows
                ), q
            assert verify_scenario(db, s, queries)["ok"], s.scenario_id
            ctx = {"entities": [], "facts": [], "relations": [], "evidence": []}
            out = build_answer(s, ctx)
            assert out is not None
            assert out["labels"]["answerable"] is False
