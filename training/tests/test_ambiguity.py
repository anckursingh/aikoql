"""T-09 RED: ambiguity scenarios (design Phase 8) — ambiguous pairs.

Two KOs of the same type sharing a scalar property value cannot be told
apart by that value; a question referencing the pair must enumerate
BOTH candidates machine-readably (AMBIGUOUS prefix, one `koid -> value`
entry per candidate, sorted by koid) and carry labels.ambiguous=True.
The oracle proves the pair is real: the query comes back with both
values. The enumeration delimiters ("; " and " -> ") are guarded at
generation — a candidate value containing one would break the parse,
so the pair is skipped.

Every test below fails against the current tree:
`aikoql_training.scenarios.ambiguity` does not exist.
"""

from __future__ import annotations

import aikoql
from hypothesis import given
from hypothesis import strategies as st

from aikoql_training.generators import build_answer, build_queries
from aikoql_training.models import validate as validate_schema
from aikoql_training.scenarios.ambiguity import ambiguity_scenarios
from aikoql_training.validation.grounding import evidence_id, validate_grounding
from conftest import make_example

_A, _B = "a" * 32, "b" * 32
_AMB_LABELS = {"grounded": True, "answerable": True,
               "ambiguous": True, "contradictory": False}
_EV = {"document_id": "d1", "extractor": "e"}


def ko(koid, type_name="service", **props):
    return {"koid": koid, "type_name": type_name, "properties": dict(props)}


# -- the generator ----------------------------------------------------------

def test_ambiguous_pair_enumerates_both_candidates():
    kos = [ko(_A, name="gateway", owner="Payments Team"),
           ko(_B, name="gateway", owner="Checkout Team")]
    [s] = ambiguity_scenarios(kos)
    assert s.task_type == "ambiguity"
    assert s.question == (
        "What is the owner of the service whose name is gateway?")
    assert s.expected_answer == (
        f"AMBIGUOUS (2 candidates): {_A[:8]} -> Payments Team; "
        f"{_B[:8]} -> Checkout Team")
    assert s.koids == (_A, _B)
    assert s.property == "owner"
    assert s.anchor_prop == "name"
    assert s.anchor_value == "gateway"
    assert s.candidates == ((_A, "Payments Team"), (_B, "Checkout Team"))


def test_no_collision_no_scenarios():
    assert ambiguity_scenarios(
        [ko(_A, name="gateway"), ko(_B, name="checkout")]) == []


def test_equal_values_on_every_common_property_not_ambiguous():
    kos = [ko(_A, name="gateway", owner="X"), ko(_B, name="gateway", owner="X")]
    assert ambiguity_scenarios(kos) == []


def test_unparseable_candidate_values_skipped():
    # "; " and " -> " are the enumeration delimiters — a value
    # containing one would break the machine-readable parse
    kos = [ko(_A, name="gateway", owner="A; B"),
           ko(_B, name="gateway", owner="C")]
    assert ambiguity_scenarios(kos) == []


def test_ambiguity_deterministic():
    kos = [ko(_A, name="gateway", owner="Payments Team"),
           ko(_B, name="gateway", owner="Checkout Team")]
    assert ambiguity_scenarios(kos) == ambiguity_scenarios(list(reversed(kos)))


# -- the query builder ------------------------------------------------------

def test_ambiguity_query_recovers_both():
    kos = [ko(_A, name="gateway", owner="Payments Team"),
           ko(_B, name="gateway", owner="Checkout Team")]
    [s] = ambiguity_scenarios(kos)
    assert build_queries(s, kos) == [
        'MATCH service WHERE name == "gateway" RETURN owner']


# -- the answer generator ---------------------------------------------------

def test_build_answer_traces_both_candidates():
    kos = [ko(_A, name="gateway", owner="Payments Team"),
           ko(_B, name="gateway", owner="Checkout Team")]
    [s] = ambiguity_scenarios(kos)
    ctx = {
        "entities": [], "relations": [], "evidence": [_EV],
        "facts": [
            {"statement": "The gateway service is owned by the Payments Team",
             "evidence": _EV},
            {"statement": "The gateway service is owned by the Checkout Team",
             "evidence": _EV},
        ],
    }
    assert build_answer(s, ctx) == {
        "answer": s.expected_answer,
        "evidence_ids": [evidence_id(_EV)],
        "labels": _AMB_LABELS,
    }


def test_build_answer_refuses_untraced_candidate():
    kos = [ko(_A, name="gateway", owner="Payments Team"),
           ko(_B, name="gateway", owner="Checkout Team")]
    [s] = ambiguity_scenarios(kos)
    ctx = {"entities": [], "relations": [], "evidence": [],
           "facts": [{"statement": "The gateway service is owned by "
                                   "the Payments Team",
                      "evidence": {}}]}
    assert build_answer(s, ctx) is None


# -- the validator ----------------------------------------------------------

def _example(labels=None, answer=None, facts=None):
    kos = [ko(_A, name="gateway", owner="Payments Team"),
           ko(_B, name="gateway", owner="Checkout Team")]
    [s] = ambiguity_scenarios(kos)
    return make_example(
        task={"type": "ambiguity", "difficulty": "factual", "requires": []},
        input={"question": s.question},
        context={
            "entities": [], "relations": [], "evidence": [_EV],
            "facts": facts or [
                {"statement": "The gateway service is owned by the "
                              "Payments Team",
                 "evidence": _EV},
                {"statement": "The gateway service is owned by the "
                              "Checkout Team",
                 "evidence": _EV},
            ],
        },
        expected={
            "answer": answer if answer is not None else s.expected_answer,
            "koids": list(s.koids),
            "evidence_ids": [evidence_id(_EV)],
        },
        labels=labels or _AMB_LABELS,
    )


def test_validator_accepts_ambiguous_example():
    assert validate_grounding(_example()) == {"ok": True, "errors": []}


def test_validator_rejects_ambiguity_without_the_label():
    out = validate_grounding(
        _example(labels={**_AMB_LABELS, "ambiguous": False}))
    assert out["ok"] is False
    assert any("ambiguous" in e for e in out["errors"])


def test_validator_rejects_untraced_candidate():
    ex = _example()
    ex["context"]["facts"] = ex["context"]["facts"][:1]
    out = validate_grounding(ex)
    assert out["ok"] is False


def test_validator_rejects_misleading_enumeration():
    # the answer says 2 candidates but only one traces: the enumeration
    # must match the context, never invent a side
    kos = [ko(_A, name="gateway", owner="Payments Team"),
           ko(_B, name="gateway", owner="Checkout Team")]
    [s] = ambiguity_scenarios(kos)
    ex = _example(facts=[{"statement": "The gateway service is owned by the "
                                       "Payments Team",
                          "evidence": _EV}])
    assert validate_grounding(ex)["ok"] is False


def test_schema_accepts_ambiguity_type():
    validate_schema(_example())


# -- property: generator accepts ⇒ validator accepts ------------------------

_alpha = st.characters(min_codepoint=ord("a"), max_codepoint=ord("z"))


@st.composite
def _ambiguous_kos(draw):
    name = draw(st.text(min_size=1, max_size=10, alphabet=_alpha))
    owner_a = draw(st.text(min_size=1, max_size=20, alphabet=_alpha))
    owner_b = draw(st.text(min_size=1, max_size=20, alphabet=_alpha))
    return [ko(_A, name=name, owner=owner_a),
            ko(_B, name=name, owner=owner_b)]


@given(kos=_ambiguous_kos())
def test_generator_accepts_implies_validator_accepts(kos):
    for s in ambiguity_scenarios(kos):
        facts = [
            {"statement": f"The gateway service is owned by {v}",
             "evidence": _EV}
            for _, v in s.candidates
        ]
        ctx = {"entities": [], "relations": [], "evidence": [_EV],
               "facts": facts}
        out = build_answer(s, ctx)
        if out is None:
            continue
        example = make_example(
            task={"type": "ambiguity", "difficulty": "factual", "requires": []},
            input={"question": s.question},
            context=ctx,
            expected={"answer": out["answer"], "koids": list(s.koids),
                      "evidence_ids": out["evidence_ids"]},
            labels=out["labels"],
        )
        assert validate_grounding(example) == {"ok": True, "errors": []}


# -- live: real pair over the wire ------------------------------------------

def test_live_ambiguous_pair_enumerates_both(mcp_server):
    """Real pair end to end: two same-name services in the live KB, the
    query returns both rows, and the accepted example enumerates both
    live values."""
    import json as _json

    from aikoql_training.context import compile_context
    from aikoql_training.validation import verify_scenario

    host, token = mcp_server
    with aikoql.Agent.connect(host, token=token) as db:
        a = db.remember("service", {"name": "gateway",
                                    "owner": "Payments Team"})["koid"]
        b = db.remember("service", {"name": "gateway",
                                    "owner": "Checkout Team"})["koid"]
        kos = [db.get(a), db.get(b)]
        scenarios = ambiguity_scenarios(kos)
        assert scenarios
        ev = {"document_id": "gateway.md", "extractor": "mock-v1",
              "confidence": 0.75}
        doc = db.remember("KnowledgeSnapshot", {"ir_json": _json.dumps({
            "entities": [], "relations": [],
            "facts": [
                {"statement": "The gateway service is owned by the "
                              "Payments Team",
                 "entities": ["Gateway"], "confidence": 0.9, "evidence": ev},
                {"statement": "The gateway service is owned by the "
                              "Checkout Team",
                 "entities": ["Gateway"], "confidence": 0.9, "evidence": ev},
            ],
            "events": [], "temporal": [],
            "page_count": 1, "extractor": "mock-v1",
        })})
        ctx = compile_context(db, doc["koid"], "who owns the gateway service")
        assert len(ctx["facts"]) >= 2
        assert ctx["evidence"]
        for s in scenarios:
            queries = build_queries(s, kos)
            assert queries, s.scenario_id
            env = db.aikoql(queries[0])
            values = {str(r["properties"][s.property])
                      for r in env.get("results", [])
                      if s.property in (r.get("properties") or {})}
            assert values == {v for _, v in s.candidates}, values
            assert verify_scenario(db, s, queries)["ok"], s.scenario_id
            out = build_answer(s, ctx)
            assert out is not None
            assert out["labels"]["ambiguous"] is True
