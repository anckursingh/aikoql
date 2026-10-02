"""D-16 §10: the Python fuzz estate — hypothesis property tests (L1/L3)
and the §12 protocol state machine (L2: DISCONNECTED→CONNECTED→
INITIALIZED→TRANSACTION→STREAMING→CLOSED).

The §17 property rules the whole file: illegal transition sequences must
error deterministically — the exact frozen exception type and code — and
never hang, panic, or let a success escape an illegal step. Everything
runs in-process against FakeSock; the socket layer itself is covered by
the transport tests.
"""

import json
import socket

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st
from hypothesis.stateful import (
    RuleBasedStateMachine,
    invariant,
    precondition,
    rule,
)

from aikoql.mcp_client import (
    MAX_FRAME,
    MIN_SERVER_VERSION,
    McpClient,
    McpError,
    _parse_version,
)


class FakeSock:
    """In-process stand-in for the client's socket: sendall() decodes the
    frame and hands it to respond(), whose frames buffer for recv(). An
    empty buffer raises socket.timeout — the client's own deadline logic
    classifies it (TIMEOUT, or the call's socket.timeout without one).
    `raw` serves arbitrary bytes (the over-cap leg), `eof` kills the
    transport once.
    """

    def __init__(self, respond):
        self.respond = respond
        self._buf = b""
        self.raw = b""
        self.eof = False
        self.sent = []
        self.timeout = None

    def sendall(self, data):
        self.sent.append(data)
        for f in self.respond(json.loads(data.decode().strip())):
            if isinstance(f, str):
                self._buf += (f + "\n").encode()
            else:
                self._buf += (json.dumps(f) + "\n").encode()

    def recv(self, n):
        if self.eof:
            self.eof = False
            return b""
        if self.raw:
            chunk, self.raw = self.raw[:n], self.raw[n:]
            return chunk
        if not self._buf:
            raise socket.timeout()
        chunk, self._buf = self._buf[:n], self._buf[n:]
        return chunk

    def settimeout(self, t):
        self.timeout = t

    def close(self):
        pass


def _armed(respond):
    """A client whose transport is the fake (the model's connect)."""
    c = McpClient()
    c._sock = FakeSock(respond)
    c._dead = False
    return c


def ok_envelope(data):
    """A respond() that answers any request with the tool envelope."""
    def respond(req):
        return [{"id": req.get("id"),
                 "result": {"content": [{"text": json.dumps(data)}]}}]
    return respond


# -- L1/L3 property tests ---------------------------------------------

@settings(max_examples=200, deadline=None)
@given(st.text())
def test_parse_version_property(v):
    """The frozen mirror: int(seg) per segment, -1 on failure."""
    parts = _parse_version(v)
    assert isinstance(parts, tuple)
    assert len(parts) == len(v.split("."))
    for seg, part in zip(v.split("."), parts):
        try:
            want = int(seg)
        except ValueError:
            want = -1
        assert part == want


@settings(max_examples=200, deadline=None)
@given(st.dictionaries(
    keys=st.text(), max_size=4,
    values=st.text() | st.integers() | st.booleans() | st.none()))
def test_mcp_error_property(err):
    """The frozen from_response defaults, restated."""
    e = McpError.from_response(err)
    assert e.code == err.get("code", "INTERNAL")
    assert e.message == err.get("message", "unknown error")
    assert e.retryable == err.get("retryable", False)
    assert e.suggestion == err.get("suggestion", "")
    assert str(e) == f"[{e.code}] {e.message}"


@settings(max_examples=200, deadline=None)
@given(st.dictionaries(
    keys=st.text(), max_size=4,
    values=st.text() | st.integers() | st.booleans() | st.none()))
def test_envelope_property(env):
    """A tool envelope decode either raises a classified error or returns
    the data payload — ok:false can never escape as success."""
    c = _armed(ok_envelope({"ok": True, "data": env}))
    out = c.call_tool("probe", {})
    assert isinstance(out, dict)
    assert out == env
    # The same script, but the tool reports failure — the error path.
    c2 = _armed(ok_envelope({"ok": False, "data": env}))
    with pytest.raises(McpError):
        c2.call_tool("probe", {})


@settings(max_examples=200, deadline=None)
@given(st.text())
def test_envelope_garbage_property(text):
    """Arbitrary payload text: success (no exception) must be a dict;
    everything else raises one of the frozen classifications."""
    c = _armed(ok_envelope({"ok": True, "data": {"text": text}}))
    try:
        out = c.call_tool("probe", {})
    except (McpError, json.JSONDecodeError, AttributeError, TypeError):
        return
    assert isinstance(out, dict)


@settings(max_examples=100, deadline=None)
@given(st.binary(max_size=MAX_FRAME + 4096))
def test_frame_cap_property(junk):
    """The §19 bound: FRAME_TOO_LARGE exactly when the junk runs past the
    cap, the client latches, and the next call fails UNAVAILABLE."""
    c = _armed(lambda req: [])
    c._sock.raw = junk
    try:
        c.call_tool("probe", {})
        raised = None
    except McpError as e:
        raised = e.code
    except socket.timeout:
        raised = "TIMEOUT"
    if len(junk) > MAX_FRAME:
        assert raised == "FRAME_TOO_LARGE"
        assert c._dead
        with pytest.raises(McpError) as e2:
            c.call_tool("probe", {})
        assert e2.value.code == "UNAVAILABLE"
    else:
        assert raised in (None, "TIMEOUT")


# -- the §12 protocol state machine -----------------------------------

class ProtocolMachine(RuleBasedStateMachine):
    """The six-state lifecycle. Every rule asserts the frozen §17
    classification for its step — including the illegal transitions, which
    must refuse before sending anything."""

    def __init__(self):
        super().__init__()
        self.client = McpClient()
        self.state = "DISCONNECTED"
        self.responder = lambda req: []
        self.fake = None
        self.txn = None
        self.gen = None

    # -- transport legs ------------------------------------------------

    @rule()
    @precondition(lambda self: self.state == "DISCONNECTED")
    def connect(self):
        # One transport per run: a latched or closed client never reconnects
        # (the SDK has no recovery path — its read-ahead buffer survives,
        # so the model must not fabricate one either).
        self.fake = FakeSock(lambda req: self.responder(req))
        self.client._sock = self.fake
        self.state = "CONNECTED"

    @rule()
    @precondition(lambda self: self.state in
                  ("CONNECTED", "INITIALIZED", "TRANSACTION", "STREAMING"))
    def close(self):
        self.client.close()
        self.state = "CLOSED"

    @rule()
    @precondition(lambda self: self.state in ("DISCONNECTED", "CLOSED"))
    def call_without_transport(self):
        """The §17 illegal transition: any call from DISCONNECTED/CLOSED
        must refuse with UNAVAILABLE before touching the socket."""
        before = len(self.fake.sent) if self.fake else 0
        with pytest.raises(McpError) as e:
            self.client.get("x")
        assert e.value.code == "UNAVAILABLE"
        if self.fake:
            assert len(self.fake.sent) == before

    # -- initialize legs -----------------------------------------------

    @rule()
    @precondition(lambda self: self.state == "CONNECTED")
    def initialize_ok(self):
        self.responder = lambda req: [
            {"id": req.get("id"),
             "result": {"serverInfo": {"version": MIN_SERVER_VERSION}}}]
        self.client.initialize()
        self.state = "INITIALIZED"

    @rule()
    @precondition(lambda self: self.state == "CONNECTED")
    def initialize_old_server(self):
        self.responder = lambda req: [
            {"id": req.get("id"),
             "result": {"serverInfo": {"version": "0.0.1"}}}]
        with pytest.raises(McpError) as e:
            self.client.initialize()
        assert e.value.code == "VERSION_MISMATCH"
        # still CONNECTED — the handshake failed, the transport is alive

    @rule()
    @precondition(lambda self: self.state == "CONNECTED")
    def initialize_silent_server(self):
        """initialize has no timeout param: a silent server surfaces as the
        socket timeout itself (the frozen classification)."""
        self.responder = lambda req: []
        with pytest.raises(socket.timeout):
            self.client.initialize()
        # still CONNECTED

    # -- transaction legs ----------------------------------------------

    @rule()
    @precondition(lambda self: self.state == "INITIALIZED")
    def begin_ok(self):
        self.responder = ok_envelope({"ok": True, "data": {"txn_id": "t"}})
        self.txn = self.client.begin(txn_id="t")
        self.state = "TRANSACTION"

    @rule()
    @precondition(lambda self: self.state == "TRANSACTION")
    def stage_ok(self):
        self.responder = ok_envelope({"ok": True, "data": {}})
        self.txn.execute("create", "thing")
        # still TRANSACTION

    @rule()
    @precondition(lambda self: self.state == "TRANSACTION")
    def commit_ok(self):
        self.responder = ok_envelope({"ok": True, "data": {}})
        self.txn.commit()
        self.state = "INITIALIZED"

    @rule()
    @precondition(lambda self: self.state == "TRANSACTION")
    def rollback_ok(self):
        self.responder = ok_envelope({"ok": True, "data": {}})
        self.txn.rollback()
        self.state = "INITIALIZED"

    @rule()
    @precondition(lambda self: self.state == "TRANSACTION")
    def commit_then_reuse(self):
        """The phantom-commit guard: a closed handle refuses."""
        self.responder = ok_envelope({"ok": True, "data": {}})
        self.txn.commit()
        self.state = "INITIALIZED"
        with pytest.raises(McpError) as e:
            self.txn.execute("create", "thing")
        assert e.value.code == "INVALID_ARGUMENT"

    # -- streaming legs -------------------------------------------------

    @rule()
    @precondition(lambda self: self.state == "INITIALIZED")
    def stream_open(self):
        head = {"stream_id": "s", "total_chunks": 2, "results": []}
        self.responder = lambda req: [
            {"id": req.get("id"), "result": head},
            {"method": "notifications/notify",
             "params": {"stream_id": "s", "done": True}}]
        self.gen = self.client.aikoql_stream("MATCH x")
        assert next(self.gen) == head
        self.state = "STREAMING"

    @rule()
    @precondition(lambda self: self.state == "STREAMING")
    def stream_next_done(self):
        chunk = next(self.gen)
        assert chunk["params"]["done"]
        self.state = "INITIALIZED"

    @rule()
    @precondition(lambda self: self.state == "STREAMING")
    def stream_next_timeout(self):
        """A truncated stream surfaces as the socket timeout — the frozen
        classification for the deadline-less stream reads."""
        with pytest.raises(socket.timeout):
            next(self.gen)
        self.state = "INITIALIZED"  # the generator died, the client lives

    @rule()
    @precondition(lambda self: self.state == "STREAMING")
    def stream_abandon(self):
        self.gen.close()
        self.state = "INITIALIZED"

    # -- protocol legs (§3.3) -------------------------------------------

    @rule()
    @precondition(lambda self: self.state == "INITIALIZED")
    def protocol_stale_id(self):
        self.responder = lambda req: [
            {"id": (req.get("id") or 0) - 1, "result": {}},
            {"id": req.get("id"), "result": {"ok": True, "data": {}}}]
        self.client.get("x")  # the stale id is skipped, the real one lands

    @rule()
    @precondition(lambda self: self.state == "INITIALIZED")
    def protocol_impossible_id(self):
        self.responder = lambda req: [
            {"id": (req.get("id") or 0) + 1, "result": {}}]
        with pytest.raises(McpError) as e:
            self.client.get("x")
        assert e.value.code == "PROTOCOL_ERROR"

    @rule()
    @precondition(lambda self: self.state == "INITIALIZED")
    def protocol_non_numeric_id(self):
        self.responder = lambda req: [{"id": "nope", "result": {}}]
        with pytest.raises(McpError) as e:
            self.client.get("x")
        assert e.value.code == "PROTOCOL_ERROR"

    @rule()
    @precondition(lambda self: self.state == "INITIALIZED")
    def protocol_malformed_frame(self):
        self.responder = lambda req: [
            "garbage",
            {"id": req.get("id"), "result": {"ok": True, "data": {}}}]
        self.client.get("x")  # garbage is skipped, never misread

    # -- failure legs ---------------------------------------------------

    @rule()
    @precondition(lambda self: self.state in ("CONNECTED", "INITIALIZED"))
    def rpc_timeout(self):
        self.responder = lambda req: []
        with pytest.raises(McpError) as e:
            self.client._rpc("probe", {}, timeout=0.02)
        assert e.value.code == "TIMEOUT"
        assert e.value.retryable
        assert not self.client._dead  # TIMEOUT never latches (§19)

    @rule()
    @precondition(lambda self: self.state in
                  ("CONNECTED", "INITIALIZED", "TRANSACTION"))
    def transport_dies(self):
        self.fake.eof = True
        with pytest.raises(McpError) as e:
            self.client.get("x")
        assert e.value.code == "UNAVAILABLE"
        assert self.client._dead
        self.state = "CLOSED"

    @rule()
    @precondition(lambda self: self.state in
                  ("CONNECTED", "INITIALIZED", "TRANSACTION"))
    def oversized_line(self):
        self.fake.raw = b"x" * (MAX_FRAME + 1)
        with pytest.raises(McpError) as e:
            self.client.get("x")
        assert e.value.code == "FRAME_TOO_LARGE"
        assert self.client._dead
        self.state = "CLOSED"

    # -- invariants -----------------------------------------------------

    @invariant()
    def buffer_bound(self):
        assert len(self.client._buf) <= MAX_FRAME + 4096

    @invariant()
    def states_imply_transport(self):
        if self.state == "DISCONNECTED":
            assert self.client._sock is None
        if self.state == "CLOSED":
            assert self.client._sock is None or self.client._dead


TestProtocolMachine = ProtocolMachine.TestCase
TestProtocolMachine.settings = settings(max_examples=30, deadline=None)
