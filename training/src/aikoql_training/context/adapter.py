"""T-06: the context adapter — the Context Compiler through the public client.

The server's `compile_context` tool ranks, filters and assembles the
ContextPackage; this module only maps the envelope into the schema's
context shape (entities / facts / relations / evidence) and collects
the facts' evidence dicts (deduped, package order). No Python
retrieval logic — the arch assertion.
"""

from __future__ import annotations

import json


def compile_context(db, document_koid, task, *, token_budget=2000, subject=None):
    """Compile the schema-shaped context for `task` against the knowledge
    document at `document_koid` (a KO carrying ir_json, or an ingested
    document). Returns {entities, facts, relations, evidence}: the
    package rows verbatim plus the deduped evidence of the ranked facts.

    Server errors (e.g. ACCESS_DENIED for an unauthorized identity)
    propagate — never masked as empty rows.
    """
    args = {"koid": document_koid, "task": task, "token_budget": token_budget}
    if subject is not None:
        args["subject"] = subject

    package = _call_tool(db, "compile_context", args).get("package", {})

    evidence = []
    seen = set()
    for fact in package.get("facts", []):
        ev = fact.get("evidence")
        if not ev:
            continue
        key = json.dumps(ev, sort_keys=True)
        if key in seen:
            continue
        seen.add(key)
        evidence.append(ev)

    return {
        "entities": package.get("entities", []),
        "facts": package.get("facts", []),
        "relations": package.get("relations", []),
        "evidence": evidence,
    }


def _call_tool(db, name, args):
    """The generic tool path: McpClient.call_tool directly, or an Agent's
    MCP backend. Anything without a tool surface (embedded, plain
    objects) is rejected — the compiler is server-surface only."""
    call = getattr(db, "call_tool", None)
    if call is None:
        call = getattr(getattr(db, "_backend", None), "call_tool", None)
    if call is None:
        raise NotImplementedError(
            "compile_context requires the MCP tool surface — "
            "connect with Agent.connect('host:port')"
        )
    return call(name, args)
