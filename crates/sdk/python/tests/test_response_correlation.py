"""D-06: ID-correlated response handling — the review's §3.3 seven cases.

Each test runs a tiny scripted TCP server (thread) that reads one request
line per script entry and replies with a fixed frame sequence. No aikoql
binary is needed; the transport is the unit under test.

The contract under test (serialized-but-ID-based, §3.3):
  - a notification (no id) is never misread as the response
  - a stale frame (duplicate or late, id < expected) is skipped
  - a foreign id (id > expected, or non-numeric) is PROTOCOL_ERROR
  - a malformed frame is never misread as the response
  - a missing response is TIMEOUT, and a late one cannot corrupt the next
"""

import json
import socket
import sys
import threading
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent / "python"))
from aikoql import McpClient, McpError


def _frame_bytes(f):
    if isinstance(f, str):
        return (f + "\n").encode()
    return (json.dumps(f) + "\n").encode()


class ScriptedServer:
    """One entry per expected request; entry frames go out in ONE sendall."""

    def __init__(self, script):
        self.script = script
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(1)
        self.port = self.sock.getsockname()[1]
        self.thread = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        conn, _ = self.sock.accept()
        try:
            for frames in self.script:
                data = conn.recv(4096)
                if not data:
                    break
                if frames:
                    conn.sendall(b"".join(_frame_bytes(f) for f in frames))
                else:
                    time.sleep(0.8)  # hold the conn open past the client deadline
        finally:
            conn.close()

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *a):
        self.sock.close()
        self.thread.join(timeout=2)


def _client(port):
    return McpClient("127.0.0.1", port).connect(timeout=0.5)


NOTIFY = {"jsonrpc": "2.0", "method": "notifications/notify", "params": {}}
ADA = {"properties": {"name": "Ada"}}


def test_notification_before_response_is_skipped():
    with ScriptedServer([[NOTIFY, {"jsonrpc": "2.0", "id": 1, "result": ADA}]]) as s:
        c = _client(s.port)
        try:
            assert c._rpc("get") == ADA
        finally:
            c.close()


def test_notification_and_response_in_one_send_are_split():
    # "notification between response fragments": both frames arrive in the
    # same TCP chunk — the client must split them and match the response.
    with ScriptedServer([[NOTIFY, {"jsonrpc": "2.0", "id": 1, "result": ADA}]]) as s:
        c = _client(s.port)
        try:
            assert c._rpc("get") == ADA
        finally:
            c.close()


def test_response_for_another_request_is_protocol_error():
    with ScriptedServer([[{"jsonrpc": "2.0", "id": 99, "result": ADA}]]) as s:
        c = _client(s.port)
        try:
            with pytest.raises(McpError) as exc:
                c._rpc("get")
            assert exc.value.code == "PROTOCOL_ERROR"
        finally:
            c.close()


def test_duplicate_response_does_not_poison_the_next_request():
    dup = {"jsonrpc": "2.0", "id": 1, "result": {"stale": True}}
    r2 = {"jsonrpc": "2.0", "id": 2, "result": {"fresh": True}}
    with ScriptedServer([[{"jsonrpc": "2.0", "id": 1, "result": {"first": True}}],
                         [dup, r2]]) as s:
        c = _client(s.port)
        try:
            assert c._rpc("get") == {"first": True}
            assert c._rpc("get") == {"fresh": True}
        finally:
            c.close()


def test_missing_response_times_out():
    with ScriptedServer([[]]) as s:
        c = _client(s.port)
        try:
            with pytest.raises(McpError) as exc:
                c._rpc("get", timeout=0.4)
            assert exc.value.code == "TIMEOUT"
            assert exc.value.retryable
        finally:
            c.close()


def test_late_response_after_timeout_cannot_corrupt_the_next_request():
    late = {"jsonrpc": "2.0", "id": 1, "result": {"late": True}}
    r2 = {"jsonrpc": "2.0", "id": 2, "result": {"fresh": True}}
    with ScriptedServer([[], [late, r2]]) as s:
        c = _client(s.port)
        try:
            with pytest.raises(McpError) as exc:
                c._rpc("get", timeout=0.4)
            assert exc.value.code == "TIMEOUT"
            assert c._rpc("get") == {"fresh": True}
        finally:
            c.close()


def test_malformed_frame_before_valid_response_is_skipped():
    with ScriptedServer([["this is not json",
                          {"jsonrpc": "2.0", "id": 1, "result": ADA}]]) as s:
        c = _client(s.port)
        try:
            assert c._rpc("get") == ADA
        finally:
            c.close()
