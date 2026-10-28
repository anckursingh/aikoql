"""MRFC-0080 R5b — the client follows a NOT_LEADER leader hint.

A fenced server refuses writes with NOT_LEADER and the newer leader's
client address; the client retries the call once against that leader over
a fresh, initialized connection, and stays there. Transport-level: two
scripted servers, no aikoql binary.
"""

import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent / "python"))
from aikoql import McpClient, McpError
from aikoql.mcp_client import MIN_SERVER_VERSION

from scripted import ScriptedServer


def _init(req):
    return {"jsonrpc": "2.0", "id": req["id"], "result": {
        "protocolVersion": "2024-11-05",
        "serverInfo": {"name": "aikoql-mcp", "version": MIN_SERVER_VERSION}}}


def _tool(req, payload):
    return {"jsonrpc": "2.0", "id": req["id"],
            "result": {"content": [{"text": json.dumps(payload)}]}}


def _not_leader(req, leader=None):
    err = {"code": "NOT_LEADER", "message": "not leader", "retryable": True}
    if leader:
        err["leader"] = leader
    return _tool(req, {"ok": False, "error": err})


def test_not_leader_follows_the_leader_hint():
    saw = []

    def lead(req):
        saw.append(req["method"])
        if req["method"] == "initialize":
            return [_init(req)]
        return [_tool(req, {"ok": True, "data": {"koid": "ko-1"}})]

    with ScriptedServer(respond=lead) as leader:
        hint = f"127.0.0.1:{leader.port}"

        def fenced(req):
            if req["method"] == "initialize":
                return [_init(req)]
            return [_not_leader(req, hint)]

        with ScriptedServer(respond=fenced) as f:
            c = McpClient("127.0.0.1", f.port, "t").connect(timeout=5)
            try:
                c.initialize()
                assert c.call_tool("remember", {"type_name": "note"}) == {"koid": "ko-1"}
                c.call_tool("remember", {"type_name": "note"})
                assert (c.host, c.port) == ("127.0.0.1", leader.port)
            finally:
                c.close()
    assert saw == ["initialize", "tools/call", "tools/call"]


def test_not_leader_without_a_hint_is_raised():
    with ScriptedServer(respond=lambda req: [_not_leader(req)]) as s:
        c = McpClient("127.0.0.1", s.port).connect(timeout=5)
        try:
            with pytest.raises(McpError) as e:
                c.call_tool("remember", {"type_name": "note"})
            assert e.value.code == "NOT_LEADER"
            assert e.value.retryable
            assert e.value.leader is None
        finally:
            c.close()
