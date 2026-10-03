"""T-07 RED: grounded answers + grounding validator (design Phase 12).

The answer generator certifies a scenario's expected answer against the
compiled context: the claim must trace to a fact statement, and every
supporting fact's evidence must be present in context — otherwise the
example is REFUSED (None), never emitted ungrounded. The validator
enforces the same claim→context→evidence tracing on a schema-shaped
example and fails closed on label/evidence_ids inconsistency. 100% of
accepted examples grounded (the §26 gate).

Every test below fails against the current tree:
`aikoql_training.generators.answer` and
`aikoql_training.validation.grounding` do not exist.
"""

from __future__ import annotations

import json

import aikoql
from hypothesis import given
from hypothesis import strategies as st

from aikoql_training.context import compile_context
from aikoql_training.scenarios.scenario import Scenario
from aikoql_training.generators.answer import build_answer
from aikoql_training.validation.grounding import evidence_id, validate_grounding


_EV = {"document_id": "acmepay.md", "extractor": "mock-v1", "confidence": 0.9}
_EV2 = {"document_id": "acmepay.md", "extractor": "mock-v1", "confidence": 0.7}


def _fact(statement, evidence=None):
    row = {"statement": statement, "entities": ["SettlementService"],
           "score": 0.8, "justification": "task match"}
    if evidence is not None:
        row["evidence"] = evidence
    return row


def _scenario(answer="Payments Team", **kw):
    base = dict(
        scenario_id="s-1", task_type="grounded_qa", difficulty="factual",
        question="Who owns the settlement service?",
        expected_answer=answer, koids=("k1",),
    )
    base.update(kw)
    return Scenario(**base)


def _context(facts, evidence):
    return {"entities": [], "facts": facts, "relations": [], "evidence": evidence}


# -- unit: the answer generator ----------------------------------------------

def test_answer_grounds_on_evidenced_fact():
    ctx = _context(
        [_fact("The settlement service is owned by the Payments Team", _EV)],
        [_EV],
    )
    out = build_answer(_scenario(), ctx)
    assert out == {"answer": "Payments Team",
                   "evidence_ids": [evidence_id(_EV)],
                   "labels": {"grounded": True, "answerable": True,
                              "ambiguous": False, "contradictory": False}}


def test_answer_refuses_unsupported_claim():
    # No fact statement contains the answer -> the claim is unsupported.
    ctx = _context([_fact("Settlement batches nightly", _EV)], [_EV])
    assert build_answer(_scenario(), ctx) is None


def test_answer_refuses_when_evidence_absent_from_context():
    # The supporting fact carries evidence, but the context has no
    # evidence rows -> required evidence absent, the example is refused.
    ctx = _context(
        [_fact("The settlement service is owned by the Payments Team", _EV)],
        [],
    )
    assert build_answer(_scenario(), ctx) is None


def test_answer_refuses_when_fact_has_no_evidence():
    # A supporting fact without evidence is an unbacked claim.
    ctx = _context([_fact("The settlement service is owned by the Payments Team")], [])
    assert build_answer(_scenario(), ctx) is None


def test_answer_collects_evidence_ids_in_package_order_deduped():
    ctx = _context(
        [
            _fact("The settlement service is owned by the Payments Team", _EV),
            _fact("Payments Team runs the settlement service", _EV2),
            _fact("Payments Team also runs billing", _EV),
        ],
        [_EV, _EV2],
    )
    out = build_answer(_scenario(), ctx)
    assert out["evidence_ids"] == [evidence_id(_EV), evidence_id(_EV2)]


def test_answer_is_deterministic():
    ctx = _context(
        [_fact("The settlement service is owned by the Payments Team", _EV)],
        [_EV],
    )
    assert build_answer(_scenario(), ctx) == build_answer(_scenario(), ctx)


# -- unit: the validator ------------------------------------------------------

def _example(answer="Payments Team", facts=None, evidence=None,
             evidence_ids=None, grounded=True):
    if facts is None:
        facts = [_fact("The settlement service is owned by the Payments Team", _EV)]
    if evidence is None:
        evidence = [_EV]
    if evidence_ids is None:
        evidence_ids = [evidence_id(_EV)]
    return {
        "context": _context(facts, evidence),
        "expected": {"answer": answer, "koids": ["k1"], "evidence_ids": evidence_ids},
        "labels": {"grounded": grounded, "answerable": True,
                   "ambiguous": False, "contradictory": False},
    }


def test_validator_accepts_grounded_example():
    out = validate_grounding(_example())
    assert out == {"ok": True, "errors": []}


def test_validator_rejects_answer_not_in_any_fact():
    out = validate_grounding(_example(answer="Vendors Inc."))
    assert not out["ok"]
    assert any("trace" in e for e in out["errors"])


def test_validator_rejects_fact_evidence_missing_from_context():
    out = validate_grounding(
        _example(evidence=[])  # the supporting fact's evidence is absent
    )
    assert not out["ok"]
    assert any("evidence" in e for e in out["errors"])


def test_validator_rejects_evidence_ids_not_tracing():
    out = validate_grounding(_example(evidence_ids=["bogus-id"]))
    assert not out["ok"]
    assert any("evidence_ids" in e for e in out["errors"])


def test_validator_rejects_grounded_label_on_ungrounded_claim():
    out = validate_grounding(_example(answer="Vendors Inc."))
    assert not out["ok"]
    assert any("grounded" in e for e in out["errors"])


def test_validator_rejects_ungrounded_label_on_grounded_claim():
    out = validate_grounding(_example(grounded=False))
    assert not out["ok"]
    assert any("grounded" in e for e in out["errors"])


def test_validator_rejects_ungrounded_example_carrying_evidence_ids():
    out = validate_grounding(_example(answer="", grounded=False,
                                      evidence_ids=[evidence_id(_EV)]))
    assert not out["ok"]


# -- property: generator accepts => validator accepts ------------------------

_json_safe = st.recursive(
    st.none() | st.booleans() | st.integers() | st.floats(allow_nan=False)
    | st.text(max_size=200),
    lambda children: st.lists(children, max_size=8)
    | st.dictionaries(st.text(max_size=40), children, max_size=8),
    max_leaves=40,
)


@st.composite
def _fact_rows(draw):
    rows = draw(st.lists(
        st.dictionaries(
            st.text(max_size=30),
            _json_safe | st.none(),
            max_size=8,
        ),
        max_size=5,
    ))
    # Keep only rows whose statement (if present) is a str — the real
    # package shape; anything else is a malformed package, not grounding.
    return [r for r in rows if isinstance(r.get("statement"), str)]


@given(
    rows=_fact_rows(),
    answer=st.text(min_size=1, max_size=60).filter(lambda a: a.strip()),
)
def test_generator_accepts_implies_validator_accepts(rows, answer):
    evidence = []
    seen = set()
    for row in rows:
        ev = row.get("evidence")
        if isinstance(ev, dict):
            key = json.dumps(ev, sort_keys=True)
            if key not in seen:
                seen.add(key)
                evidence.append(ev)
    ctx = _context(rows, evidence)
    out = build_answer(_scenario(answer=answer), ctx)
    if out is None:
        return  # refused: no example is emitted, nothing to validate
    example = _example(
        answer=out["answer"], facts=rows, evidence=evidence,
        evidence_ids=out["evidence_ids"], grounded=True,
    )
    assert validate_grounding(example) == {"ok": True, "errors": []}


# -- live: the answer traces to compiled evidence -----------------------------

def test_live_answer_traces_to_compiled_evidence(mcp_server):
    """The full chain over the spawned server: a knowledge document whose
    fact answers the scenario compiles to a context whose evidence backs
    the generated answer (the T-06 live pattern + grounding)."""
    ir = {
        "entities": [
            {"name": "SettlementService", "type_hint": "service",
             "mentions": ["settles payments"], "confidence": 0.9,
             "evidence": _EV},
        ],
        "relations": [],
        "facts": [
            {"statement": "The settlement service is owned by the Payments Team",
             "entities": ["SettlementService"], "confidence": 0.9,
             "evidence": _EV},
        ],
        "events": [], "temporal": [],
        "page_count": 1, "extractor": "mock-v1",
    }
    host, token = mcp_server
    with aikoql.Agent.connect(host, token=token) as db:
        doc = db.remember("KnowledgeSnapshot", {"ir_json": json.dumps(ir)})
        ctx = compile_context(db, doc["koid"], "who owns the settlement service")

        out = build_answer(_scenario(), ctx)
        assert out is not None
        assert out["answer"] == "Payments Team"
        ctx_ids = [evidence_id(e) for e in ctx["evidence"]]
        assert out["evidence_ids"] and all(i in ctx_ids for i in out["evidence_ids"])
