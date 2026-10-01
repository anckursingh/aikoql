"""D-09: the §3.4 connection pool and the §21 behavioral spec.

Scripted-server legs pin the pool mechanics (exhaustion →
RESOURCE_EXHAUSTED, transaction pinning + session reset on release, reuse
after a failed call); the real-server leg pins reconnect and auth reset
across a server restart. The factory is the seam: each call must return a
fully established session (connected + initialized) for one connection.
"""

import json
import socket
import threading
import time

import pytest

from aikoql import McpError, Pool
from aikoql.mcp_client import McpClient

from conftest import restartable_server
from scripted import ScriptedServer


def _envelope(rid, data):
    return {
        "jsonrpc": "2.0",
        "id": rid,
        "result": {
            "content": [{"type": "text", "text": json.dumps({"ok": True, "data": data})}]
        },
    }


def _responder(log):
    """Answers every tool a pooled connection can issue and logs the call."""

    def respond(req):
        log.append(req)
        name = req["params"]["name"]
        args = req["params"].get("arguments", {})
        if name == "health":
            data = {"status": "ok"}
        elif name == "aikoql":
            data = {"results": []}
        elif name == "txn_begin":
            data = {"txn_id": args["txn_id"], "snapshot_ts": 1000}
        elif name == "txn_stage":
            data = {"staged": 1}
        elif name == "txn_rollback":
            data = {"rolled_back": True}
        else:
            raise AssertionError(f"unexpected tool {name!r}")
        return [_envelope(req["id"], data)]

    return respond


def make_factory(respond, log):
    """A factory that starts a fresh scripted server per dial."""
    servers = []
    dials = []

    def factory():
        srv = ScriptedServer(respond=respond)
        srv.__enter__()
        servers.append(srv)
        dials.append(1)
        return McpClient("127.0.0.1", srv.port).connect()

    factory.servers = servers
    factory.dials = dials
    return factory


def test_pool_exhaustion_waits_then_resource_exhausted():
    log = []
    factory = make_factory(_responder(log), log)
    pool = Pool(factory, max_connections=2, acquire_timeout=0.2)
    a = pool.acquire()
    b = pool.acquire()
    assert len(factory.dials) == 2
    start = time.monotonic()
    with pytest.raises(McpError) as exc:
        pool.acquire()
    assert exc.value.code == "RESOURCE_EXHAUSTED"
    assert exc.value.retryable
    assert time.monotonic() - start >= 0.19  # it waited for the timeout
    assert len(factory.dials) == 2  # exhausted: no extra dial
    # A freed connection is handed to a waiter without a redial.
    got = {}

    def borrower():
        got["pc"] = pool.acquire()

    t = threading.Thread(target=borrower, daemon=True)
    t.start()
    time.sleep(0.05)
    a.release()
    t.join(timeout=2)
    assert not t.is_alive() and "pc" in got
    assert len(factory.dials) == 2
    got["pc"].release()
    b.release()
    pool.close()


def test_pool_release_resets_open_transaction():
    log = []
    factory = make_factory(_responder(log), log)
    pool = Pool(factory, max_connections=1)
    pc = pool.acquire()
    tx = pc.begin("abc")
    tx.execute("create", type_name="person", properties={"name": "ada"})
    pc.release()
    # Session reset: the open transaction was rolled back on release.
    assert any(call["params"]["name"] == "txn_rollback" for call in log)
    # The next borrower must NOT inherit the transaction (§21).
    pc2 = pool.acquire()
    assert len(factory.dials) == 1  # the same connection, no redial
    tx2 = pc2.begin()
    assert tx2.txn_id != "abc"
    assert len(tx2.txn_id) == 32
    pc2.release()
    pool.close()


def test_pool_connection_reusable_after_cancelled_call():
    log = []
    held = {"done": False}

    def respond(req):
        log.append(req)
        name = req["params"]["name"]
        if name == "aikoql" and not held["done"]:
            held["done"] = True
            return []  # hold past the deadline — the call times out
        return _responder(log)(req)

    factory = make_factory(respond, log)
    pool = Pool(factory, max_connections=1)
    pc = pool.acquire()
    pc.client._sock.settimeout(0.1)  # a deadline fires mid-call
    with pytest.raises(socket.timeout):
        pc.aikoql("MATCH person RETURN *")
    pc.client._sock.settimeout(5.0)
    pc.release()
    # The connection must be reusable (§21): same conn, no redial.
    pc2 = pool.acquire()
    assert len(factory.dials) == 1
    assert pc2.aikoql("MATCH person RETURN *")["results"] == []
    pc2.release()
    pool.close()


def test_pool_fill_min_idle():
    log = []
    factory = make_factory(_responder(log), log)
    pool = Pool(factory, max_connections=3, min_idle=2)
    pool.fill_min_idle()
    assert pool.stats()["idle"] == 2
    assert len(factory.dials) == 2
    pool.close()


def test_pool_reconnects_after_server_restart():
    with restartable_server() as (spawn, port, _db):
        def factory():
            c = McpClient("127.0.0.1", port, token="test-token").connect()
            c.initialize()  # auth reset: the factory re-establishes the session
            return c

        proc = spawn()
        pool = Pool(factory, max_connections=1, acquire_timeout=10.0,
                    health_check_interval=0.0)  # 0 = ping every borrow
        pc = pool.acquire()
        pc.remember("person", {"name": "ada"})
        pc.release()
        proc.terminate()
        proc.wait(timeout=5)  # the server dies
        # Borrow while the server is down: the pool keeps reconnecting...
        borrowed = {}

        def borrower():
            borrowed["pc"] = pool.acquire()

        t = threading.Thread(target=borrower, daemon=True)
        t.start()
        time.sleep(0.3)
        assert "pc" not in borrowed  # still reconnecting
        spawn()  # ... until the server returns on the same addr
        t.join(timeout=10)
        assert not t.is_alive() and "pc" in borrowed
        pc2 = borrowed["pc"]
        rows = pc2.aikoql('MATCH person WHERE name == "ada" RETURN *')
        assert rows["results"]  # the db survived the restart
        pc2.release()
        pool.close()
