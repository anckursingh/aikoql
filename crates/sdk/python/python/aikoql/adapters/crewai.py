"""CrewAI memory adapter backed by aikoql.

Presents the small surface CrewAI expects from a memory backend:
`save(value, metadata)`, `search(query, limit)`, and `reset()`.
"""

from __future__ import annotations

import json
from typing import Any, Optional

from aikoql._aikoql import aikoql


class AikoqlCrewAIMemory:
    """Long-term memory backend for a CrewAI agent/role using aikoql."""

    def __init__(
        self,
        path: str,
        subject: str = "crewai",
        role: str = "memory",
        salt: int = 0,
    ) -> None:
        self.client = aikoql(path, salt)
        self.subject = subject
        self.role = role
        self._type_name = "crewai_memory"

    @classmethod
    def from_client(
        cls,
        client: aikoql,
        subject: str = "crewai",
        role: str = "memory",
    ) -> "AikoqlCrewAIMemory":
        """Wrap an existing aikoql client (useful for tests)."""
        inst = cls.__new__(cls)
        inst.client = client
        inst.subject = subject
        inst.role = role
        inst._type_name = "crewai_memory"
        return inst

    def save(self, value: str, metadata: Optional[dict] = None) -> dict[str, Any]:
        """Store a memory value for the configured role."""
        props = {
            "text": value,
            "role": self.role,
            "metadata_json": json.dumps(metadata or {}),
        }
        return self.client.remember(
            self.subject,
            self._type_name,
            props,
            semantic={"summary": value},
        )

    def search(self, query: str, limit: int = 3) -> list[str]:
        """Recall the most relevant stored memory values as plain strings."""
        results = self.client.find_similar(
            self.subject,
            text=query,
            k=limit,
            fusion="text_only",
        )
        out: list[str] = []
        for r in results:
            text = r["ko"]["properties"].get("text")
            if text:
                out.append(text)
        return out

    def reset(self) -> None:
        """Tombstone all stored memories for this role.

        # ponytail: O(n) linear scan+forget; CrewAI memory reset is rare in
        production runs, so this is fine. Add a bulk-tombstone API if it ever
        becomes hot.
        # An empty find_similar query used to enumerate every visible
        # object via pre-AG-20 zero-score padding — it now honestly finds
        # nothing, so enumerate with a MATCH statement instead.
        """
        rows = self.client.aikoql(
            f"MATCH {self._type_name} RETURN *", subject=self.subject
        )
        if isinstance(rows, dict):  # MCP clients wrap rows in {"results": [...]}
            rows = rows.get("results", [])
        for r in rows:
            self.client.forget(self.subject, r["koid"])

    def close(self) -> None:
        self.client.close()
