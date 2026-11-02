"""AG-60: LlamaIndex chat memory backed by aikoql.

Drop-in for the upstream example (the docs' chat engine)::

    memory = AikoqlMemory(client=client)
    engine = SimpleChatEngine.from_defaults(llm=..., memory=memory)

Every ChatMessage is one KO (JSON via model_dump(mode="json")); get()
recalls the most relevant prior messages for the input by text.
"""

from __future__ import annotations

import json
from typing import Any, Optional

from llama_index.core.base.llms.types import ChatMessage, MessageRole
from llama_index.core.memory import BaseMemory
from pydantic import PrivateAttr

from ._client import enumerate_rows, forget, recall_properties, remember


class AikoqlMemory(BaseMemory):
    """Long-term chat memory: each ChatMessage is one KO."""

    _TYPE = "llamaindex_chat_message"

    client: Any
    subject: str = "llamaindex"
    recall_k: int = 10
    _last_user_text: Optional[str] = PrivateAttr(default=None)

    @classmethod
    def from_defaults(cls, **kwargs: Any) -> "AikoqlMemory":
        # upstream calls from_defaults(**kwargs); client= is ours
        if "client" not in kwargs:
            raise ValueError("AikoqlMemory.from_defaults needs client=")
        return cls(**kwargs)

    def _message(self, props: dict[str, Any]) -> Optional[ChatMessage]:
        raw = props.get("message_json")
        return ChatMessage(**json.loads(raw)) if raw else None

    def get(self, input: Optional[str] = None, **kwargs: Any) -> list[ChatMessage]:
        # the engine calls get(None) before responding — recall over the
        # last user message
        query = input if isinstance(input, str) and input.strip() else self._last_user_text
        if not query:
            return []
        pairs = []
        for props in recall_properties(self.client, self.subject, query, self.recall_k, self._TYPE):
            msg = self._message(props)
            if msg is not None:
                pairs.append((int(props.get("seq") or 0), msg))
        pairs.sort(key=lambda p: p[0])
        return [m for _, m in pairs]

    def get_all(self) -> list[ChatMessage]:
        pairs = []
        for row in enumerate_rows(self.client, self.subject, self._TYPE):
            msg = self._message(row.get("properties", {}))
            if msg is not None:
                pairs.append((int(row.get("properties", {}).get("seq") or 0), msg))
        pairs.sort(key=lambda p: p[0])
        return [m for _, m in pairs]

    def set(self, messages: list[ChatMessage]) -> None:
        self.reset()
        for m in messages:
            self.put(m)

    def put(self, message: ChatMessage) -> None:
        remember(
            self.client,
            self.subject,
            self._TYPE,
            {
                "seq": self._next_seq(),
                "message_json": json.dumps(message.model_dump(mode="json")),
            },
            semantic={"summary": str(message.content)},
        )
        if message.role == MessageRole.USER:
            self._last_user_text = str(message.content)

    def _next_seq(self) -> int:
        # one MATCH per put — chat-message volume is tiny
        rows = enumerate_rows(self.client, self.subject, self._TYPE)
        return max(
            (int(r.get("properties", {}).get("seq") or 0) for r in rows), default=0
        ) + 1

    def reset(self) -> None:
        # ponytail: O(n) scan+forget; a bulk-tombstone API would make this
        # one call (same note as the crewai adapter)
        for row in enumerate_rows(self.client, self.subject, self._TYPE):
            forget(self.client, self.subject, row["koid"])
        self._last_user_text = None
