"""Unified Agent interface for Mnemosyne (MRFC-0040 items #1 + #12).

Supports both embedded (PyO3) and server (MCP TCP) modes transparently.

    from mnemosyne import Agent

    # Embedded mode (in-process, requires compiled PyO3 extension):
    db = Agent.connect("./kb.redb")

    # Server mode (talks to mnemosyne-mcp over TCP):
    db = Agent.connect("localhost:9090")

    # Server mode (tuple):
    db = Agent.connect(("localhost", 9090))
"""

import os
from typing import Any, Dict, List, Optional, Tuple, Union


class Agent:
    """Unified Mnemosyne agent interface.

    Auto-detects connection mode:
    - Path ending in .redb → embedded mode (PyO3)
    - "host:port" string → server mode (MCP TCP)
    - (host, port) tuple → server mode (MCP TCP)
    """

    def __init__(self):
        self._backend: Any = None
        self._mode: str = ""

    @staticmethod
    def connect(target: Union[str, Tuple[str, int]], **kwargs) -> "Agent":
        """Connect to Mnemosyne. Auto-detects mode from target format."""
        agent = Agent()
        if isinstance(target, tuple):
            host, port = target
            agent._mode = "mcp"
            from .mcp_client import McpClient
            agent._backend = McpClient(host, port).connect(timeout=kwargs.get("timeout", 5.0))
            agent._backend.initialize(
                kwargs.get("client_name", "mnemosyne-py"),
                kwargs.get("client_version", "0.1.0"),
            )
        elif isinstance(target, str):
            if target.endswith(".redb") or os.path.exists(target):
                # Embedded mode: use PyO3 native extension.
                agent._mode = "embedded"
                try:
                    from ._mnemosyne import Mnemosyne
                    agent._backend = Mnemosyne(target)
                except ImportError:
                    raise ImportError(
                        "Embedded mode requires the compiled PyO3 extension (_mnemosyne). "
                        "Build with: pip install maturin && maturin develop --release\n"
                        "Or use server mode: Agent.connect('localhost:9090')"
                    )
            else:
                # Server mode: assume "host:port".
                agent._mode = "mcp"
                host, _, port = target.partition(":")
                from .mcp_client import McpClient
                agent._backend = McpClient(host, int(port)).connect(timeout=kwargs.get("timeout", 5.0))
                agent._backend.initialize(
                    kwargs.get("client_name", "mnemosyne-py"),
                    kwargs.get("client_version", "0.1.0"),
                )
        else:
            raise TypeError(f"Expected str or tuple, got {type(target).__name__}")
        return agent

    @property
    def mode(self) -> str:
        """'embedded' or 'mcp'."""
        return self._mode

    def close(self):
        if self._backend is not None:
            if self._mode == "mcp":
                self._backend.close()
            elif hasattr(self._backend, "close"):
                self._backend.close()
            self._backend = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    # -- Session identity (MRFC-0040 #2) -------------------------------

    def session_init(self, agent_id: str, run_id: Optional[str] = None,
                     tenant: Optional[str] = None, roles: Optional[List[str]] = None) -> dict:
        """Establish session identity. Server mode only."""
        if self._mode == "mcp":
            return self._backend.session_init(agent_id, run_id, tenant, roles)
        # Embedded mode: identity is implicit (the process owner).
        return {"session": {"agent_id": agent_id}, "established": True,
                "note": "Embedded mode: identity is the process owner."}

    # -- Tool methods (unified API) ------------------------------------

    def remember(self, type_name: str, properties: Optional[dict] = None,
                 koid: Optional[str] = None, subject: Optional[str] = None,
                 note: Optional[str] = None, **kwargs) -> dict:
        if self._mode == "mcp":
            return self._backend.remember(type_name, properties, koid, subject, note, **kwargs)
        return self._backend.remember(type_name, properties or {}, koid, subject, note, **kwargs)

    def get(self, koid: str, subject: Optional[str] = None) -> dict:
        if self._mode == "mcp":
            return self._backend.get(koid, subject)
        return self._backend.get(koid, subject)

    def find_similar(self, text: Optional[str] = None, vector: Optional[List[float]] = None,
                     type_name: Optional[str] = None, k: int = 10, **kwargs) -> dict:
        if self._mode == "mcp":
            return self._backend.find_similar(text, vector, type_name, k, **kwargs)
        return self._backend.find_similar(text, vector, type_name, k, **kwargs)

    def aikoql(self, query: str, subject: Optional[str] = None) -> dict:
        if self._mode == "mcp":
            return self._backend.aikoql(query, subject)
        return self._backend.aikoql(query, subject)

    def relate(self, from_koid: str, to_koid: str, rel_type: str,
               subject: Optional[str] = None) -> dict:
        if self._mode == "mcp":
            return self._backend.relate(from_koid, to_koid, rel_type, subject)
        return self._backend.relate(from_koid, to_koid, rel_type, subject)

    def traverse(self, koid: str, rel_type: Optional[str] = None,
                 depth: int = 1, subject: Optional[str] = None) -> dict:
        if self._mode == "mcp":
            return self._backend.traverse(koid, rel_type, depth, subject)
        return self._backend.traverse(koid, rel_type, depth, subject)

    def forget(self, koid: str, mode: str = "tombstone",
               subject: Optional[str] = None) -> dict:
        if self._mode == "mcp":
            return self._backend.forget(koid, mode, subject)
        return self._backend.forget(koid, mode, subject)

    def health(self) -> dict:
        if self._mode == "mcp":
            return self._backend.health()
        return {"ready": True, "status": "healthy"}

    def metrics(self) -> dict:
        if self._mode == "mcp":
            return self._backend.metrics()
        return {}

    def batch(self, operations: List[dict]) -> dict:
        if self._mode == "mcp":
            return self._backend.batch(operations)
        raise NotImplementedError("Batch operations require server mode (MCP).")

    def decide(self, koid: str, decision: str, rationale: str = "",
               confidence: float = 1.0) -> dict:
        if self._mode == "mcp":
            return self._backend.decide(koid, decision, rationale, confidence)
        raise NotImplementedError("Decide requires server mode (MCP).")

    def agent_memory(self, agent_id: str, key: Optional[str] = None,
                     value: Any = None, ttl: int = 3600) -> dict:
        if self._mode == "mcp":
            return self._backend.agent_memory(agent_id, key, value, ttl)
        raise NotImplementedError("Agent memory requires server mode (MCP).")

    def discover_schema(self) -> dict:
        if self._mode == "mcp":
            return self._backend.discover_schema()
        raise NotImplementedError("Schema discovery requires server mode (MCP).")
