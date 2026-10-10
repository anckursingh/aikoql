"""D-16 fault matrix (line wire): the §18 fault proxy sits between the SDK
and a real server and mangles the newline-delimited JSON-RPC wire (§7 of
the testing plan: every SDK passes the same matrix against the same
misbehaving server). Frame accounting: client line #1 = initialize, #2 =
the victim call, #3 = the follow-up (no HELLO/AUTH frames on the MCP wire).

The contracts GREEN must make hold (the proxy --wire line arm + the
client's MAX_FRAME cap and closed latch):
  drop-request/drop-response → victim TIMEOUT (retryable), follow-up ok
  delay-response → tight deadline TIMEOUT; generous deadline ok
  duplicate-response → both ok (id correlation skips the duplicate)
  reorder-response → victim TIMEOUT (response held), follow-up ok
  truncate-frame → victim TIMEOUT (the missing bytes never arrive),
    follow-up ok (the wire is self-delimiting)
  corrupt-frame → the line is noise per the frozen §3.3 semantics →
    victim TIMEOUT, follow-up ok
  inject-notification → both ok (an id-less frame is never a response)
  inject-stale-response → both ok (stale id skipped)
  close/half-close → victim ok, follow-up fails fast (never TIMEOUT),
    the client latches closed → UNAVAILABLE from then on
  slow-server → tight deadline TIMEOUT
  oversized-response → FRAME_TOO_LARGE before buffering past the 1 MiB
    cap (§19: a malicious server cannot cause unbounded client memory),
    follow-up UNAVAILABLE (the stream is desynced — latched)
"""

import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from aikoql import McpClient, McpError

ROOT = Path(__file__).resolve().parents[4]
TOKEN = "test-token"
TIGHT = 0.2


def _bin(name):
    for cand in (ROOT / "target" / "debug" / f"{name}.exe",
                 ROOT / "target" / "debug" / name):
        if cand.exists():
            return str(cand)
    pytest.skip(f"{name} binary not built. Run: cargo build -p {name}")


def _free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait(proc, port, what, stderr_pipe, timeout=15.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            err = stderr_pipe.read().decode(errors="replace")
            raise RuntimeError(f"{what} exited early ({proc.returncode}):\n{err}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"{what} never listened on 127.0.0.1:{port}")


@contextmanager
def fault_env(mode, *extra):
    """A real server + one fault-proxy instance (one fault mode). The
    port-probe idiom races under parallel legs → 3 spawn attempts."""
    mcp, proxy = _bin("aikoql-mcp"), _bin("aikoql-fault-proxy")
    last = None
    for _ in range(3):
        srv = px = None
        dbdir = None
        try:
            srv_port = _free_port()
            dbdir = tempfile.mkdtemp(prefix="aikoql-py-fault-")
            srv = subprocess.Popen(
                [mcp, "serve", os.path.join(dbdir, "db.aikoql"),
                 "--listen", f"127.0.0.1:{srv_port}",
                 "--tcp-token", f"{TOKEN}::admin"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            _wait(srv, srv_port, "aikoql-mcp", srv.stderr)
            px_port = _free_port()
            px = subprocess.Popen(
                [proxy, "--listen", f"127.0.0.1:{px_port}",
                 "--target", f"127.0.0.1:{srv_port}",
                 "--wire", "line", "--mode", mode, *extra],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            _wait(px, px_port, "aikoql-fault-proxy", px.stderr, timeout=10.0)
            break
        except RuntimeError as e:
            last = e
            for p in (px, srv):
                if p is not None and p.poll() is None:
                    p.terminate()
            for p in (px, srv):
                if p is not None:
                    try:
                        p.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        p.kill()
            if dbdir:
                shutil.rmtree(dbdir, ignore_errors=True)
    else:
        raise last
    try:
        yield f"127.0.0.1:{px_port}"
    finally:
        for p in (px, srv):
            if p.poll() is None:
                p.terminate()
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()
        shutil.rmtree(dbdir, ignore_errors=True)


def client(addr):
    host, _, port = addr.partition(":")
    c = McpClient(host, int(port), TOKEN).connect()
    c.initialize()
    return c


def test_drop_request():
    with fault_env("drop-request", "--n", "2") as addr:
        c = client(addr)
        with pytest.raises(McpError) as ei:
            c._rpc("ping", timeout=TIGHT)
        assert ei.value.code == "TIMEOUT" and ei.value.retryable
        c._rpc("ping")  # the follow-up proves the connection survived


def test_drop_response():
    with fault_env("drop-response", "--n", "2") as addr:
        c = client(addr)
        with pytest.raises(McpError) as ei:
            c._rpc("ping", timeout=TIGHT)
        assert ei.value.code == "TIMEOUT" and ei.value.retryable
        c._rpc("ping")


def test_delay_response():
    with fault_env("delay-response", "--from", "2", "--delay-ms", "400") as addr:
        c = client(addr)
        with pytest.raises(McpError) as ei:
            c._rpc("ping", timeout=TIGHT)
        assert ei.value.code == "TIMEOUT"
        c.close()
    with fault_env("delay-response", "--from", "2", "--delay-ms", "400") as addr:
        c = client(addr)
        c._rpc("ping", timeout=2.0)  # a generous deadline absorbs the delay


def test_duplicate_response():
    with fault_env("duplicate-response", "--n", "2") as addr:
        c = client(addr)
        c._rpc("ping")
        c._rpc("ping")  # the duplicate is a stale id — skipped


def test_reorder_response():
    with fault_env("reorder-response", "--n", "2") as addr:
        c = client(addr)
        with pytest.raises(McpError) as ei:
            c._rpc("ping", timeout=TIGHT)
        assert ei.value.code == "TIMEOUT"
        c._rpc("ping")  # request #3 releases response #2 — both arrive


def test_truncate_response():
    with fault_env("truncate-response", "--n", "2", "--bytes", "8") as addr:
        c = client(addr)
        with pytest.raises(McpError) as ei:
            c._rpc("ping", timeout=TIGHT)
        assert ei.value.code == "TIMEOUT"
        c._rpc("ping")


def test_corrupt_response():
    with fault_env("corrupt-response", "--n", "2") as addr:
        c = client(addr)
        with pytest.raises(McpError) as ei:
            c._rpc("ping", timeout=TIGHT)
        assert ei.value.code == "TIMEOUT"  # noise-skip, never a fast error
        c._rpc("ping")


def test_inject_notification():
    with fault_env("inject-notification", "--after", "2") as addr:
        c = client(addr)
        c._rpc("ping")
        c._rpc("ping")  # the id-less frame is never a response


def test_inject_stale_response():
    with fault_env("inject-stale-response", "--after", "2") as addr:
        c = client(addr)
        c._rpc("ping")
        c._rpc("ping")  # the replayed initialize response is stale — skipped


def test_close_after():
    with fault_env("close-after", "--n", "2") as addr:
        c = client(addr)
        c._rpc("ping")  # victim ok, then the proxy closes
        with pytest.raises(Exception) as ei:
            c._rpc("ping")
        assert not (isinstance(ei.value, McpError) and ei.value.code == "TIMEOUT")
        with pytest.raises(McpError) as ei:
            c._rpc("ping")
        assert ei.value.code == "UNAVAILABLE"  # latched: a dead conn never hangs


def test_half_close_after():
    with fault_env("half-close-after", "--n", "2") as addr:
        c = client(addr)
        c._rpc("ping")  # victim ok, then the proxy shutdown(Write)s
        with pytest.raises(Exception) as ei:
            c._rpc("ping")
        assert not (isinstance(ei.value, McpError) and ei.value.code == "TIMEOUT")
        with pytest.raises(McpError) as ei:
            c._rpc("ping")
        assert ei.value.code == "UNAVAILABLE"


def test_slow_server():
    with fault_env("slow-server", "--from", "2", "--bytes", "4",
                   "--delay-ms", "25") as addr:
        c = client(addr)
        with pytest.raises(McpError) as ei:
            c._rpc("ping", timeout=TIGHT)
        assert ei.value.code == "TIMEOUT"


def test_oversized_response():
    with fault_env("oversized-response", "--n", "2",
                   "--claim", str(64 * 1024 * 1024)) as addr:
        c = client(addr)
        with pytest.raises(McpError) as ei:
            c._rpc("ping", timeout=5.0)
        assert ei.value.code == "FRAME_TOO_LARGE"  # the 1 MiB cap, not 64 MiB
        with pytest.raises(McpError) as ei:
            c._rpc("ping")
        assert ei.value.code == "UNAVAILABLE"  # the stream is desynced — latched
