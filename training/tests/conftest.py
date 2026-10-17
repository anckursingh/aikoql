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
from copy import deepcopy as _deepcopy
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


def _wait_ready(proc, host, port, errf, timeout=15.0):
    """Poll until the server listens; surface its stderr if it exits early
    (CI-15: the real error sits in the log file, not in a refused socket)."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            with open(errf, "r", errors="replace") as f:
                stderr = f.read()
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


def _serve(tcp_tokens):
    """Spawn a real aikoql-mcp TCP server on a free port with the given
    token specs (repeated --tcp-token flags accumulate). Yields
    (host, tokens) once ready; fresh store per spawn."""
    fd, db = tempfile.mkstemp(prefix=_PREFIX, suffix=".redb")
    os.close(fd)
    os.unlink(db)  # non-existent path -> serve auto-creates aikoql-v2

    # T-45: keep corpus seeding (~150 tool calls per slice) clear of the
    # 300 calls/min default budget — test servers disable the limiter.
    fd, cfg = tempfile.mkstemp(prefix=_PREFIX, suffix=".toml")
    os.close(fd)
    with open(cfg, "w", encoding="utf-8") as f:
        f.write("[rate_limit]\nenabled = false\n")

    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()

    # CI-15 + T-14: server logs go to a FILE, never an undrained pipe —
    # the tantivy commit storm after corpus seeding writes thousands of
    # lines; a full pipe blocks the logging thread, and the next handler
    # that logs ("client connected") stalls before it ever answers
    # initialize — the validator then times out. A file has no
    # backpressure and is still read for the CI-15 early-exit diagnostic.
    fdf, errf = tempfile.mkstemp(prefix=_PREFIX, suffix=".log")
    log_handle = os.fdopen(fdf, "wb")
    proc = subprocess.Popen(
        [find_binary(), "--config", cfg, "serve", db,
         "--listen", f"127.0.0.1:{port}"]
        + [arg for tok in tcp_tokens for arg in ("--tcp-token", tok)],
        stdout=subprocess.DEVNULL,  # TCP mode: all server logs go to stderr
        stderr=log_handle,
    )
    try:
        _wait_ready(proc, "127.0.0.1", port, errf)
        yield f"127.0.0.1:{port}", tcp_tokens
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        log_handle.close()
        try:
            os.remove(errf)
        except OSError:
            pass
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
        try:
            os.remove(cfg)
        except OSError:
            pass


@pytest.fixture
def mcp_server():
    """A real aikoql-mcp TCP server on a free port, fresh store per test.

    The server registers the base token as the lookup key (SDK conftest
    pattern): spawn with TOKEN::admin, the client sends the base TOKEN.
    """
    for host, _specs in _serve(["test-token::admin"]):
        yield host, "test-token"


@pytest.fixture
def mcp_server_two_tokens():
    """One server, two TCP identities in DIFFERENT tenants: alice (acme,
    admin) owns the knowledge, bob (other, viewer) must be denied.

    Recon: over TCP every authenticated connection gets agent_id
    "tcp-agent" (transport.rs), so the subject name is
    connection-invariant and same-tenant tokens share it — the denial
    boundary is the TENANT (tcp_tenant_isolation_across_tokens, the
    house-pinned pattern). CTX-001's subject-level denial is the stdio
    behavior; over TCP the equivalent cell is cross-tenant. Token
    spec: TOKEN[:TENANT[:ROLE1,ROLE2]]."""
    for host, _specs in _serve(["alice-tok:acme:admin", "bob-tok:other:viewer"]):
        yield {"host": host, "alice": "alice-tok", "bob": "bob-tok"}


_KOID = "a" * 32

_BASE = {
    "schema_version": SCHEMA_VERSION,
    "generator_version": "0.2.0",
    "source": {
        "database_id": "acmepay",
        "snapshot_id": "snap-1",
        "knowledge_revision": "journal_seq=42;audit_hash=" + "0" * 64,
        "scenario_id": "factual:policy:p-01",
        "created_at": "2026-10-03T00:00:00Z",
    },
    "task": {"type": "grounded_qa", "difficulty": "factual", "requires": []},
    "input": {"question": "What is the owner of the settlement service?"},
    "semantic_target": {
        "operation": "query",
        "intent": "grounded_qa",
        "entities": [{"koid": _KOID, "role": "subject"}],
        "requirements": ["owner"],
        "plan": {"steps": [
            {"op": "resolve_entity", "koid": _KOID},
            {"op": "project", "properties": ["owner"]},
        ]},
    },
    "query_target": {
        "language": "aikoql",
        # T-05: text string literals are double-quoted only (lexer.rs).
        "query": 'MATCH Service WHERE name == "settlement" RETURN owner',
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
    """A valid example with overrides applied and a matching example_id.

    A deep copy: tests mutate nested plan/entities structures and a
    shallow copy shares them, poisoning _BASE for every later test."""
    example = _deepcopy(_BASE)
    example.update(overrides)
    example["example_id"] = compute_id(example)
    return example
