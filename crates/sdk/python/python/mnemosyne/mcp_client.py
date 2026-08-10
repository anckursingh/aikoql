"""Pure Python MCP JSON-RPC client for Mnemosyne (MRFC-0040).

Talks to a mnemosyne-mcp server over TCP. No native dependencies.
"""

import json
import socket
import time
from typing import Any, Dict, List, Optional, Tuple, Union


class McpError(Exception):
    """Structured error from the MCP server (MRFC-0040 error codes)."""

    def __init__(self, code: str, message: str, retryable: bool = False, suggestion: str = ""):
        self.code = code
        self.message = message
        self.retryable = retryable
        self.suggestion = suggestion
        super().__init__(f"[{code}] {message}")

    @classmethod
    def from_response(cls, err: dict) -> "McpError":
        return cls(
            code=err.get("code", "INTERNAL"),
            message=err.get("message", "unknown error"),
            retryable=err.get("retryable", False),
            suggestion=err.get("suggestion", ""),
        )


class McpClient:
    """JSON-RPC 2.0 client for mnemosyne-mcp over TCP."""

    def __init__(self, host: str = "127.0.0.1", port: int = 9090):
        self.host = host
        self.port = port
        self._sock: Optional[socket.socket] = None
        self._buf = b""
        self._next_id = 0

    def connect(self, timeout: float = 5.0) -> "McpClient":
        self._sock = socket.create_connection((self.host, self.port), timeout=timeout)
        self._sock.settimeout(timeout)
        return self

    def close(self):
        if self._sock:
            try:
                self._sock.close()
            except OSError:
                pass
            self._sock = None

    def __enter__(self):
        return self.connect()

    def __exit__(self, *args):
        self.close()

    # -- JSON-RPC core --------------------------------------------------

    def _send(self, payload: dict):
        frame = json.dumps(payload, default=str) + "\n"
        self._sock.sendall(frame.encode("utf-8"))

    def _recv(self) -> dict:
        while True:
            # Check if we already have a full line in buffer.
            if b"\n" in self._buf:
                line, self._buf = self._buf.split(b"\n", 1)
                text = line.decode("utf-8").strip()
                if not text:
                    continue
                return json.loads(text)
            chunk = self._sock.recv(4096)
            if not chunk:
                raise ConnectionError("server closed connection")
            self._buf += chunk

    def _rpc(self, method: str, params: Optional[dict] = None) -> dict:
        self._next_id += 1
        req = {"jsonrpc": "2.0", "id": self._next_id, "method": method}
        if params is not None:
            req["params"] = params
        self._send(req)
        resp = self._recv()
        if "error" in resp:
            err = resp["error"]
            raise McpError(
                code=err.get("code", "INTERNAL"),
                message=err.get("message", str(err)),
            )
        return resp.get("result", resp)

    # -- MCP protocol ---------------------------------------------------

    def initialize(self, client_name: str = "mnemosyne-py", client_version: str = "0.1.0"):
        return self._rpc("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": client_name, "version": client_version},
        })

    def session_init(self, agent_id: str, run_id: Optional[str] = None,
                     tenant: Optional[str] = None, roles: Optional[List[str]] = None):
        """Establish session identity (MRFC-0040). Subsequent calls inherit it."""
        params: Dict[str, Any] = {"agent_id": agent_id}
        if run_id:
            params["run_id"] = run_id
        if tenant:
            params["tenant"] = tenant
        if roles:
            params["roles"] = roles
        return self._rpc("session/init", params)

    def call_tool(self, name: str, arguments: Optional[dict] = None) -> dict:
        """Call an MCP tool. Returns the parsed data payload."""
        params: Dict[str, Any] = {"name": name}
        if arguments:
            params["arguments"] = arguments
        result = self._rpc("tools/call", params)
        # Unwrap MCP content envelope.
        text = result.get("content", [{}])[0].get("text", "{}")
        data = json.loads(text)
        if not data.get("ok", True):
            raise McpError.from_response(data.get("error", {}))
        return data.get("data", data)

    # -- Tool wrappers (high-level API) ---------------------------------

    def remember(self, type_name: str, properties: Optional[dict] = None,
                 koid: Optional[str] = None, subject: Optional[str] = None,
                 note: Optional[str] = None, idempotency_key: Optional[str] = None,
                 embed: bool = False, **kwargs) -> dict:
        args: Dict[str, Any] = {"type_name": type_name}
        if properties:
            args["properties"] = properties
        if koid:
            args["koid"] = koid
        if subject:
            args["subject"] = subject
        if note:
            args["note"] = note
        if idempotency_key:
            args["idempotency_key"] = idempotency_key
        if embed:
            args["embed"] = True
        args.update(kwargs)
        return self.call_tool("remember", args)

    def get(self, koid: str, subject: Optional[str] = None) -> dict:
        args: Dict[str, Any] = {"koid": koid}
        if subject:
            args["subject"] = subject
        return self.call_tool("get", args)

    def forget(self, koid: str, mode: str = "tombstone", subject: Optional[str] = None) -> dict:
        args: Dict[str, Any] = {"koid": koid, "mode": mode}
        if subject:
            args["subject"] = subject
        return self.call_tool("forget", args)

    def find_similar(self, text: Optional[str] = None, vector: Optional[List[float]] = None,
                     type_name: Optional[str] = None, k: int = 10,
                     fusion: Optional[str] = None, subject: Optional[str] = None) -> dict:
        args: Dict[str, Any] = {}
        if text:
            args["text"] = text
        if vector:
            args["vector"] = vector
        if type_name:
            args["type_name"] = type_name
        args["k"] = k
        if fusion:
            args["fusion"] = fusion
        if subject:
            args["subject"] = subject
        return self.call_tool("find_similar", args)

    def aikoql(self, query: str, subject: Optional[str] = None) -> dict:
        args: Dict[str, Any] = {"query": query}
        if subject:
            args["subject"] = subject
        return self.call_tool("aikoql", args)

    def relate(self, from_koid: str, to_koid: str, rel_type: str,
               subject: Optional[str] = None) -> dict:
        args = {"from": from_koid, "to": to_koid, "rel_type": rel_type}
        if subject:
            args["subject"] = subject
        return self.call_tool("relate", args)

    def traverse(self, koid: str, rel_type: Optional[str] = None, depth: int = 1,
                 subject: Optional[str] = None) -> dict:
        args: Dict[str, Any] = {"koid": koid, "depth": depth}
        if rel_type:
            args["rel_type"] = rel_type
        if subject:
            args["subject"] = subject
        return self.call_tool("traverse", args)

    def batch(self, operations: List[dict]) -> dict:
        return self.call_tool("batch", {"operations": operations})

    def health(self) -> dict:
        return self.call_tool("health", {})

    def discover_schema(self) -> dict:
        return self.call_tool("discover_schema", {})

    def decide(self, koid: str, decision: str, rationale: str = "",
               confidence: float = 1.0) -> dict:
        return self.call_tool("decide", {
            "koid": koid, "decision": decision,
            "rationale": rationale, "confidence": confidence,
        })

    def agent_memory(self, agent_id: str, key: Optional[str] = None,
                     value: Any = None, ttl: int = 3600) -> dict:
        args: Dict[str, Any] = {"agent_id": agent_id}
        if key is not None:
            args["key"] = key
        if value is not None:
            args["value"] = value
        args["ttl"] = ttl
        return self.call_tool("agent_memory", args)

    def metrics(self) -> dict:
        return self.call_tool("metrics", {})

    def trace(self, koid: str, subject: Optional[str] = None) -> dict:
        args: Dict[str, Any] = {"koid": koid}
        if subject:
            args["subject"] = subject
        return self.call_tool("trace", args)

    def explain(self, koid: str, version: Optional[int] = None,
                subject: Optional[str] = None) -> dict:
        args: Dict[str, Any] = {"koid": koid}
        if version is not None:
            args["version"] = version
        if subject:
            args["subject"] = subject
        return self.call_tool("explain", args)

    def aikoql_stream(self, query: str, subject: Optional[str] = None):
        """Streaming query — yields result chunks incrementally (MRFC-0040 #5).

        Usage:
            for chunk in client.aikoql_stream("MATCH Task RETURN *"):
                for row in chunk["results"]:
                    process(row)
        """
        params: Dict[str, Any] = {"query": query}
        if subject:
            params["subject"] = subject

        resp = self._rpc("aikoql/stream", params)
        stream_id = resp["stream_id"]
        yield resp  # first chunk

        # Read subsequent notification frames until done.
        total = resp.get("total_chunks", 1)
        received = 1
        while received < total:
            frame = self._recv()
            if frame.get("method") != "notifications/notify":
                continue
            p = frame.get("params", {})
            if p.get("stream_id") != stream_id:
                continue
            yield p
            received += 1
            if p.get("done"):
                break
