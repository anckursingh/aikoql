"""Unified Agent interface for aikoql (MRFC-0040 items #1 + #12).

Supports both embedded (PyO3) and server (MCP TCP) modes transparently.

    from aikoql import Agent

    # Embedded mode (in-process, requires compiled PyO3 extension).
    # A fresh path creates an aikoql-v2 database directory (the default):
    db = Agent.connect("./kb")

    # Server mode (talks to aikoql-mcp over TCP):
    db = Agent.connect("localhost:9090")

    # Server mode (tuple):
    db = Agent.connect(("localhost", 9090))
"""

import os
from typing import Any, Dict, List, Optional, Tuple, Union


class Agent:
    """Unified aikoql agent interface.

    Auto-detects connection mode:
    - Any filesystem path (fresh or existing) → embedded mode (PyO3).
      The 2026-09-07 default: a fresh path creates an aikoql-v2 database
      directory.
    - "host:port" string → server mode (MCP TCP)
    - (host, port) tuple → server mode (MCP TCP)
    """

    def __init__(self):
        self._backend: Any = None
        self._mode: str = ""

    @staticmethod
    def connect(target: Union[str, Tuple[str, int]], **kwargs) -> "Agent":
        """Connect to aikoql. Auto-detects mode from target format."""
        agent = Agent()
        if isinstance(target, tuple):
            host, port = target
            agent._mode = "mcp"
            from .mcp_client import McpClient
            agent._backend = McpClient(host, port, kwargs.get("token")).connect(timeout=kwargs.get("timeout", 5.0))
            agent._backend.initialize(
                kwargs.get("client_name", "aikoql-py"),
                kwargs.get("client_version", "0.1.0"),
            )
        elif isinstance(target, str):
            # Server mode iff the string is a bare host:port — no path
            # separators and a numeric port. Anything path-like (fresh
            # paths included) is embedded: the engine's own auto-detection
            # decides aikoql-v2 / redb / v1 from what is on disk.
            is_host_port = (
                "/" not in target and "\\" not in target
                and target.count(":") == 1
                and target.partition(":")[2].isdigit()
            )
            if is_host_port:
                agent._mode = "mcp"
                host, _, port = target.partition(":")
                from .mcp_client import McpClient
                agent._backend = McpClient(host, int(port), kwargs.get("token")).connect(timeout=kwargs.get("timeout", 5.0))
                agent._backend.initialize(
                    kwargs.get("client_name", "aikoql-py"),
                    kwargs.get("client_version", "0.1.0"),
                )
            else:
                # Embedded mode: use PyO3 native extension.
                agent._mode = "embedded"
                try:
                    from ._aikoql import aikoql
                    agent._backend = aikoql(target)
                except ImportError:
                    raise ImportError(
                        "Embedded mode requires the compiled PyO3 extension (_aikoql). "
                        "Build with: pip install maturin && maturin develop --release\n"
                        "Or use server mode: Agent.connect('localhost:9090')"
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

    def session_init(self, agent_id: Optional[str] = None, run_id: Optional[str] = None,
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
        if note is not None:
            raise NotImplementedError("embedded remember: note is server-mode only")
        # Embedded identity: the process owner. The native surface is
        # subject-first: (subject, type_name, properties, semantic, roles, koid).
        return self._backend.remember(subject or "owner", type_name, properties or {},
                                      kwargs.get("semantic"), kwargs.get("roles"), koid)

    def get(self, koid: str, subject: Optional[str] = None) -> dict:
        if self._mode == "mcp":
            return self._backend.get(koid, subject)
        return self._backend.get(subject or "owner", koid)

    def find_similar(self, text: Optional[str] = None, vector: Optional[List[float]] = None,
                     type_name: Optional[str] = None, k: int = 10, **kwargs) -> list:
        if self._mode == "mcp":
            return self._backend.find_similar(text, vector, type_name, k, **kwargs).get(
                "results", [])
        if type_name is not None:
            raise NotImplementedError(
                "embedded find_similar: type_name filter is server-mode only")
        hits = self._backend.find_similar(kwargs.get("subject") or "owner", text, vector,
                                          kwargs.get("embedding_model"), k,
                                          kwargs.get("fusion", "rrf"))
        # Native Scored shape -> MCP results-item shape (flat {koid, score, ...}):
        # one Agent contract in both modes. The adapters use the native class
        # directly and keep the nested {ko, ...} shape.
        return [{"koid": h["ko"]["koid"], "score": h["score"],
                 "index_lag_ms": h["index_lag_ms"],
                 "type_name": h["ko"]["type_name"], "version": h["ko"]["version"]}
                for h in hits]

    def aikoql(self, query: str, subject: Optional[str] = None) -> dict:
        if self._mode == "mcp":
            return self._backend.aikoql(query, subject)
        return self._backend.aikoql(query, subject or "owner")

    def create_index(self, name: str, type_name: str,
                     properties: List[str]) -> dict:
        """P5-M17b: declare a property index (and refresh its statistics).
        MCP: the index_create tool; embedded: the native surface."""
        if self._mode == "mcp":
            return self._backend.call_tool(
                "index_create",
                {"name": name, "type_name": type_name, "properties": properties},
            )
        return self._backend.create_index(name, type_name, properties)

    def relate(self, from_koid: str, to_koid: str, rel_type: str,
               subject: Optional[str] = None) -> dict:
        if self._mode == "mcp":
            return self._backend.relate(from_koid, to_koid, rel_type, subject)
        return self._backend.relate(subject or "owner", from_koid, to_koid, rel_type)

    def traverse(self, koid: str, rel_type: Optional[str] = None,
                 depth: int = 1, subject: Optional[str] = None) -> dict:
        if self._mode == "mcp":
            return self._backend.traverse(koid, rel_type, depth, subject)
        return self._backend.traverse(subject or "owner", koid, rel_type, depth)

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
