"""LangGraph-native checkpoint/long-term memory adapter backed by Mnemosyne.

Implements the saver surface LangGraph expects (`get`, `put`, `list` and async
variants). Does not require LangGraph at import time.
"""

from __future__ import annotations

import asyncio
import copy
import json
from typing import Any, Iterator, Optional

from mnemosyne import Mnemosyne


class MnemosyneLangGraphSaver:
    """LangGraph-compatible checkpoint saver backed by Mnemosyne KOs."""

    def __init__(self, path: str, subject: str = "langgraph", salt: int = 0) -> None:
        self.client = Mnemosyne(path, salt)
        self.subject = subject
        self._type_name = "langgraph_checkpoint"

    @classmethod
    def from_client(
        cls, client: Mnemosyne, subject: str = "langgraph"
    ) -> "MnemosyneLangGraphSaver":
        """Wrap an existing Mnemosyne client (useful for tests)."""
        inst = cls.__new__(cls)
        inst.client = client
        inst.subject = subject
        inst._type_name = "langgraph_checkpoint"
        return inst

    @staticmethod
    def _thread_id(config: dict) -> str:
        return str(config.get("configurable", {}).get("thread_id", "default"))

    @staticmethod
    def _checkpoint_id(config: dict) -> Optional[str]:
        return config.get("configurable", {}).get("checkpoint_id")

    def get(self, config: dict) -> Optional[dict]:
        tid = self._thread_id(config)
        cid = self._checkpoint_id(config)
        if cid is not None:
            key = f"{tid}:{cid}"
            results = self.client.find_similar(
                self.subject, text=key, k=5, fusion="text_only"
            )
            for r in results:
                props = r["ko"]["properties"]
                if props.get("search_key") == key:
                    return json.loads(props["checkpoint_json"])
            return None

        # No checkpoint_id requested: return the latest for the thread.
        results = self.client.find_similar(
            self.subject, text=tid, k=50, fusion="text_only"
        )
        best: Optional[dict] = None
        best_ts = 0
        for r in results:
            props = r["ko"]["properties"]
            if props.get("thread_id") == tid and r["ko"]["commit_ts"] > best_ts:
                best = json.loads(props["checkpoint_json"])
                best_ts = r["ko"]["commit_ts"]
        return best

    def put(
        self,
        config: dict,
        checkpoint: dict,
        metadata: Optional[dict] = None,
    ) -> dict:
        tid = self._thread_id(config)
        cid = checkpoint.get("id") or checkpoint.get("ts") or "latest"
        key = f"{tid}:{cid}"
        props: dict[str, Any] = {
            "thread_id": tid,
            "checkpoint_id": cid,
            "search_key": key,
            "checkpoint_json": json.dumps(checkpoint),
            "metadata_json": json.dumps(metadata or {}),
        }
        self.client.remember(self.subject, self._type_name, props)
        new_config = copy.deepcopy(config)
        new_config.setdefault("configurable", {})["checkpoint_id"] = cid
        return new_config

    def list(self, config: dict, *, limit: Optional[int] = None) -> Iterator[dict]:
        tid = self._thread_id(config)
        results = self.client.find_similar(
            self.subject, text=tid, k=limit or 100, fusion="text_only"
        )
        for r in results:
            props = r["ko"]["properties"]
            if props.get("thread_id") == tid:
                yield json.loads(props["checkpoint_json"])

    async def aget(self, config: dict) -> Optional[dict]:
        return await asyncio.to_thread(self.get, config)

    async def aput(
        self,
        config: dict,
        checkpoint: dict,
        metadata: Optional[dict] = None,
    ) -> dict:
        return await asyncio.to_thread(self.put, config, checkpoint, metadata)

    async def alist(self, config: dict, *, limit: Optional[int] = None) -> list[dict]:
        return await asyncio.to_thread(
            lambda: list(self.list(config, limit=limit))
        )

    def close(self) -> None:
        self.client.close()
