"""Shared duck-typing over the two aikoql python client shapes (embedded
`aikoql` and the MCP `McpClient`) for the framework adapters.

The embedded client takes positional (subject-first) args and returns plain
lists; the MCP client takes kwargs and wraps rows in {"results": [...]}.
"""

from __future__ import annotations

from typing import Any


def _is_mcp(client: Any) -> bool:
    return hasattr(client, "call_tool")


def remember(
    client: Any,
    subject: str,
    type_name: str,
    properties: dict[str, Any],
    semantic: dict[str, Any] | None = None,
) -> dict[str, Any]:
    if _is_mcp(client):
        return client.remember(
            subject=subject,
            type_name=type_name,
            properties=properties,
            **({"semantic": semantic} if semantic else {}),
        )
    return client.remember(subject, type_name, properties, semantic=semantic)


def recall_properties(
    client: Any, subject: str, text: str, k: int, type_name: str
) -> list[dict[str, Any]]:
    """Top-k text-relevant KO property dicts (text-only fusion).

    shortcut: the MCP hits carry koid-only (one get per hit, N+1) — k is
    tiny for memory recall; upgrade if the recall width ever grows.
    """
    if _is_mcp(client):
        res = client.find_similar(
            text=text,
            k=k,
            fusion="text",  # the MCP spelling; "text_only" is embedded-only
            type_name=type_name,
            subject=subject,
            wait_for_freshness_ms=2000,
        )
        out = []
        for hit in res.get("results", []):
            ko = client.get(hit["koid"], subject=subject)
            if ko.get("properties"):
                out.append(ko["properties"])
        return out
    hits = client.find_similar(subject, text=text, k=k, fusion="text_only")
    return [h["ko"]["properties"] for h in hits]


def enumerate_rows(client: Any, subject: str, type_name: str) -> list[dict[str, Any]]:
    """Every visible KO row of `type_name` (dicts with koid + properties)."""
    rows = client.aikoql(f"MATCH {type_name} RETURN *", subject=subject)
    if isinstance(rows, dict):  # MCP clients wrap rows in {"results": [...]}
        rows = rows.get("results", [])
    return rows


def forget(client: Any, subject: str, koid: str) -> None:
    if _is_mcp(client):
        client.forget(koid=koid, subject=subject)
    else:
        client.forget(subject, koid)
