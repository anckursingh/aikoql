"""T-08 RED: provenance scenarios (design Phase 7).

Provenance examples cite REAL evidence: every generated scenario
carries the KO's evidence entries, the answer generator refuses any
example whose evidence is absent from the compiled context, and the
validator fails `evidence_ids` that do not point at real context
evidence. Every test below fails against the current tree:
`aikoql_training.scenarios.provenance` does not exist.
"""

from __future__ import annotations

import json

import aikoql
from hypothesis import given
from hypothesis import strategies as st

from aikoql_training.context import compile_context
from aikoql_training.generators import build_queries
from aikoql_training.generators.answer import build_answer
from aikoql_training.scenarios.provenance import provenance_scenarios
from aikoql_training.validation import verify_scenario
from aikoql_training.validation.grounding import evidence_id, validate_grounding


_K = "a" * 32
_EV = {"source_artifact": "acmepay.md", "method": "doc_extraction",
       "location": "§2", "revision": "r1", "confidence": 0.9}
_CITE = "acmepay.md (doc_extraction) §2 @r1"


def _ko(koid, evidence=None, **props):
    ko = {"koid": koid, "type_name": "service", "properties": dict(props)}
    if evidence is not None:
        ko["evidence"] = evidence
    return ko


# -- the generator ----------------------------------------------------------

def test_provenance_scenario_cites_real_evidence():
    s = provenance_scenarios([_ko(_K, owner="Payments Team", evidence=[_EV])])[0]
    assert s.task_type == "provenance"
    assert s.difficulty == "factual"
    assert s.evidence == (_EV,)
    assert s.expected_answer == _CITE
    assert s.question == (f"What evidence supports that the owner of service "
                          f"{_K[:8]} is Payments Team?")


def test_one_scenario_per_scalar_property():
    kos = [_ko(_K, name="settlement", owner="Payments Team",
               nested={"x": 1}, evidence=[_EV])]
    scenarios = provenance_scenarios(kos)
    assert [s.scenario_id.split(":")[2] for s in scenarios] == ["name", "owner"]


def test_no_evidence_no_scenarios():
    assert provenance_scenarios([_ko(_K, owner="X")]) == []


def test_non_citable_evidence_skipped():
    bad = {"document_id": "x.md", "extractor": "mock"}  # no artifact/method
    assert provenance_scenarios([_ko(_K, owner="X", evidence=[bad])]) == []


def test_provenance_deterministic():
    kos = [_ko("b" * 32, owner="B", evidence=[_EV]),
           _ko("a" * 32, owner="A", evidence=[_EV])]
    assert provenance_scenarios(kos) == provenance_scenarios(list(reversed(kos)))


def test_provenance_query_is_the_factual_match():
    ko = _ko(_K, owner="Payments Team", evidence=[_EV])
    s = provenance_scenarios([ko])[0]
    assert build_queries(s, [ko]) == [
        'MATCH service WHERE owner == "Payments Team" RETURN owner']


# -- the answer generator ---------------------------------------------------

def test_build_answer_provenance_emits_real_evidence_ids():
    ko = _ko(_K, owner="Payments Team", evidence=[_EV])
    s = provenance_scenarios([ko])[0]
    ctx = {"entities": [], "facts": [], "relations": [], "evidence": [_EV]}
    assert build_answer(s, ctx) == {"answer": _CITE,
                                    "evidence_ids": [evidence_id(_EV)]}


def test_build_answer_refuses_evidence_absent_from_context():
    ko = _ko(_K, owner="Payments Team", evidence=[_EV])
    s = provenance_scenarios([ko])[0]
    ctx = {"entities": [], "facts": [], "relations": [], "evidence": []}
    assert build_answer(s, ctx) is None


# -- the validator ----------------------------------------------------------

def _example(evidence_ids=None, answer=None):
    return {
        "task": {"type": "provenance", "difficulty": "factual", "requires": []},
        "context": {"entities": [], "facts": [], "relations": [],
                    "evidence": [_EV]},
        "expected": {
            "answer": answer if answer is not None else _CITE,
            "koids": [],
            "evidence_ids": (evidence_ids if evidence_ids is not None
                             else [evidence_id(_EV)]),
        },
        "labels": {"grounded": True, "answerable": True, "ambiguous": False,
                   "contradictory": False},
    }


def test_validator_accepts_provenance_example():
    assert validate_grounding(_example()) == {"ok": True, "errors": []}


def test_validator_rejects_provenance_without_evidence_ids():
    out = validate_grounding(_example(evidence_ids=[]))
    assert out["ok"] is False
    assert any("no evidence" in e for e in out["errors"])


def test_validator_rejects_evidence_ids_absent_from_context():
    out = validate_grounding(_example(evidence_ids=["bogus"]))
    assert out["ok"] is False
    assert any("absent from context" in e for e in out["errors"])


def test_validator_rejects_empty_answer():
    out = validate_grounding(_example(answer=""))
    assert out["ok"] is False


# -- property: generator accepts ⇒ validator accepts ------------------------

_json = st.recursive(
    st.none() | st.booleans() | st.integers() | st.floats(allow_nan=False)
    | st.text(max_size=100),
    lambda children: st.lists(children, max_size=6)
    | st.dictionaries(st.text(max_size=20), children, max_size=6),
    max_leaves=30,
)


@st.composite
def _kos(draw):
    def a_ko():
        return {
            "koid": draw(st.text(min_size=8, max_size=8,
                                 alphabet=st.characters(
                                     min_codepoint=ord("a"),
                                     max_codepoint=ord("f")))),
            "type_name": draw(st.text(min_size=1, max_size=10,
                                      alphabet=st.characters(
                                          min_codepoint=ord("a"),
                                          max_codepoint=ord("z")))),
            "properties": draw(st.dictionaries(st.text(max_size=12),
                                               st.text(max_size=30), max_size=4)),
            "evidence": draw(st.lists(
                st.dictionaries(st.text(max_size=15), _json | st.none(),
                                max_size=6),
                max_size=3)),
        }

    return draw(st.lists(a_ko, min_size=1, max_size=4))


@given(kos=_kos())
def test_generator_accepts_implies_validator_accepts(kos):
    for s in provenance_scenarios(kos):
        ctx = {"entities": [], "facts": [], "relations": [],
               "evidence": list(s.evidence)}
        out = build_answer(s, ctx)
        if out is None:
            continue
        example = {
            "task": {"type": "provenance", "difficulty": "factual",
                     "requires": []},
            "context": ctx,
            "expected": {"answer": out["answer"], "koids": [],
                         "evidence_ids": out["evidence_ids"]},
            "labels": {"grounded": True, "answerable": True, "ambiguous": False,
                       "contradictory": False},
        }
        assert validate_grounding(example) == {"ok": True, "errors": []}


# -- live: real evidence over the wire --------------------------------------

def test_live_provenance_cites_real_server_evidence(mcp_server):
    """Real evidence end to end: the KO carries canonical evidence at the
    protocol boundary (extensions), the document compiles to a context
    whose rows carry the SAME evidence, and the generated example's
    evidence_ids point at the live rows."""
    host, token = mcp_server
    with aikoql.Agent.connect(host, token=token) as db:
        doc = db.remember("KnowledgeSnapshot", {"ir_json": json.dumps({
            "entities": [], "relations": [],
            "facts": [{
                "statement": "The settlement service is owned by the "
                             "Payments Team",
                "entities": ["SettlementService"], "confidence": 0.9,
                "evidence": _EV,
            }],
            "events": [], "temporal": [],
            "page_count": 1, "extractor": "mock-v1",
        })})
        koid = db.remember(
            "service", {"name": "settlement", "owner": "Payments Team"},
            extensions={"evidence": [_EV]},
        )["koid"]
        traced = db.trace(koid)
        assert [evidence_id(e) for e in traced["evidence"]] == [evidence_id(_EV)]

        kos = [db.get(koid)]
        kos[0]["evidence"] = traced["evidence"]
        scenarios = provenance_scenarios(kos)
        assert scenarios

        ctx = compile_context(db, doc["koid"], "who owns the settlement service")
        for s in scenarios:
            queries = build_queries(s, kos)
            assert queries, s.scenario_id
            for q in queries:
                env = db.aikoql(q)
                assert isinstance(env.get("results"), list), q
            assert verify_scenario(db, s, queries)["ok"], s.scenario_id
            out = build_answer(s, ctx)
            assert out is not None
            assert out["evidence_ids"] == [evidence_id(_EV)]
