"""Integration tests for the Aikoql Python MCP Client (MRFC-0040).

Requires: aikoql-mcp binary built (cargo build -p aikoql-mcp).
Run: pytest tests/test_mcp_client.py -v
"""

import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import pytest

# Ensure the package is importable.
sys.path.insert(0, str(Path(__file__).parent.parent / "python"))
from aikoql import Agent, McpClient, McpError


def find_binary():
    """Find the aikoql-mcp binary."""
    candidates = [
        Path(__file__).parent.parent.parent.parent.parent / "target" / "debug" / "aikoql-mcp",
        Path(__file__).parent.parent.parent.parent.parent / "target" / "debug" / "aikoql-mcp.exe",
    ]
    for c in candidates:
        if c.exists():
            return str(c)
    pytest.skip("aikoql-mcp binary not built. Run: cargo build -p aikoql-mcp")


@pytest.fixture
def mcp_server():
    """Start a temporary aikoql-mcp server on a free port."""
    import socket
    binary = find_binary()
    # Find a free port.
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()

    # CodeQL py/insecure-temporary-file: mkstemp → non-guessable name, 0600.
    fd, db = tempfile.mkstemp(suffix=".redb")
    os.close(fd)
    proc = subprocess.Popen(
        [binary, "--listen", f"127.0.0.1:{port}", db],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    time.sleep(0.5)  # Wait for server to start.

    yield f"127.0.0.1:{port}", db

    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    try:
        os.remove(db)
    except OSError:
        pass


class TestMcpClient:
    """Test the low-level MCP client."""

    def test_connect_and_initialize(self, mcp_server):
        host_port, _ = mcp_server
        host, _, port = host_port.partition(":")
        c = McpClient(host, int(port)).connect()
        try:
            result = c.initialize()
            assert result["protocolVersion"] == "2024-11-05"
            assert result["serverInfo"]["name"] == "aikoql-mcp"
        finally:
            c.close()

    def test_remember_and_get(self, mcp_server):
        host_port, _ = mcp_server
        host, _, port = host_port.partition(":")
        c = McpClient(host, int(port)).connect()
        try:
            c.initialize()
            r = c.remember("Task", {"title": "Fix login bug", "priority": 1},
                           subject="alice", note="test")
            assert "koid" in r
            assert r["version"] == 1

            ko = c.get(r["koid"], subject="alice")
            assert ko["properties"]["title"] == "Fix login bug"
            assert ko["type_name"] == "Task"
        finally:
            c.close()

    def test_session_identity(self, mcp_server):
        host_port, _ = mcp_server
        host, _, port = host_port.partition(":")
        c = McpClient(host, int(port)).connect()
        try:
            c.initialize()
            # Establish session.
            sess = c.session_init("pm-agent-7", run_id="run-42", roles=["admin"])
            assert sess["session"]["agent_id"] == "pm-agent-7"
            assert sess["established"] is True

            # Create without explicit subject — session identity used.
            r = c.remember("Task", {"title": "From session"})
            assert "koid" in r
        finally:
            c.close()

    def test_health(self, mcp_server):
        host_port, _ = mcp_server
        host, _, port = host_port.partition(":")
        c = McpClient(host, int(port)).connect()
        try:
            c.initialize()
            h = c.health()
            assert h["status"] == "healthy"
            assert h["ready"] is True
            assert "journal_lag_ms" in h
            assert "connection_pool" in h
        finally:
            c.close()

    def test_structured_error(self, mcp_server):
        host_port, _ = mcp_server
        host, _, port = host_port.partition(":")
        c = McpClient(host, int(port)).connect()
        try:
            c.initialize()
            # Try to get a nonexistent KOID.
            with pytest.raises(McpError) as exc:
                c.get("00000000000000000000000000000000", subject="alice")
            assert exc.value.code == "NOT_FOUND"
            assert not exc.value.retryable
            assert exc.value.suggestion
        finally:
            c.close()


class TestAgentUnified:
    """Test the unified Agent.connect() interface with MCP mode."""

    def test_agent_connect_server_mode(self, mcp_server):
        host_port, db_path = mcp_server
        a = Agent.connect(host_port)
        try:
            assert a.mode == "mcp"
            r = a.remember("Task", {"title": "Unified test"}, subject="alice")
            assert "koid" in r
            ko = a.get(r["koid"], subject="alice")
            assert ko["properties"]["title"] == "Unified test"
        finally:
            a.close()

    def test_agent_health(self, mcp_server):
        host_port, _ = mcp_server
        a = Agent.connect(host_port)
        try:
            h = a.health()
            assert h["status"] == "healthy"
        finally:
            a.close()


class TestStreaming:
    """Test aikoql/stream incremental results (MRFC-0040 #5)."""

    def test_aikoql_stream(self, mcp_server):
        host_port, _ = mcp_server
        host, _, port = host_port.partition(":")
        c = McpClient(host, int(port)).connect()
        try:
            c.initialize()

            # Create 50 objects (single chunk).
            for i in range(50):
                c.remember("Item", {"idx": i, "label": f"item-{i}"}, subject="alice")

            # Stream query.
            chunks = list(c.aikoql_stream("MATCH Item RETURN *", subject="alice"))
            assert len(chunks) >= 1, f"Expected at least 1 chunk, got {len(chunks)}"

            all_results = []
            for chunk in chunks:
                results = chunk.get("results", [])
                all_results.extend(results)

            assert len(all_results) == 50
            koids = {r["koid"] for r in all_results}
            assert len(koids) == 50, "All results should have unique KOIDs"
        finally:
            c.close()
