"""T-10 RED: authorization scenarios (design Phase 9) — verdict
questions through the real ACL path.

A decision record is the kernel's own policy evaluation for a
(principal, action, resource_type) tuple: the verdict (allowed bool)
and, for denials, the kernel's reason ("Denied by policy: <koid>").
The generator pairs each decision with every KO of its resource type:
the question names the object, the answer is the machine-readable
verdict (ALLOWED:/DENIED:) preserving the reason verbatim — the engine
never re-derives a verdict, the oracle re-checks it live
(evaluate_policies). Unauthorized knowledge never reaches the dataset
context: a DENIED example's context may carry the decision fact and
nothing else that names the denied object.

Every test below fails against the current tree:
`aikoql_training.scenarios.authorization` does not exist.
"""

from __future__ import annotations

import aikoql

from aikoql_training.generators import build_answer, build_queries
from aikoql_training.models import validate as validate_schema
from aikoql_training.scenarios.authorization import authorization_scenarios
from aikoql_training.validation.grounding import evidence_id, validate_grounding
from conftest import make_example

_K = "a" * 32
_VERDICT_LABELS = {"grounded": True, "answerable": True,
                   "ambiguous": False, "contradictory": False}
_EV = {"document_id": "d1", "extractor": "e"}


def ko(koid, type_name="service", **props):
    return {"koid": koid, "type_name": type_name, "properties": dict(props)}


def _decision(**overrides):
    d = {"principal": "reader", "action": "read", "resource_type": "service",
         "allowed": True, "reason": "allowed"}
    d.update(overrides)
    return d


# -- the generator ----------------------------------------------------------

def test_allowed_decision_question_and_answer():
    [s] = authorization_scenarios(
        [ko(_K, name="settlement", owner="Payments Team")], [_decision()])
    assert s.task_type == "authorization"
    assert s.difficulty == "factual"
    assert s.question == "May reader read the service whose name is settlement?"
    assert s.expected_answer == (
        "ALLOWED: reader may read the service whose name is settlement")
    assert s.koids == (_K,)
    assert s.anchor_prop == "name"
    assert s.anchor_value == "settlement"
    assert s.subject == "reader"
    assert s.action == "read"
    assert s.type_name == "service"


def test_denied_decision_preserves_the_kernel_reason():
    [s] = authorization_scenarios(
        [ko(_K, name="settlement")],
        [_decision(allowed=False, reason="Denied by policy: " + "c" * 32)])
    assert s.question == "May reader read the service whose name is settlement?"
    assert s.expected_answer == (
        "DENIED: reader may not read the service whose name is settlement "
        "(Denied by policy: " + "c" * 32 + ")")


def test_decision_over_a_type_with_no_kos_is_skipped():
    assert authorization_scenarios([], [_decision()]) == []
    assert authorization_scenarios(
        [ko(_K, type_name="team", name="x")], [_decision()]) == []


def test_malformed_decisions_skipped():
    kos = [ko(_K, name="settlement")]
    out = authorization_scenarios(kos, [
        {"principal": "reader", "action": "read", "resource_type": "service",
         "reason": "allowed"},  # verdict missing
        _decision(allowed="yes"),  # not a bool
        _decision(principal=""),  # empty principal
        _decision(action="frobnicate"),  # not a kernel action
        _decision(allowed=False, reason=""),  # denial without a reason
    ])
    assert out == []


def test_authorization_deterministic():
    kos = [ko(_K, name="settlement"), ko("b" * 32, name="checkout")]
    decisions = [_decision(),
                 _decision(allowed=False, reason="Denied by policy: " + "c" * 32)]
    assert authorization_scenarios(kos, decisions) == authorization_scenarios(
        list(reversed(kos)), list(reversed(decisions)))


# -- the query builder ------------------------------------------------------

def test_authorization_query_anchors_the_object():
    kos = [ko(_K, name="settlement")]
    [s] = authorization_scenarios(kos, [_decision()])
    assert build_queries(s, kos) == [
        'MATCH service WHERE name == "settlement" RETURN name']


# -- the answer generator ---------------------------------------------------

def test_build_answer_grounds_on_the_decision_fact():
    kos = [ko(_K, name="settlement")]
    [s] = authorization_scenarios(kos, [_decision()])
    ctx = {"entities": [], "relations": [], "evidence": [_EV],
           "facts": [{"statement": "Policy decision: " + s.expected_answer,
                      "evidence": _EV}]}
    assert build_answer(s, ctx) == {
        "answer": s.expected_answer,
        "evidence_ids": [evidence_id(_EV)],
        "claims": [{"statement": "Policy decision: " + s.expected_answer,
                    "evidence_ids": [evidence_id(_EV)]}],
        "labels": _VERDICT_LABELS,
    }


def test_build_answer_refuses_untraced_verdict():
    kos = [ko(_K, name="settlement")]
    [s] = authorization_scenarios(kos, [_decision()])
    ctx = {"entities": [], "facts": [], "relations": [], "evidence": []}
    assert build_answer(s, ctx) is None


# -- the validator ----------------------------------------------------------

def _example(decision=None, facts=None, labels=None, policy=None, answer=None):
    decision = decision or _decision()
    kos = [ko(_K, name="settlement")]
    [s] = authorization_scenarios(kos, [decision])
    return make_example(
        task={"type": "authorization", "difficulty": "factual", "requires": []},
        input={"question": s.question},
        query_target={"language": "aikoql",
                      "query": 'MATCH service WHERE name == "settlement" '
                               "RETURN name"},
        context={
            "entities": [], "relations": [], "evidence": [_EV],
            "facts": facts or [
                {"statement": "Policy decision: " + s.expected_answer,
                 "evidence": _EV},
            ],
        },
        expected={"answer": answer if answer is not None else s.expected_answer,
                  "koids": list(s.koids),
                  "evidence_ids": [evidence_id(_EV)]},
        policy=policy or {"authorization_required": True,
                          "subject": s.subject, "action": s.action},
        labels=labels or _VERDICT_LABELS,
    )


def test_validator_accepts_grounded_verdict():
    assert validate_grounding(_example()) == {"ok": True, "errors": []}


def test_validator_rejects_non_verdict_answer():
    out = validate_grounding(_example(
        answer="Maybe", facts=[{"statement": "Maybe", "evidence": _EV}]))
    assert out["ok"] is False
    assert any("ALLOWED" in e or "DENIED" in e for e in out["errors"])


def test_validator_rejects_authorization_without_the_policy_flag():
    out = validate_grounding(_example(policy={"authorization_required": False}))
    assert out["ok"] is False
    assert any("authorization_required" in e for e in out["errors"])


def test_validator_rejects_ungrounded_verdict():
    out = validate_grounding(
        _example(labels={**_VERDICT_LABELS, "grounded": False},
                 facts=[]))
    assert out["ok"] is False


def test_validator_rejects_context_that_leaks_the_denied_object():
    # a DENIED example whose context carries the object's content (not
    # just the decision fact) leaks unauthorized knowledge — fail closed
    denied = _decision(allowed=False, reason="Denied by policy: " + "c" * 32)
    [s] = authorization_scenarios([ko(_K, name="settlement")], [denied])
    ex = _example(decision=denied, facts=[
        {"statement": "Policy decision: " + s.expected_answer, "evidence": _EV},
        {"statement": "The settlement service is owned by the Payments Team",
         "evidence": _EV},
    ])
    out = validate_grounding(ex)
    assert out["ok"] is False
    assert any("leak" in e for e in out["errors"])


def test_schema_accepts_authorization_type():
    validate_schema(_example())


# -- live: the real ACL path -------------------------------------------------

def test_live_authorization_runs_through_the_real_acl(mcp_server):
    """Real ACL end to end: a Deny policy deployed, the kernel's own
    evaluation denied and allowed, scenarios emitted from those
    verdicts, and the oracle re-checking the live policy engine.
    Unauthorized knowledge never reaches the context: the denied
    example's context is the decision fact only."""
    import json as _json

    from aikoql_training.context import compile_context
    from aikoql_training.validation import verify_scenario

    host, token = mcp_server
    with aikoql.Agent.connect(host, token=token) as db:
        koid = db.remember("service", {"name": "settlement",
                                       "owner": "Payments Team"})["koid"]
        # the policy KO's action property is the enum's Debug spelling
        # (kom.rs Action: "Read"/"Write"/...) — kernel.rs evaluate_policies
        # compares it to format!("{:?}", action); lowercase never matches.
        # Default-deny: an ALLOWED verdict needs an explicit Allow policy.
        db._backend.call_tool("deploy_policy", {
            "name": "reader-deny-service", "effect": "Deny",
            "principal": "reader", "action": "Read", "resource_type": "service",
        })
        db._backend.call_tool("deploy_policy", {
            "name": "admin-allow-service", "effect": "Allow",
            "principal": "admin", "action": "Read", "resource_type": "service",
        })
        denied = db._backend.call_tool("evaluate_policies", {
            "principal": "reader", "action": "read", "resource_type": "service",
        })
        allowed = db._backend.call_tool("evaluate_policies", {
            "principal": "admin", "action": "read", "resource_type": "service",
        })
        assert denied["allowed"] is False
        assert denied["reason"].startswith("Denied by policy:")
        assert allowed["allowed"] is True
        decisions = [
            {"principal": "reader", "action": "read", "resource_type": "service",
             "allowed": denied["allowed"], "reason": denied["reason"]},
            {"principal": "admin", "action": "read", "resource_type": "service",
             "allowed": allowed["allowed"], "reason": allowed["reason"]},
        ]
        kos = [db.get(koid)]
        scenarios = authorization_scenarios(kos, decisions)
        assert len(scenarios) == 2
        ev = {"document_id": "policy.md", "extractor": "mock-v1",
              "confidence": 0.75}
        doc = db.remember("KnowledgeSnapshot", {"ir_json": _json.dumps({
            "entities": [], "relations": [],
            "facts": [
                {"statement": "Policy decision: " + scenarios[0].expected_answer,
                 "entities": ["Settlement"], "confidence": 0.9, "evidence": ev},
                {"statement": "Policy decision: " + scenarios[1].expected_answer,
                 "entities": ["Settlement"], "confidence": 0.9, "evidence": ev},
            ],
            "events": [], "temporal": [],
            "page_count": 1, "extractor": "mock-v1",
        })})
        ctx = compile_context(db, doc["koid"],
                              "may reader read the settlement service")
        assert ctx["facts"]
        assert ctx["evidence"]
        for s in scenarios:
            queries = build_queries(s, kos)
            assert queries, s.scenario_id
            env = db.aikoql(queries[0])
            assert any(r.get("koid") == koid for r in env.get("results", []))
            # the oracle re-checks the verdict against the live policy
            # engine — the scenario never asserts what the ACL denies
            assert verify_scenario(db, s, queries)["ok"], s.scenario_id
            out = build_answer(s, ctx)
            assert out is not None
            assert out["labels"]["grounded"] is True
        # unauthorized knowledge never reaches the context: a content
        # fact about the denied object is a leak the validator rejects
        denied_scenario = next(s for s in scenarios
                               if s.expected_answer.startswith("DENIED:"))
        ex = make_example(
            task={"type": "authorization", "difficulty": "factual",
                  "requires": []},
            input={"question": denied_scenario.question},
            query_target={"language": "aikoql",
                          "query": queries[0]},
            context={"entities": [], "relations": [], "evidence": [],
                     "facts": [
                         {"statement": "Policy decision: "
                                       + denied_scenario.expected_answer,
                          "evidence": ev},
                         {"statement": "The settlement service is owned by "
                                       "the Payments Team",
                          "evidence": ev},
                     ]},
            expected={"answer": denied_scenario.expected_answer,
                      "koids": list(denied_scenario.koids),
                      "evidence_ids": [evidence_id(ev)]},
            policy={"authorization_required": True},
            labels=_VERDICT_LABELS,
        )
        assert validate_grounding(ex)["ok"] is False
