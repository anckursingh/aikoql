"""The training side's AIKOQL boundary — the SDK/MCP public surface only.

One function today: capture the dataset snapshot from a live agent
through the public health tool. Context/auth conveniences grow here
(T-06/T-10), never in snapshot.py — the snapshot module stays pure.
"""

from __future__ import annotations

from typing import Any, Dict, Optional

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
