"""T-09 RED: contradiction scenarios (design Phase 8) — conflicting
facts preserve Conflict metadata.

Two claims disagree on a property; the kernel holds a real Conflict KO
(contradict: CONTRADICTS edge + `aikoql:conflict` KO). The example
answers machine-readably (CONTRADICTED prefix, both values with their
claim koids) and preserves the Conflict metadata: the conflict koid and
its resolution state appear verbatim — the generator never picks a
side. labels.contradictory=True; the oracle proves both claims are
retrievable (the query returns both values).

Every test below fails against the current tree:
`aikoql_training.scenarios.contradiction` does not exist.
"""

from __future__ import annotations

import aikoql

from aikoql_training.generators import build_answer, build_queries
from aikoql_training.models import validate as validate_schema
from aikoql_training.scenarios.contradiction import contradiction_scenarios
from aikoql_training.validation.grounding import evidence_id, validate_grounding
from conftest import make_example

_A, _B, _C = "a" * 32, "b" * 32, "c" * 32
_CTR_LABELS = {"grounded": True, "answerable": True,
               "ambiguous": False, "contradictory": True}
_EV = {"document_id": "d1", "extractor": "e"}


def ko(koid, type_name="service", **props):
    return {"koid": koid, "type_name": type_name, "properties": dict(props)}


def _conflict(koid=_C, claims=(_A, _B), resolution="unresolved"):
    return {
        "koid": koid,
        "type_name": "aikoql:conflict",
        "resolution": resolution,
        "properties": {
            "description": f"Contradictory claims: {claims[0]} vs {claims[1]}",
            "claim_a": claims[0],
            "claim_b": claims[1],
        },
    }


# -- the generator ----------------------------------------------------------

def test_contradiction_preserves_conflict_metadata():
    claims = [ko(_A, name="settlement", owner="Payments Team"),
              ko(_B, name="settlement", owner="Checkout Team")]
    [s] = contradiction_scenarios([{"conflict": _conflict(), "claims": claims}])
    assert s.task_type == "contradiction"
    assert s.question == (
        "What is the owner of the service whose name is settlement?")
    assert s.expected_answer == (
        f"CONTRADICTED: Payments Team (claim {_A[:8]}) vs "
        f"Checkout Team (claim {_B[:8]}); conflict {_C[:8]}, "
        f"resolution unresolved")
    assert s.koids == (_A, _B, _C)
    assert s.property == "owner"
    assert s.anchor_prop == "name"
    assert s.anchor_value == "settlement"
    assert s.candidates == ((_A, "Payments Team"), (_B, "Checkout Team"))


def test_resolution_state_is_preserved_verbatim():
    claims = [ko(_A, name="settlement", owner="X"),
              ko(_B, name="settlement", owner="Y")]
    [s] = contradiction_scenarios(
        [{"conflict": _conflict(resolution="resolved_a_preferred"),
          "claims": claims}])
    assert s.expected_answer.endswith("resolution resolved_a_preferred")


def test_claims_must_match_the_conflict_record():
    # the conflict KO names claim_a/claim_b; claims that do not match
    # the recorded koids are malformed input — skipped, never emitted
    conflict = _conflict(claims=("d" * 32, _B))
    claims = [ko(_A, name="settlement", owner="X"),
              ko(_B, name="settlement", owner="Y")]
    assert contradiction_scenarios(
        [{"conflict": conflict, "claims": claims}]) == []


def test_identical_claims_not_a_contradiction():
    claims = [ko(_A, name="settlement", owner="X"),
              ko(_B, name="settlement", owner="X")]
    assert contradiction_scenarios(
        [{"conflict": _conflict(), "claims": claims}]) == []


def test_different_claim_types_skipped():
    claims = [ko(_A, name="settlement", owner="X"),
              ko(_B, type_name="team", name="settlement", owner="Y")]
    assert contradiction_scenarios(
        [{"conflict": _conflict(), "claims": claims}]) == []


def test_no_shared_anchor_skipped():
    claims = [ko(_A, owner="X"), ko(_B, owner="Y")]
    assert contradiction_scenarios(
        [{"conflict": _conflict(), "claims": claims}]) == []


def test_contradiction_deterministic():
    claims = [ko(_A, name="settlement", owner="Payments Team"),
              ko(_B, name="settlement", owner="Checkout Team")]
    c = {"conflict": _conflict(), "claims": claims}
    assert contradiction_scenarios([c]) == contradiction_scenarios([
        {"conflict": c["conflict"], "claims": list(reversed(c["claims"]))}])


# -- the query builder ------------------------------------------------------

def test_contradiction_query_recovers_both_claims():
    claims = [ko(_A, name="settlement", owner="Payments Team"),
              ko(_B, name="settlement", owner="Checkout Team")]
    conflict = _conflict()
    [s] = contradiction_scenarios([{"conflict": conflict, "claims": claims}])
    assert build_queries(s, claims + [conflict]) == [
        'MATCH service WHERE name == "settlement" RETURN owner']


# -- the answer generator ---------------------------------------------------

def test_build_answer_traces_both_claims():
    claims = [ko(_A, name="settlement", owner="Payments Team"),
              ko(_B, name="settlement", owner="Checkout Team")]
    [s] = contradiction_scenarios([{"conflict": _conflict(), "claims": claims}])
    ctx = {
        "entities": [], "relations": [], "evidence": [_EV],
        "facts": [
            {"statement": "The settlement service is owned by the "
                          "Payments Team",
             "evidence": _EV},
            {"statement": "The settlement service is owned by the "
                          "Checkout Team",
             "evidence": _EV},
        ],
    }
    assert build_answer(s, ctx) == {
        "answer": s.expected_answer,
        "evidence_ids": [evidence_id(_EV)],
        "labels": _CTR_LABELS,
    }


def test_build_answer_refuses_untraced_claim():
    claims = [ko(_A, name="settlement", owner="Payments Team"),
              ko(_B, name="settlement", owner="Checkout Team")]
    [s] = contradiction_scenarios([{"conflict": _conflict(), "claims": claims}])
    ctx = {"entities": [], "relations": [], "evidence": [],
           "facts": [{"statement": "The settlement service is owned by the "
                                   "Payments Team",
                      "evidence": {}}]}
    assert build_answer(s, ctx) is None


# -- the validator ----------------------------------------------------------

def _example(labels=None, answer=None, facts=None):
    claims = [ko(_A, name="settlement", owner="Payments Team"),
              ko(_B, name="settlement", owner="Checkout Team")]
    [s] = contradiction_scenarios([{"conflict": _conflict(), "claims": claims}])
    return make_example(
        task={"type": "contradiction", "difficulty": "factual", "requires": []},
        input={"question": s.question},
        context={
            "entities": [], "relations": [], "evidence": [_EV],
            "facts": facts or [
                {"statement": "The settlement service is owned by the "
                              "Payments Team",
                 "evidence": _EV},
                {"statement": "The settlement service is owned by the "
                              "Checkout Team",
                 "evidence": _EV},
            ],
        },
        expected={
            "answer": answer if answer is not None else s.expected_answer,
            "koids": list(s.koids),
            "evidence_ids": [evidence_id(_EV)],
        },
        labels=labels or _CTR_LABELS,
    )


def test_validator_accepts_contradictory_example():
    assert validate_grounding(_example()) == {"ok": True, "errors": []}


def test_validator_rejects_contradiction_without_the_label():
    out = validate_grounding(
        _example(labels={**_CTR_LABELS, "contradictory": False}))
    assert out["ok"] is False
    assert any("contradictory" in e for e in out["errors"])


def test_validator_rejects_untraced_claim():
    ex = _example()
    ex["context"]["facts"] = ex["context"]["facts"][:1]
    assert validate_grounding(ex)["ok"] is False


def test_validator_rejects_answer_without_conflict_metadata():
    # a contradiction answer that dropped the conflict id / resolution
    # is a machine-readable violation — the metadata must be preserved
    out = validate_grounding(_example(answer="CONTRADICTED: Payments Team "
                                             "vs Checkout Team"))
    assert out["ok"] is False
    assert any("conflict" in e for e in out["errors"])


def test_schema_accepts_contradiction_type():
    validate_schema(_example())


# -- live: real Conflict over the wire --------------------------------------

def test_live_conflicting_claims_preserve_conflict(mcp_server):
    """Real Conflict end to end: claim A remembered, claim B registered
    through contradict — the kernel's Conflict KO is the metadata the
    example preserves (koid + resolution). The query returns both
    values; the generator never picks a side."""
    import json as _json

    from aikoql_training.context import compile_context
    from aikoql_training.validation import verify_scenario

    host, token = mcp_server
    with aikoql.Agent.connect(host, token=token) as db:
        a = db.remember("service", {"name": "settlement",
                                    "owner": "Payments Team"})["koid"]
        r = db._backend.call_tool("contradict", {
            "claim": a,
            "counter_type": "service",
            "properties": {"name": "settlement", "owner": "Checkout Team"},
            "evidence": [{"source_artifact": "audit.md",
                          "method": "doc_extraction", "confidence": 0.75}],
        })
        conflict_koid = r["conflict"]
        conflict = db.get(conflict_koid)
        assert conflict["properties"]["claim_a"] == a
        assert conflict["properties"]["claim_b"] == r["counter"]
        assert conflict["resolution"] == "unresolved"
        claims = [db.get(a), db.get(r["counter"])]
        scenarios = contradiction_scenarios(
            [{"conflict": conflict, "claims": claims}])
        assert scenarios
        ev = {"document_id": "settlement.md", "extractor": "mock-v1",
              "confidence": 0.75}
        doc = db.remember("KnowledgeSnapshot", {"ir_json": _json.dumps({
            "entities": [], "relations": [],
            "facts": [
                {"statement": "The settlement service is owned by the "
                              "Payments Team",
                 "entities": ["Settlement"], "confidence": 0.9,
                 "evidence": ev},
                {"statement": "The settlement service is owned by the "
                              "Checkout Team",
                 "entities": ["Settlement"], "confidence": 0.9,
                 "evidence": ev},
            ],
            "events": [], "temporal": [],
            "page_count": 1, "extractor": "mock-v1",
        })})
        ctx = compile_context(db, doc["koid"],
                              "who owns the settlement service")
        assert len(ctx["facts"]) >= 2
        assert ctx["evidence"]
        for s in scenarios:
            queries = build_queries(s, claims + [conflict])
            assert queries, s.scenario_id
            env = db.aikoql(queries[0])
            values = {str(row["properties"][s.property])
                      for row in env.get("results", [])
                      if s.property in (row.get("properties") or {})}
            assert values == {v for _, v in s.candidates}, values
            assert verify_scenario(db, s, queries)["ok"], s.scenario_id
            out = build_answer(s, ctx)
            assert out is not None
            assert out["labels"]["contradictory"] is True
            assert conflict_koid[:8] in out["answer"]
            assert "resolution unresolved" in out["answer"]
