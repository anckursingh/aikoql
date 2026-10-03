"""T-06 RED: context adapter (design Phase 11).

The adapter compiles the schema-shaped context package (entities /
facts / relations / evidence) from the server's Context Compiler —
the `compile_context` tool — through the public client. The arch
assertion: NO Python retrieval logic — the server ranks, filters and
assembles; the adapter only maps the envelope into the schema shape.
Every test below fails against the current tree:
`aikoql_training.context` does not exist.

Recon pins (against the T-06 pre-fix head):

- compile_context IS on the MCP surface (tool_registry.rs ->
  tools/agent_knowledge.rs) — the T-01 "no context tool" finding was
  wrong. Input {koid, task, token_budget?, subject?}; the koid names a
  KO carrying ir_json (or an ingested document's sha256).
- ACL is server-side: an unauthorized subject gets an ACCESS_DENIED
  error envelope, never rows (mcp_real_world CTX-001).
- Staleness is the IR-versioning boundary: the compiler reads the live
  KO's ir_json and its 5-min cache is fingerprint-keyed, so an updated
  document never serves its superseded facts (CTX-003).
- PRR-2 (session.rs inject_session_forced): TCP trust mode forces the
  token identity — per-call subject/roles are overwritten. The denied
  reader is therefore a second --tcp-token identity, not a subject arg.
- The Python SDK has no compile_context wrapper: McpClient.call_tool
  is the generic path (mcp_client.py), and tool errors raise McpError.
"""

from __future__ import annotations

import json

import pytest
from hypothesis import given
from hypothesis import strategies as st

from aikoql import Agent, McpError

from aikoql_training.context import compile_context


# -- seed IR (the ir_json a knowledge document carries) --------------------

def _evidence():
    return {"document_id": "acmepay.md", "extractor": "mock-v1", "confidence": 0.9}


def _ir_v1():
    return {
        "entities": [
            {"name": "PaymentService", "type_hint": "service",
             "mentions": ["processes payments"], "confidence": 0.9,
             "evidence": _evidence()},
            {"name": "SettlementService", "type_hint": "service",
             "mentions": ["settles payments"], "confidence": 0.8,
             "evidence": _evidence()},
        ],
        "relations": [
            {"subject": "PaymentService", "predicate": "depends_on",
             "object": "SettlementService", "confidence": 0.8,
             "evidence": _evidence()},
        ],
        "facts": [
            {"statement": "Payments flow through Stripe",
             "entities": ["PaymentService"], "confidence": 0.9,
             "evidence": _evidence()},
        ],
        "events": [], "temporal": [],
        "page_count": 1, "extractor": "mock-v1",
    }


def _ir_v2():
    ir = _ir_v1()
    ir["entities"] = ir["entities"][:1] + [
        {"name": "InternalLedger", "type_hint": "service",
         "mentions": ["internal payment ledger"], "confidence": 0.8,
         "evidence": _evidence()},
    ]
    ir["relations"] = [
        {"subject": "PaymentService", "predicate": "depends_on",
         "object": "InternalLedger", "confidence": 0.8,
         "evidence": _evidence()},
    ]
    ir["facts"] = [
        {"statement": "Payments flow through the internal ledger",
         "entities": ["PaymentService"], "confidence": 0.9,
         "evidence": _evidence()},
    ]
    return ir


# -- unit: the adapter over a fake tool surface ---------------------------

_ENV = {
    "context_markdown": "rendered",
    "package": {
        "entities": [
            {"name": "PaymentService", "type_hint": "service", "score": 0.9,
             "mentions": ["processes payments"], "justification": "task match"},
        ],
        "facts": [
            {"statement": "Payments flow through Stripe",
             "entities": ["PaymentService"], "score": 0.8,
             "justification": "task match",
             "evidence": {"document_id": "acmepay.md", "extractor": "mock-v1",
                          "confidence": 0.9}},
            {"statement": "Settlement batches nightly",
             "entities": ["SettlementService"], "score": 0.4,
             "justification": "task match",
             "evidence": {"document_id": "acmepay.md", "extractor": "mock-v1",
                          "confidence": 0.9}},
            {"statement": "Unbacked claim", "entities": [], "score": 0.1,
             "justification": "task match"},
        ],
        "relations": [
            {"subject": "PaymentService", "predicate": "depends_on",
             "object": "SettlementService", "score": 0.7,
             "justification": "task match"},
        ],
        "estimated_tokens": 42,
        "trimmed": False,
        "status": "healthy",
    },
    "koid": "k1",
    "task": "process payments",
    "token_budget": 2000,
    "semantic": False,
    "experiences": [],
}


class _FakeTool:
    """A client whose ONLY surface is call_tool — any retrieval call the
    adapter attempts (aikoql/traverse/find_similar/get) raises
    AttributeError, enforcing the arch assertion structurally."""

    def __init__(self, env):
        self._env = env
        self.calls = []

    def call_tool(self, name, args):
        self.calls.append((name, args))
        return self._env


def test_adapter_maps_package_rows_into_schema_shape():
    ctx = compile_context(_FakeTool(_ENV), "k1", "process payments")
    assert ctx["entities"] == _ENV["package"]["entities"]
    assert ctx["facts"] == _ENV["package"]["facts"]
    assert ctx["relations"] == _ENV["package"]["relations"]
    assert ctx["evidence"] == [
        {"document_id": "acmepay.md", "extractor": "mock-v1", "confidence": 0.9}
    ]


def test_adapter_collects_evidence_from_facts_and_dedupes():
    # Two facts carry the same evidence dict; a third carries none.
    ctx = compile_context(_FakeTool(_ENV), "k1", "process payments")
    assert ctx["evidence"] == [
        {"document_id": "acmepay.md", "extractor": "mock-v1", "confidence": 0.9}
    ]


def test_adapter_passes_koid_task_budget_and_subject():
    fake = _FakeTool(_ENV)
    compile_context(fake, "k1", "process payments", token_budget=1234, subject="alice")
    name, args = fake.calls[0]
    assert name == "compile_context"
    assert args == {"koid": "k1", "task": "process payments",
                    "token_budget": 1234, "subject": "alice"}


def test_adapter_defaults_budget_and_omits_subject():
    fake = _FakeTool(_ENV)
    compile_context(fake, "k1", "process payments")
    _, args = fake.calls[0]
    assert args["token_budget"] == 2000
    assert "subject" not in args


def test_adapter_is_deterministic():
    a = compile_context(_FakeTool(_ENV), "k1", "process payments")
    b = compile_context(_FakeTool(_ENV), "k1", "process payments")
    assert a == b


def test_adapter_uses_only_the_compiler_path():
    # The fake has NO aikoql/traverse/find_similar/get — the adapter must
    # not touch them (AttributeError = arch assertion violated).
    ctx = compile_context(_FakeTool(_ENV), "k1", "process payments")
    assert ctx["facts"]


def test_adapter_surfaces_server_errors_instead_of_rows():
    class _Denied(_FakeTool):
        def call_tool(self, name, args):
            self.calls.append((name, args))
            raise McpError(code="ACCESS_DENIED", message="denied")

    with pytest.raises(McpError):
        compile_context(_Denied(_ENV), "k1", "process payments")


def test_adapter_rejects_clients_without_a_tool_surface():
    with pytest.raises(NotImplementedError):
        compile_context(object(), "k1", "process payments")


@given(st.text(max_size=60))
def test_adapter_returns_schema_shape_for_any_task(task):
    ctx = compile_context(_FakeTool(_ENV), "k1", task)
    assert sorted(ctx) == ["entities", "evidence", "facts", "relations"]
    assert all(isinstance(ctx[k], list) for k in ctx)


# -- live: the real compiler over the spawned MCP server -------------------

def test_live_context_has_required_rows(mcp_server):
    """The 6-point context acceptance minus auth/staleness cells: the
    required entities/facts/relations + evidence are present in the
    compiled package over the spawned server."""
    host, token = mcp_server
    with Agent.connect(host, token=token) as db:
        doc = db.remember("KnowledgeSnapshot", {"ir_json": json.dumps(_ir_v1())})
        ctx = compile_context(db, doc["koid"], "process payments")

        names = [e["name"] for e in ctx["entities"]]
        assert "PaymentService" in names, names

        statements = [f["statement"] for f in ctx["facts"]]
        assert "Payments flow through Stripe" in statements, statements

        rels = [(r["subject"], r["predicate"], r["object"]) for r in ctx["relations"]]
        assert ("PaymentService", "depends_on", "SettlementService") in rels, rels

        assert any(e.get("document_id") == "acmepay.md" for e in ctx["evidence"])


def test_live_unauthorized_identity_gets_no_rows(mcp_server_two_tokens):
    """No unauthorized rows: a second token identity without a grant gets
    an ACCESS_DENIED error, never a context package (PRR-2 pins identity
    to the token on TCP, so the denied reader is a token, not a subject
    arg)."""
    srv = mcp_server_two_tokens
    with Agent.connect(srv["host"], token=srv["alice"]) as alice:
        doc = alice.remember("KnowledgeSnapshot", {"ir_json": json.dumps(_ir_v1())})
        koid = doc["koid"]

    with Agent.connect(srv["host"], token=srv["bob"]) as bob:
        with pytest.raises(McpError) as exc:
            compile_context(bob, koid, "process payments")
        assert "DENIED" in str(exc.value)


def test_live_superseded_knowledge_never_rides_in(mcp_server):
    """No stale rows: after the knowledge document moves to v2, the old
    fact is gone from the context and the new fact is present (the
    compiler reads the live ir_json; its cache is fingerprint-keyed)."""
    host, token = mcp_server
    with Agent.connect(host, token=token) as db:
        doc = db.remember("KnowledgeSnapshot", {"ir_json": json.dumps(_ir_v1())})
        koid = doc["koid"]

        ctx1 = compile_context(db, koid, "process payments")
        statements1 = [f["statement"] for f in ctx1["facts"]]
        assert "Payments flow through Stripe" in statements1, statements1

        db.remember("KnowledgeSnapshot", {"ir_json": json.dumps(_ir_v2())}, koid=koid)

        ctx2 = compile_context(db, koid, "process payments")
        statements2 = [f["statement"] for f in ctx2["facts"]]
        assert "Payments flow through Stripe" not in statements2, statements2
        assert "Payments flow through the internal ledger" in statements2, statements2
