"""T-23 RED: grounding mutation fuzz (FZ-07) — every mutation of a
valid claims-carrying example that breaks the claim/evidence
relationship must fail validate_grounding; mutations that only touch
grounding-external fields (koids, entity names, relations) must keep
validating — those are the leakage/scenario-match gates' fields, and
grounding rejecting them would double-count.

The review's property, applied to every axis it lists (answer, fact
statement, evidence, evidence_id, KOID, entity name, relation, label):

    mutate(x) != original
        AND
    mutation removes its supporting evidence
        =>
    validate_grounding == FAIL

The registered matrix below is the deterministic teeth; the two
Hypothesis properties generalize the answer and fact-statement axes
over arbitrary strings.

Every test below fails against the current tree: a grounded answer
labeled answerable=False — and one labeled ambiguous — validate ok.
"""

from __future__ import annotations

from hypothesis import given
from hypothesis import strategies as st

from aikoql_training.validation.grounding import evidence_id, validate_grounding

_FACT = "The settlement service is owned by the Payments Team"
_FACT2 = "Settlement batches nightly"
_EV = {"document_id": "acmepay.md", "extractor": "mock-v1", "confidence": 0.9}
_EV2 = {"document_id": "acmepay.md", "extractor": "mock-v1", "confidence": 0.7}
_EID, _EID2 = evidence_id(_EV), evidence_id(_EV2)


def _fact(statement, evidence=None, entities=("SettlementService",)):
    row = {"statement": statement, "entities": list(entities)}
    if evidence is not None:
        row["evidence"] = evidence
    return row


def _claim(statement=_FACT, evidence=_EV):
    return {"statement": statement, "evidence_ids": [evidence_id(evidence)]}


def _base():
    """A valid claims-carrying grounded example (the T-22 shape)."""
    return {
        "context": {
            "entities": [], "relations": [],
            "facts": [_fact(_FACT, _EV)],
            "evidence": [_EV],
        },
        "expected": {"answer": "Payments Team", "koids": ["k1"],
                     "evidence_ids": [_EID], "claims": [_claim()]},
        "labels": {"grounded": True, "answerable": True,
                   "ambiguous": False, "contradictory": False},
    }


# (name, mutate(ex), expect_ok, error_substr) — substr "" pins nothing.
_MUTANTS = [
    # -- answer ---------------------------------------------------------
    ("answer no longer in any fact",
     lambda ex: ex["expected"].__setitem__("answer", "Vendors Inc."),
     False, "trace"),
    # -- fact statement -------------------------------------------------
    ("fact statement loses the answer",
     lambda ex: ex["context"]["facts"][0].__setitem__("statement",
                                                      "Settlement batches nightly"),
     False, ""),
    ("fact statement paraphrased but claim stale",
     lambda ex: ex["context"]["facts"][0].__setitem__(
         "statement", "The Payments Team owns the settlement service"),
     False, "claim"),
    # -- evidence -------------------------------------------------------
    ("fact evidence absent from context",
     lambda ex: ex["context"]["facts"][0].__setitem__("evidence", _EV2),
     False, "evidence"),
    ("context evidence row removed",
     lambda ex: ex["context"].__setitem__("evidence", []),
     False, "evidence"),
    ("context evidence row mutated",
     lambda ex: ex["context"].__setitem__(
         "evidence", [{"document_id": "acmepay.md", "extractor": "mock-v1",
                       "confidence": 0.99}]),
     False, "evidence"),
    ("fact+context evidence mutated in lockstep, ids stale",
     lambda ex: (ex["context"]["facts"][0].__setitem__("evidence", _EV2),
                 ex["context"].__setitem__("evidence", [_EV2])),
     False, "evidence_ids"),
    # -- evidence_id ----------------------------------------------------
    ("expected evidence id forged",
     lambda ex: ex["expected"].__setitem__("evidence_ids", [_EID2]),
     False, "evidence_ids"),
    ("claim cites a forged evidence id",
     lambda ex: ex["expected"]["claims"][0].__setitem__(
         "evidence_ids", [_EID2]),
     False, "forged"),
    # -- entity name / relation / KOID (grounding-external: accept) -----
    ("claim statement dangles",
     lambda ex: ex["expected"]["claims"][0].__setitem__(
         "statement", "Settlement batches nightly"),
     False, "trace"),
    ("claim swapped onto a non-supporting fact",
     lambda ex: (ex["context"]["facts"].append(_fact(_FACT2, _EV2)),
                 ex["context"]["evidence"].append(_EV2),
                 ex["expected"].__setitem__("claims", [_claim(_FACT2, _EV2)]),
                 ex["expected"].__setitem__("evidence_ids", [_EID2])),
     False, "covered"),
    ("claim entry malformed",
     lambda ex: ex["expected"].__setitem__("claims", ["not a dict"]),
     False, "malformed"),
    ("claim carries no evidence ids",
     lambda ex: ex["expected"]["claims"][0].__setitem__("evidence_ids", []),
     False, "no evidence ids"),
    ("claims is not a list",
     lambda ex: ex["expected"].__setitem__("claims", _claim()),
     False, "must be a list"),
    ("empty claim decomposition",
     lambda ex: ex["expected"].__setitem__("claims", []),
     False, "covered"),
    # -- label ----------------------------------------------------------
    ("grounded label flipped",
     lambda ex: ex["labels"].__setitem__("grounded", False),
     False, "grounded"),
    ("grounded answer labeled unanswerable",
     lambda ex: ex["labels"].__setitem__("answerable", False),
     False, "answerable"),
    ("grounded answer labeled ambiguous",
     lambda ex: ex["labels"].__setitem__("ambiguous", True),
     False, "ambiguous"),
    # -- grounding-external fields: the boundary pins -------------------
    ("expected koid swapped",
     lambda ex: ex["expected"].__setitem__("koids", ["k2"]),
     True, ""),
    ("fact entity name swapped",
     lambda ex: ex["context"]["facts"][0].__setitem__(
         "entities", ["OtherService"]),
     True, ""),
    ("context relation added",
     lambda ex: ex["context"].__setitem__(
         "relations", [{"from": "a", "to": "b", "type": "r"}]),
     True, ""),
    ("non-supporting fact added",
     lambda ex: (ex["context"]["facts"].append(_fact(_FACT2, _EV2)),
                 ex["context"]["evidence"].append(_EV2)),
     True, ""),
    ("claim ids duplicated",
     lambda ex: ex["expected"]["claims"][0].__setitem__(
         "evidence_ids", [_EID, _EID]),
     True, ""),
]


def test_every_mutant_meets_expectation():
    mismatches = []
    for name, mutate, expect_ok, substr in _MUTANTS:
        ex = _base()
        mutate(ex)
        out = validate_grounding(ex)
        if bool(out["ok"]) != expect_ok:
            mismatches.append((name, "ok" if expect_ok else "reject", out))
        elif not expect_ok and substr and \
                not any(substr in e for e in out["errors"]):
            mismatches.append((name, f"reject via {substr!r}", out))
    assert not mismatches, mismatches


# -- properties: the review's headline axis --------------------------------
#
# A mutation that removes the answer's supporting evidence must fail;
# one that keeps it may validate (answer identity is the oracle's job,
# not grounding's — the substring trace is the deterministic ceiling).

@given(answer=st.text(max_size=80))
def test_answer_mutation_removing_support_fails_closed(answer):
    if answer == "Payments Team":
        return  # unchanged
    ex = _base()
    ex["expected"]["answer"] = answer
    if any(answer in f["statement"]
           for f in ex["context"]["facts"]
           if isinstance(f.get("statement"), str)):
        return  # still supported by a fact: grounding may accept
    assert validate_grounding(ex)["ok"] is False


@given(statement=st.text(max_size=80))
def test_fact_statement_mutation_fails_closed(statement):
    if statement == _FACT:
        return  # unchanged
    ex = _base()
    ex["context"]["facts"][0]["statement"] = statement
    # claims still name the original statement: any mutation either
    # removes the answer's support or dangles the claim
    assert validate_grounding(ex)["ok"] is False
