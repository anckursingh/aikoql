"""CrewAI memory adapter backed by Mnemosyne.

Presents the small surface CrewAI expects from a memory backend:
`save(value, metadata)`, `search(query, limit)`, and `reset()`.
"""

from __future__ import annotations

import json
from typing import Any, Optional

from mnemosyne._mnemosyne import Mnemosyne


class MnemosyneCrewAIMemory:
    """Long-term memory backend for a CrewAI agent/role using Mnemosyne."""

    def __init__(
        self,
        path: str,
        subject: str = "crewai",
        role: str = "memory",
        salt: int = 0,
    ) -> None:
        self.client = Mnemosyne(path, salt)
        self.subject = subject
        self.role = role
        self._type_name = "crewai_memory"

    @classmethod
    def from_client(
        cls,
        client: Mnemosyne,
        subject: str = "crewai",
        role: str = "memory",
    ) -> "MnemosyneCrewAIMemory":
        """Wrap an existing Mnemosyne client (useful for tests)."""
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
        """
        results = self.client.find_similar(
            self.subject, text="", k=1000, fusion="text_only"
        )
        for r in results:
            if r["ko"]["type_name"] == self._type_name:
                self.client.forget(self.subject, r["ko"]["koid"])

    def close(self) -> None:
        self.client.close()
