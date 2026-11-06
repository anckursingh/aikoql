"""AG-60: OpenAI Agents SDK session backed by aikoql.

Drop-in for the upstream example (the docs' Runner)::

    Runner.run(agent, "hi", session=AikoqlSession(session_id="...", client=...))

One KO per conversation item (JSON). get_items returns the latest N in
chronological order, like the SDK's own in-memory session. The async
methods offload the sync aikoql I/O (both clients are sync) to a worker
thread via asyncio.to_thread, like langgraph.py — the Runner awaits them
on the event loop.
"""

from __future__ import annotations

import asyncio
import json
from typing import Any, Optional

from ._client import enumerate_rows, forget, remember


class AikoqlSession:
    """The Session protocol (session_id/session_settings/get_items/add_items/
    pop_item/clear_session) over aikoql."""

    _TYPE = "openai_agents_session_item"
    session_settings = None  # the SDK 0.24 surface we don't set

    def __init__(
        self,
        session_id: str,
        client: Any,
        subject: Optional[str] = None,
    ) -> None:
        self.session_id = session_id
        self.client = client
        self.subject = subject or f"openai-agents:{session_id}"
        self._next_seq: Optional[int] = None

    def _items(self) -> list[tuple[int, dict[str, Any]]]:
        """(seq, item) pairs, chronological."""
        pairs = []
        for row in enumerate_rows(self.client, self.subject, self._TYPE):
            raw = row.get("properties", {}).get("item_json")
            if raw:
                pairs.append(
                    (int(row.get("properties", {}).get("seq") or 0), json.loads(raw))
                )
        pairs.sort(key=lambda p: p[0])
        return pairs

    def _seed_seq(self) -> int:
        # lazy: one MATCH on the first write of an instance
        if self._next_seq is None:
            pairs = self._items()
            self._next_seq = (pairs[-1][0] if pairs else 0) + 1
        return self._next_seq

    async def get_items(self, limit: Optional[int] = None) -> list[dict[str, Any]]:
        return await asyncio.to_thread(self._get_items_sync, limit)

    def _get_items_sync(self, limit: Optional[int] = None) -> list[dict[str, Any]]:
        items = [item for _, item in self._items()]
        if limit is not None:
            items = items[-limit:]
        return items

    async def add_items(self, items: list[dict[str, Any]]) -> None:
        await asyncio.to_thread(self._add_items_sync, items)

    def _add_items_sync(self, items: list[dict[str, Any]]) -> None:
        seq = self._seed_seq()
        for item in items:
            remember(self.client, self.subject, self._TYPE, {
                "seq": seq,
                "item_json": json.dumps(item),
            })
            seq += 1
        self._next_seq = seq

    async def pop_item(self) -> Optional[dict[str, Any]]:
        return await asyncio.to_thread(self._pop_item_sync)

    def _pop_item_sync(self) -> Optional[dict[str, Any]]:
        rows = enumerate_rows(self.client, self.subject, self._TYPE)
        if not rows:
            return None
        newest = max(rows, key=lambda r: int(r.get("properties", {}).get("seq") or 0))
        raw = newest.get("properties", {}).get("item_json")
        forget(self.client, self.subject, newest["koid"])
        self._next_seq = None  # re-seed lazily
        return json.loads(raw) if raw else None

    async def clear_session(self) -> None:
        await asyncio.to_thread(self._clear_session_sync)

    def _clear_session_sync(self) -> None:
        for row in enumerate_rows(self.client, self.subject, self._TYPE):
            forget(self.client, self.subject, row["koid"])
        self._next_seq = None
