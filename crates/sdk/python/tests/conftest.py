"""Shared test plumbing: temp db dirs removed at teardown, corpses purged at startup.

Killed runs never run teardown, and close() is a documented no-op in this
revision (drop-on-GC) — so the fixtures here `del` the client and collect
before rmtree (redb holds an exclusive file lock until the Rust struct
drops), and any aikoql-py-* temp dir older than a day is swept at session
start so the next run collects what a dead run left behind.
"""

import gc
import os
import shutil
import socket
import subprocess
import tempfile
import time
from pathlib import Path

import pytest

_PREFIX = "aikoql-py-"
_STALE_S = 24 * 3600

_ROOT = Path(__file__).parent.parent.parent.parent.parent


def find_binary():
    """The aikoql-mcp debug binary (the CI job builds it before the suite)."""
    candidates = [
        _ROOT / "target" / "debug" / "aikoql-mcp",
        _ROOT / "target" / "debug" / "aikoql-mcp.exe",
    ]
    for c in candidates:
        if c.exists():
            return str(c)
    pytest.skip("aikoql-mcp binary not built. Run: cargo build -p aikoql-mcp")


def _wait_ready(proc, host, port, timeout=15.0):
    """Poll until the server listens; surface its stderr if it exits early.

    CI-15: the old fixed sleep(0.5) raced the debug binary's startup, and
    when the server died the tests only saw a useless ConnectionRefused —
    the real error sat unread in the stderr pipe.
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            stderr = proc.stderr.read().decode(errors="replace")
            raise RuntimeError(
                f"aikoql-mcp exited early (code {proc.returncode}):\n{stderr}"
            )
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"aikoql-mcp did not listen on {host}:{port} within {timeout}s")


def _purge_stale() -> None:
    d = tempfile.gettempdir()
    cutoff = time.time() - _STALE_S
    try:
        entries = os.listdir(d)
    except OSError:
        return
    for name in entries:
        if not name.startswith(_PREFIX):
            continue
        p = os.path.join(d, name)
        try:
            if os.path.getmtime(p) < cutoff:
                if os.path.isdir(p):
                    shutil.rmtree(p, ignore_errors=True)
                else:
                    os.unlink(p)
        except OSError:
            continue


_purge_stale()


@pytest.fixture
def tmp_aikoql():
    from aikoql import aikoql

    d = tempfile.mkdtemp(prefix=_PREFIX)
    path = os.path.join(d, "test.redb")
    client = aikoql(path, salt=42)
    try:
        yield client
    finally:
        client.close()  # no-op this revision — release happens on drop
        del client
        gc.collect()  # drop the Rust struct so redb's file lock releases
        shutil.rmtree(d, ignore_errors=True)


@pytest.fixture
def mcp_server():
    """A real aikoql-mcp TCP server on a free port (the contract target).

    CI-15: the S-02 kernel refuses any pre-existing `*.redb` path (legacy
    v1 file — serve exits with a migration message), so the fixture
    reserves the path via mkstemp (CodeQL-safe) and unlinks it: `serve`
    then auto-creates the v2 store there. Readiness is polled, not slept
    (see _wait_ready). The name keeps the `aikoql-py-` prefix so a killed
    run's leftovers are swept by _purge_stale on the next session.
    """
    fd, db = tempfile.mkstemp(prefix=_PREFIX, suffix=".redb")
    os.close(fd)
    os.unlink(db)  # non-existent path → serve auto-creates aikoql-v2

    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()

    token = "test-token"
    proc = subprocess.Popen(
        [find_binary(), "serve", db, "--listen", f"127.0.0.1:{port}",
         "--tcp-token", f"{token}::admin"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        _wait_ready(proc, "127.0.0.1", port)
        yield f"127.0.0.1:{port}", db, token
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        # The v2 store is a directory at the path; tolerate either shape.
        if os.path.isdir(db):
            shutil.rmtree(db, ignore_errors=True)
        else:
            try:
                os.remove(db)
            except OSError:
                pass
        try:
            os.remove(db + ".audit.log")  # the kernel's sibling audit log
        except OSError:
            pass
