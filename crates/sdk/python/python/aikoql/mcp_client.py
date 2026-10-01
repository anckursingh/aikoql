"""Pure Python MCP JSON-RPC client for aikoql (MRFC-0040).

Talks to a aikoql-mcp server over TCP. No native dependencies.
"""

import json
import socket
import time
import uuid
from typing import Any, Dict, List, Optional, Tuple, Union

# P5-M12 (ND-12) version contract: the oldest server this SDK will talk to.
# Pinned by tests/test_version_contract.py to the workspace version — a
# workspace bump turns that test RED until this constant follows.
MIN_SERVER_VERSION = "0.2.0"


def _parse_version(v: str) -> Tuple[int, ...]:
    """Dotted-int version tuple; non-numeric segments become -1 (never >=)."""
    parts: List[int] = []
    for seg in v.split("."):
        try:
            parts.append(int(seg))
        except ValueError:
            parts.append(-1)
    return tuple(parts)


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


class Transaction:
    """A staged write handle (§3.5): begin on the connection, execute
    stages ops, commit or rollback closes it. The txn_id is a first-class
    attribute — it never leaks as a bare tool argument.
    """

    def __init__(self, client: "McpClient", txn_id: str):
        self._client = client
        self.txn_id = txn_id
        self._done = False

    def _guard(self):
        if self._done:
            raise McpError(
                code="INVALID_ARGUMENT",
                message=f"transaction {self.txn_id} is closed",
                suggestion="Begin a new transaction.",
            )

    def execute(self, action: str, type_name: Optional[str] = None,
                koid: Optional[str] = None,
                properties: Optional[dict] = None) -> dict:
        """Stage one write: action is "create" or "update"."""
        self._guard()
        op: Dict[str, Any] = {"action": action}
        if type_name is not None:
            op["type_name"] = type_name
        if koid is not None:
            op["koid"] = koid
        if properties is not None:
            op["properties"] = properties
        return self._client.call_tool(
            "txn_stage", {"txn_id": self.txn_id, "op": op})

    def commit(self) -> dict:
        """Apply the staged writes and close the handle."""
        self._guard()
        result = self._client.call_tool(
            "txn_commit", {"txn_id": self.txn_id})
        self._done = True
        return result

    def rollback(self) -> dict:
        """Discard the staged writes and close the handle."""
        self._guard()
        result = self._client.call_tool(
            "txn_rollback", {"txn_id": self.txn_id})
        self._done = True
        return result


class McpClient:
    """JSON-RPC 2.0 client for aikoql-mcp over TCP."""

    def __init__(self, host: str = "127.0.0.1", port: int = 9090, token: Optional[str] = None):
        self.host = host
        self.port = port
        # P3-M1 servers require a --tcp-token: it rides initialize params.
        self.token = token
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

    def _rpc(self, method: str, params: Optional[dict] = None,
             timeout: Optional[float] = None) -> dict:
        self._next_id += 1
        req = {"jsonrpc": "2.0", "id": self._next_id, "method": method}
        if params is not None:
            req["params"] = params
        self._send(req)
        resp = self._recv_response(self._next_id, timeout)
        if "error" in resp:
            err = resp["error"]
            raise McpError(
                code=err.get("code", "INTERNAL"),
                message=err.get("message", str(err)),
            )
        return resp.get("result", resp)

    def _recv_response(self, expected_id: int,
                       timeout: Optional[float] = None) -> dict:
        """Read frames until the response for expected_id arrives.

        Serialized-but-ID-based (§3.3): a notification or id-less frame is
        never a response, a stale id (duplicate or late, < expected) is
        skipped, an impossible response (id > expected, or non-numeric) is
        a protocol violation, and a malformed frame is never misread as
        the response. A missing response is TIMEOUT (retryable).
        """
        deadline = None if timeout is None else time.monotonic() + timeout
        while True:
            if deadline is not None and time.monotonic() >= deadline:
                raise McpError(
                    code="TIMEOUT",
                    message=f"no response for request {expected_id} "
                            f"within {timeout}s",
                    retryable=True,
                    suggestion="Retry with backoff; the request may have "
                               "committed.",
                )
            try:
                frame = self._recv()
            except socket.timeout:
                if deadline is None:
                    raise
                continue  # bounded by the deadline check above
            except ValueError:
                continue  # malformed frame — never a response
            rid = frame.get("id")
            if rid is None:
                continue  # notification / id-less frame
            if not isinstance(rid, int):
                raise McpError(
                    code="PROTOCOL_ERROR",
                    message=f"response id {rid!r} is not an integer",
                    suggestion="Check SDK/server version pairing.",
                )
            if rid < expected_id:
                continue  # stale: duplicate or late response
            if rid > expected_id:
                raise McpError(
                    code="PROTOCOL_ERROR",
                    message=f"response id {rid} does not match request "
                            f"{expected_id}",
                    suggestion="Check SDK/server version pairing.",
                )
            return frame

    # -- MCP protocol ---------------------------------------------------

    def initialize(self, client_name: str = "aikoql-py", client_version: str = "0.1.0"):
        params = {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": client_name, "version": client_version},
        }
        if self.token:
            params["token"] = self.token
        result = self._rpc("initialize", params)
        # P5-M12 (ND-12): fail fast on a server older than the contract
        # minimum (docs/version-compatibility.md). A newer server is fine —
        # forward-compatible optimism, no upper bound.
        server_version = (
            result.get("serverInfo", {}) or {}
        ).get("version", "")
        if _parse_version(server_version) < _parse_version(MIN_SERVER_VERSION):
            raise McpError(
                code="VERSION_MISMATCH",
                message=(
                    f"server version {server_version!r} is older than the SDK "
                    f"minimum {MIN_SERVER_VERSION}"
                ),
                retryable=False,
                suggestion="Upgrade the aikoql-mcp server to a supported version",
            )
        return result

    def session_init(self, agent_id: Optional[str] = None, run_id: Optional[str] = None,
                     tenant: Optional[str] = None, roles: Optional[List[str]] = None):
        """Establish session identity (MRFC-0040). Subsequent calls inherit it.

        P3-M1: on TCP the identity is server-assigned by --tcp-token, so
        agent_id must be omitted there (only run_id is per-session).
        """
        params: Dict[str, Any] = {}
        if agent_id:
            params["agent_id"] = agent_id
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

    def begin(self, txn_id: Optional[str] = None) -> "Transaction":
        """Open a transaction (§3.5). A generated 32-hex id by default;
        pass one to retry the same begin idempotently (P5-M20).
        """
        if txn_id is None:
            txn_id = uuid.uuid4().hex
        self.call_tool("txn_begin", {"txn_id": txn_id})
        return Transaction(self, txn_id)

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
                     fusion: Optional[str] = None, subject: Optional[str] = None,
                     wait_for_freshness_ms: Optional[int] = None) -> dict:
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
        if wait_for_freshness_ms is not None:
            args["wait_for_freshness_ms"] = wait_for_freshness_ms
        return self.call_tool("find_similar", args)

    def aikoql(self, query: str, subject: Optional[str] = None) -> dict:
        args: Dict[str, Any] = {"query": query}
        if subject:
            args["subject"] = subject
        return self.call_tool("aikoql", args)

    def prepare(self, query: str) -> "PreparedStatement":
        """Prepare a statement (§3.6): validates the query client-side and
        extracts its :name placeholders. Compiles on every execute until a
        native prepare protocol lands."""
        from aikoql.prepared import PreparedStatement, _PLACEHOLDER

        if not query.strip():
            raise McpError(
                code="INVALID_ARGUMENT",
                message="prepared statement query is empty",
                suggestion="Pass a non-empty AikoQL query.",
            )
        params = []
        seen = set()
        for m in _PLACEHOLDER.finditer(query):
            if m.group(1) not in seen:
                seen.add(m.group(1))
                params.append(m.group(1))
        return PreparedStatement(self, query, tuple(params))

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
