"""The training side's AIKOQL boundary — the SDK/MCP public surface only.

One function today: capture the dataset snapshot from a live agent
through the public health tool. Context/auth conveniences grow here
(T-06/T-10), never in snapshot.py — the snapshot module stays pure.
"""

from __future__ import annotations

from typing import Any, Dict, List, Optional

from aikoql_training.snapshot import Snapshot, capture_snapshot


def capture_from_agent(
    agent,
    database_id: str,
    *,
    config: Optional[Dict[str, Any]] = None,
    source_manifest: Any = None,
    seed: int = 0,
    now: Optional[str] = None,
) -> Snapshot:
    """Capture a snapshot over the real public interface: the agent's
    health() call (tool_health on MCP)."""
    health = agent.health()
    return capture_snapshot(
        database_id,
        health,
        config=config,
        source_manifest=source_manifest,
        seed=seed,
        now=now,
    )


def scan_edges(agent, koids) -> List[dict]:
    """Recover the live relation edges for the given KOs through the
    public traverse surface. Returns deduplicated, normalized
    {"from", "rel", "to"} dicts — an edge reached from either endpoint
    appears once."""
    seen = set()
    edges = []
    for koid in koids:
        result = agent.traverse(koid, None, 1)
        hits = result.get("hits", []) if isinstance(result, dict) else result
        for h in hits:
            if h.get("direction") == "inbound":
                edge = (h["koid"], h["rel_type"], koid)
            else:
                edge = (koid, h["rel_type"], h["koid"])
            if edge in seen:
                continue
            seen.add(edge)
            edges.append({"from": edge[0], "rel": edge[1], "to": edge[2]})
    return edges
