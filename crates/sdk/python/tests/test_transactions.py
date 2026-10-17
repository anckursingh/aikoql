"""D-08: the §3.5 transaction handle — conn.begin() / tx.execute() /
tx.commit() / tx.rollback(). The txn_id is a first-class attribute of the
handle, never a bare tool argument the caller threads between call sites.

Scripted-server legs pin the SDK shape and the done-guard; the real-server
legs (mcp_server) pin the semantics: committed stages are visible, rolled
back stages are gone.
"""

import json
import re
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent / "python"))
from aikoql import McpClient, McpError, Transaction

from scripted import ScriptedServer


def _client(port):
    return McpClient("127.0.0.1", port).connect(timeout=2.0)


def _envelope(rid, data):
    text = json.dumps({"ok": True, "data": data})
    return {"jsonrpc": "2.0", "id": rid,
            "result": {"content": [{"type": "text", "text": text}]}}


def _responder():
    """A scripted server answering the four txn tools; records staged ops."""
    staged = []

    def respond(req):
        name = req["params"]["name"]
        args = req["params"]["arguments"]
        if name == "txn_begin":
            data = {"txn_id": args["txn_id"], "snapshot_ts": 1000}
        elif name == "txn_stage":
            staged.append(args)
            data = {"staged": 1}
        elif name == "txn_commit":
            data = {"results": [{"action": "create", "koid": "k1"}],
                    "deduped": False}
        elif name == "txn_rollback":
            data = {"rolled_back": True}
        return [_envelope(req["id"], data)]

    return respond, staged


def test_begin_returns_a_transaction_handle():
    respond, _ = _responder()
    with ScriptedServer(respond=respond) as s:
        c = _client(s.port)
        try:
            tx = c.begin(txn_id="abc")
            assert isinstance(tx, Transaction)
            assert tx.txn_id == "abc"
        finally:
            c.close()


def test_begin_defaults_to_a_generated_id():
    respond, _ = _responder()
    with ScriptedServer(respond=respond) as s:
        c = _client(s.port)
        try:
            tx = c.begin()
            assert re.fullmatch(r"[0-9a-f]{32}", tx.txn_id)
        finally:
            c.close()


def test_execute_stages_an_op_on_the_handle():
    respond, staged = _responder()
    with ScriptedServer(respond=respond) as s:
        c = _client(s.port)
        try:
            tx = c.begin(txn_id="abc")
            tx.execute("create", type_name="person",
                       properties={"name": "Ada"})
        finally:
            c.close()
    assert staged == [{"txn_id": "abc",
                       "op": {"action": "create", "type_name": "person",
                              "properties": {"name": "Ada"}}}]


def test_commit_returns_results_and_closes_the_handle():
    respond, _ = _responder()
    with ScriptedServer(respond=respond) as s:
        c = _client(s.port)
        try:
            tx = c.begin(txn_id="abc")
            assert tx.commit() == {"results": [{"action": "create",
                                                "koid": "k1"}],
                                   "deduped": False}
            with pytest.raises(McpError) as exc:
                tx.execute("create")
            assert exc.value.code == "INVALID_ARGUMENT"
        finally:
            c.close()


def test_rollback_closes_the_handle():
    respond, _ = _responder()
    with ScriptedServer(respond=respond) as s:
        c = _client(s.port)
        try:
            tx = c.begin(txn_id="abc")
            assert tx.rollback() == {"rolled_back": True}
            with pytest.raises(McpError) as exc:
                tx.commit()
            assert exc.value.code == "INVALID_ARGUMENT"
        finally:
            c.close()


def _rows(data):
    return data["results"] if isinstance(data, dict) and "results" in data else data


def test_transaction_commit_is_visible_on_the_real_server(mcp_server):
    host_port, _, token = mcp_server
    host, _, port = host_port.partition(":")
    c = McpClient(host, int(port), token).connect()
    try:
        c.initialize()
        tx = c.begin()
        tx.execute("create", type_name="person", properties={"name": "Ada"})
        tx.commit()
        rows = _rows(c.aikoql("MATCH person RETURN *"))
        assert any(r["properties"].get("name") == "Ada" for r in rows)
    finally:
        c.close()


def test_transaction_rollback_is_gone_on_the_real_server(mcp_server):
    host_port, _, token = mcp_server
    host, _, port = host_port.partition(":")
    c = McpClient(host, int(port), token).connect()
    try:
        c.initialize()
        tx = c.begin()
        tx.execute("create", type_name="person", properties={"name": "Ghost"})
        tx.rollback()
        rows = _rows(c.aikoql("MATCH person RETURN *"))
        assert not any(r["properties"].get("name") == "Ghost" for r in rows)
    finally:
        c.close()
