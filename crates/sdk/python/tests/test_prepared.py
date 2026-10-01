"""D-10: the §3.6 prepared statement and the §22 lifecycle spec.

prepare/bind/execute/close. The initial implementation compiles the AikoQL
query client-side on every execute — the abstraction precedes a native
prepare protocol, so the statement holds no server-side plan (nothing to
invalidate, nothing lost across a server restart). Scripted-server legs pin
the bind/validate/substitute mechanics; the real-server legs pin transaction
interaction and the re-compile after a restart.
"""

import json
import threading
import time

import pytest

from aikoql import McpError, PreparedStatement
from aikoql.mcp_client import McpClient

from conftest import restartable_server
from scripted import ScriptedServer
from test_pool import _envelope, _responder


def _prepared_responder(log):
    """Answers aikoql + remember and logs every call (the schema-invalidation
    leg needs both surfaces)."""

    def respond(req):
        log.append(req)
        name = req["params"]["name"]
        if name == "aikoql":
            data = {"results": [{"koid": "k1", "type_name": "person",
                                 "properties": {"name": "ada"}}]}
        elif name == "remember":
            data = {"koid": "k2", "version": 1}
        else:
            raise AssertionError(f"unexpected tool {name!r}")
        return [_envelope(req["id"], data)]

    return respond


def _invalid_query_responder():
    """A server that rejects the statement at compile time (invalid
    statement, server-side arm of §22)."""

    def respond(req):
        if req["params"]["name"] != "aikoql":
            raise AssertionError(f"unexpected tool {req['params']['name']!r}")
        text = json.dumps({"ok": False, "error": {
            "code": "INVALID_QUERY", "message": "could not compile the query",
        }})
        return [{"jsonrpc": "2.0", "id": req["id"],
                 "result": {"content": [{"type": "text", "text": text}]}}]

    return respond


def _client_with(respond):
    srv = ScriptedServer(respond=respond)
    srv.__enter__()
    return McpClient("127.0.0.1", srv.port).connect()


def _aikoql_queries(log):
    return [c["params"]["arguments"]["query"]
            for c in log if c["params"]["name"] == "aikoql"]


def test_prepared_lifecycle_bind_execute_execute_close():
    log = []
    client = _client_with(_prepared_responder(log))
    ps = client.prepare("MATCH person WHERE name == :who RETURN *")
    bound = ps.bind({"who": "ada"})
    r1 = bound.execute()
    r2 = bound.execute()  # a bound statement is re-executable
    assert r1["results"][0]["properties"]["name"] == "ada"
    assert r2["results"][0]["properties"]["name"] == "ada"
    assert _aikoql_queries(log) == [
        'MATCH person WHERE name == "ada" RETURN *',
        'MATCH person WHERE name == "ada" RETURN *',
    ]
    ps.close()
    with pytest.raises(McpError) as exc:
        ps.execute({"who": "ada"})
    assert exc.value.code == "INVALID_ARGUMENT"
    with pytest.raises(McpError) as exc:
        bound.execute()  # the bound statement rides the closed statement
    assert exc.value.code == "INVALID_ARGUMENT"


def test_prepared_parameter_type_mismatch():
    client = _client_with(_prepared_responder([]))
    ps = client.prepare("MATCH person WHERE age == :age RETURN *")
    with pytest.raises(McpError) as exc:
        ps.execute({"age": [1, 2]})  # containers are not bindable scalars
    assert exc.value.code == "INVALID_ARGUMENT"


def test_prepared_wrong_parameter_count():
    client = _client_with(_prepared_responder([]))
    ps = client.prepare("MATCH person WHERE name == :who RETURN *")
    with pytest.raises(McpError) as exc:
        ps.execute({})  # missing :who
    assert exc.value.code == "INVALID_ARGUMENT"
    with pytest.raises(McpError) as exc:
        ps.execute({"who": "ada", "extra": 1})  # unknown binding
    assert exc.value.code == "INVALID_ARGUMENT"


def test_prepared_invalid_statement():
    client = _client_with(_prepared_responder([]))
    with pytest.raises(McpError) as exc:
        client.prepare("   ")  # client-side arm
    assert exc.value.code == "INVALID_ARGUMENT"
    # A syntactically bad query compiles at the server on execute and the
    # server's error surfaces as-is.
    client2 = _client_with(_invalid_query_responder())
    ps = client2.prepare("MATCH WHERE garbage")
    with pytest.raises(McpError) as exc:
        ps.execute({})
    assert exc.value.code == "INVALID_QUERY"


def test_prepared_schema_invalidation_recompiles():
    log = []
    client = _client_with(_prepared_responder(log))
    ps = client.prepare("MATCH person WHERE name == :who RETURN *")
    ps.execute({"who": "ada"})
    client.remember("pet", {"name": "rex"})  # the schema changes under it
    ps.execute({"who": "ada"})  # re-compiles: no cached plan to invalidate
    assert len(_aikoql_queries(log)) == 2


def test_prepared_transaction_interaction(mcp_server):
    host_port, _db, token = mcp_server
    host, port = host_port.split(":")
    client = McpClient(host, int(port), token=token).connect()
    client.initialize()
    ps = client.prepare("MATCH person WHERE name == :who RETURN *")
    assert ps.execute({"who": "ada"})["results"] == []  # nothing yet
    tx = client.begin()
    tx.execute("create", type_name="person", properties={"name": "ada"})
    tx.commit()
    rows = ps.execute({"who": "ada"})["results"]  # a prepared read sees it
    assert rows and rows[0]["properties"]["name"] == "ada"


def test_prepared_concurrent_use():
    log = []
    client = _client_with(_prepared_responder(log))
    ps = client.prepare("MATCH person WHERE name == :who RETURN *")
    bound = ps.bind({"who": "ada"})
    errors = []

    def run():
        try:
            for _ in range(5):
                assert bound.execute()["results"][0]["properties"]["name"] == "ada"
        except Exception as exc:  # noqa: BLE001 - collected, not swallowed
            errors.append(exc)

    threads = [threading.Thread(target=run) for _ in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert not errors
    assert len(_aikoql_queries(log)) == 20


def test_prepared_close_then_execute_refused():
    client = _client_with(_prepared_responder([]))
    ps = client.prepare("MATCH person RETURN *")
    ps.close()
    with pytest.raises(McpError) as exc:
        ps.execute({})
    assert exc.value.code == "INVALID_ARGUMENT"
    # close is per-statement: a fresh prepare works on the same client
    ps2 = client.prepare("MATCH person RETURN *")
    ps2.execute({})


def test_prepared_recompiles_after_server_restart():
    with restartable_server() as (spawn, port, _db):
        proc = spawn()
        client = McpClient("127.0.0.1", port, token="test-token").connect()
        client.initialize()
        ps = client.prepare("MATCH person WHERE name == :who RETURN *")
        client.remember("person", {"name": "ada"})
        assert ps.execute({"who": "ada"})["results"]
        client.close()
        proc.terminate()
        proc.wait(timeout=5)  # the server dies
        # The pool reconnect leg (test_pool) blocks the borrow and returns
        # once the server is back; the prepared statement held no
        # server-side plan, so a fresh prepare on the new connection
        # compiles it afresh against the restarted server.
        from aikoql.pool import Pool

        def factory():
            c = McpClient("127.0.0.1", port, token="test-token").connect()
            c.initialize()
            return c

        pool = Pool(factory, max_connections=1, acquire_timeout=10.0,
                    health_check_interval=0.0)
        borrowed = {}

        def borrower():
            borrowed["pc"] = pool.acquire()

        t = threading.Thread(target=borrower, daemon=True)
        t.start()
        time.sleep(0.3)
        assert "pc" not in borrowed  # still reconnecting
        spawn()  # the server returns on the same addr
        t.join(timeout=10)
        assert not t.is_alive() and "pc" in borrowed
        pc = borrowed["pc"]
        ps2 = pc.prepare("MATCH person WHERE name == :who RETURN *")
        assert ps2.execute({"who": "ada"})["results"]  # the db survived
        pc.release()
        pool.close()
