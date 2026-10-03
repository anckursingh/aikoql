"""Shared fixtures for aikoql-training tests.

make_example builds a valid canonical example (design §9 + the T-01
recon corrections) with its content-derived example_id filled in.

mcp_server spawns the real aikoql-mcp binary on a free port (the SDK
conftest pattern, CI-15 discipline: mkstemp-reserve then unlink so
serve auto-creates aikoql-v2, readiness polled not slept, teardown
removes the store and its audit log). Embedded mode is NOT used for
snapshot work: Agent.health() is a stub in embedded mode — journal_seq
and audit_hash exist only behind the MCP health tool (recon §4).
"""

import os
import shutil
import socket
import subprocess
import tempfile
import time
from pathlib import Path

import pytest

from aikoql_training.models import SCHEMA_VERSION, compute_id

_PREFIX = "aikoql-tr-"
_STALE_S = 24 * 3600

_ROOT = Path(__file__).parents[2]  # training/tests -> repo root


def find_binary():
    """The aikoql-mcp debug binary (the SDK conftest's contract)."""
    candidates = [
        _ROOT / "target" / "debug" / "aikoql-mcp",
        _ROOT / "target" / "debug" / "aikoql-mcp.exe",
    ]
    for c in candidates:
        if c.exists():
            return str(c)
    pytest.skip("aikoql-mcp binary not built. Run: cargo build -p aikoql-mcp")


def _wait_ready(proc, host, port, timeout=15.0):
    """Poll until the server listens; surface its stderr if it exits early
    (CI-15: the real error sits in the stderr pipe, not in a refused socket)."""
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


def _purge_stale():
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
def mcp_server():
    """A real aikoql-mcp TCP server on a free port, fresh store per test."""
    fd, db = tempfile.mkstemp(prefix=_PREFIX, suffix=".redb")
    os.close(fd)
    os.unlink(db)  # non-existent path -> serve auto-creates aikoql-v2

    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()

    token = "test-token::admin"
    proc = subprocess.Popen(
        [find_binary(), "serve", db, "--listen", f"127.0.0.1:{port}",
         "--tcp-token", token],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        _wait_ready(proc, "127.0.0.1", port)
        yield f"127.0.0.1:{port}", token
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


_BASE = {
    "schema_version": SCHEMA_VERSION,
    "generator_version": "0.1.0",
    "source": {
        "database_id": "acmepay",
        "snapshot_id": "snap-1",
        "knowledge_revision": "journal_seq=42;audit_hash=" + "0" * 64,
        "scenario_id": "factual:policy:p-01",
        "created_at": "2026-10-03T00:00:00Z",
    },
    "task": {"type": "grounded_qa", "difficulty": "factual", "requires": []},
    "input": {"question": "What is the owner of the settlement service?"},
    "semantic_target": {"operation": "query"},
    "query_target": {
        "language": "aikoql",
        "query": "MATCH Service WHERE name == 'settlement' RETURN owner",
    },
    "context": {"entities": [], "facts": [], "relations": [], "evidence": []},
    "expected": {"answer": "Payments Team", "koids": [], "evidence_ids": []},
    "policy": {"authorization_required": False},
    "labels": {
        "grounded": True,
        "answerable": True,
        "ambiguous": False,
        "contradictory": False,
    },
    "split_key": "policy:p-01",
}


def make_example(**overrides):
    """A valid example with overrides applied and a matching example_id."""
    example = {k: (dict(v) if isinstance(v, dict) else list(v) if isinstance(v, list) else v)
               for k, v in _BASE.items()}
    example.update(overrides)
    example["example_id"] = compute_id(example)
    return example
