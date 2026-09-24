"""cl02 (P5-M12, ND-12) — the SDK ↔ server version compatibility contract.

The Python SDK fails fast with VERSION_MISMATCH against a server older than
MIN_SERVER_VERSION (parsed as dotted ints, so "0.0.1" < "0.1.19"). A newer
server is accepted (forward-compatible optimism — no upper bound).

MIN_SERVER_VERSION must equal the workspace version: any future workspace
bump turns test_min_server_version_matches_workspace RED by itself until the
SDK constant follows (the same self-pinning pattern as test_version_parity).

The contract lives in docs/version-compatibility.md.

Requires: the package importable (maturin develop or PYTHONPATH) and the
aikoql-mcp binary built (cargo build -p aikoql-mcp) for the happy path.
Run: pytest tests/test_version_contract.py -v
"""

import json
import os
import re
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

import pytest

# Ensure the package is importable.
sys.path.insert(0, str(Path(__file__).parent.parent / "python"))
from aikoql import McpClient, McpError  # noqa: E402

ROOT = Path(__file__).parent.parent.parent.parent.parent


def workspace_version() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'\[workspace\.package\]\s*version = "([^"]+)"', text)
    assert m, "workspace.package version not found in root Cargo.toml"
    return m.group(1)


def find_binary():
    candidates = [
        ROOT / "target" / "debug" / "aikoql-mcp",
        ROOT / "target" / "debug" / "aikoql-mcp.exe",
    ]
    for c in candidates:
        if c.exists():
            return str(c)
    pytest.skip("aikoql-mcp binary not built. Run: cargo build -p aikoql-mcp")


def test_min_server_version_matches_workspace():
    from aikoql import mcp_client

    assert mcp_client.MIN_SERVER_VERSION == workspace_version(), (
        f"mcp_client.MIN_SERVER_VERSION = {mcp_client.MIN_SERVER_VERSION!r}, "
        f"workspace = {workspace_version()!r} — bump the SDK constant"
    )


def test_version_compatibility_doc():
    doc = (ROOT / "docs" / "version-compatibility.md").read_text(encoding="utf-8")
    for needle in [
        "# Version Compatibility Contract",
        "Contract version: 1",
        "VERSION_MISMATCH",
        "fail-fast",
    ]:
        assert needle in doc, f"version-compatibility.md must document {needle!r}"


def test_sdk_rejects_old_server():
    """A server below MIN_SERVER_VERSION fails fast on initialize."""

    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    port = srv.getsockname()[1]

    def serve():
        conn, _ = srv.accept()
        f = conn.makefile("rwb")
        req = json.loads(f.readline())
        result = {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            # Deliberately ancient: the SDK minimum tracks the workspace.
            "serverInfo": {"name": "aikoql-mcp", "version": "0.0.1"},
        }
        f.write(
            (json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}) + "\n").encode()
        )
        f.flush()
        try:
            f.readline()  # the client closes on mismatch — EOF here
        except OSError:
            pass
        conn.close()

    t = threading.Thread(target=serve, daemon=True)
    t.start()
    try:
        c = McpClient("127.0.0.1", port).connect()
        try:
            with pytest.raises(McpError) as exc:
                c.initialize()
            assert exc.value.code == "VERSION_MISMATCH", (
                f"expected VERSION_MISMATCH, got {exc.value.code}"
            )
        finally:
            c.close()
    finally:
        srv.close()
        t.join(timeout=5)


@pytest.fixture
def mcp_server():
    """A real aikoql-mcp server (TCP) — the happy path of the contract."""
    binary = find_binary()
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()

    fd, db = tempfile.mkstemp(suffix=".redb")
    os.close(fd)
    token = "test-token"
    proc = subprocess.Popen(
        [binary, "serve", db, "--listen", f"127.0.0.1:{port}", "--tcp-token", f"{token}::admin"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    time.sleep(0.5)  # Wait for server to start.

    yield f"127.0.0.1:{port}", db, token

    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    try:
        os.remove(db)
    except OSError:
        pass


def test_sdk_accepts_current_server(mcp_server):
    host_port, _, token = mcp_server
    host, _, port = host_port.partition(":")
    c = McpClient(host, int(port), token).connect()
    try:
        result = c.initialize()
        assert result["serverInfo"]["version"] == workspace_version(), (
            f"server reported {result['serverInfo']['version']!r}, "
            f"workspace is {workspace_version()!r}"
        )
    finally:
        c.close()
