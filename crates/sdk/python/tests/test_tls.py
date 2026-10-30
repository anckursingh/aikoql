"""The SDK over TLS, against the real server started with --tls-cert /
--tls-key (the MCP crate's test certificates, SAN IP:127.0.0.1): a client
built with an ``ssl.SSLContext`` authenticates and round-trips a write, the
Agent facade takes the same context, and a server outside the context's
trust anchors is refused at connect — before the token is sent."""

import os
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent / "python"))
from aikoql import Agent, McpClient  # noqa: E402

from conftest import _wait_ready, find_binary  # noqa: E402

FX = Path(__file__).resolve().parents[3] / "services/api/mcp/tests/fixtures/tls"


@pytest.fixture
def tls_server():
    db = tempfile.mkdtemp(prefix="aikoql-py-tls-")
    shutil.rmtree(db)  # a fresh path: serve creates the aikoql-v2 directory
    probe = socket.socket()
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
    probe.close()
    proc = subprocess.Popen(
        [find_binary(), "serve", db, "--listen", f"127.0.0.1:{port}",
         "--tcp-token", "py-tls::admin",
         "--tls-cert", str(FX / "server.pem"), "--tls-key", str(FX / "server.key")],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    try:
        _wait_ready(proc, "127.0.0.1", port)
        yield port
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(db, ignore_errors=True)


def _trust(ca: str) -> ssl.SSLContext:
    return ssl.create_default_context(cafile=str(FX / ca))


def test_mcp_over_tls(tls_server):
    c = McpClient("127.0.0.1", tls_server, token="py-tls", tls=_trust("ca.pem")).connect()
    try:
        c.initialize()
        put = c.remember("note", {"n": 7}, subject="admin")
        got = c.get(put["koid"], subject="admin")
        assert got["properties"]["n"] == 7
    finally:
        c.close()


def test_agent_connect_over_tls(tls_server):
    agent = Agent.connect(("127.0.0.1", tls_server), token="py-tls", tls=_trust("ca.pem"))
    assert agent._backend.call_tool("health", {}) is not None


def test_an_untrusted_server_is_refused(tls_server):
    with pytest.raises(ssl.SSLError):
        McpClient("127.0.0.1", tls_server, token="py-tls", tls=_trust("other-ca.pem")).connect()


def test_the_client_never_offers_below_tls_1_2():
    """A caller's context that still allows TLS 1.0/1.1 (Python 3.9's
    create_default_context can, per the system OpenSSL) is raised to a
    TLS 1.2 floor — the SDK never offers a broken protocol. A stricter
    floor is left as it is."""
    import warnings

    weak = ssl.create_default_context(cafile=str(FX / "ca.pem"))
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", DeprecationWarning)  # TLSv1 is deprecated
        weak.minimum_version = ssl.TLSVersion.TLSv1
    McpClient("127.0.0.1", 1, tls=weak)
    assert weak.minimum_version == ssl.TLSVersion.TLSv1_2

    strict = ssl.create_default_context(cafile=str(FX / "ca.pem"))
    strict.minimum_version = ssl.TLSVersion.TLSv1_3
    McpClient("127.0.0.1", 1, tls=strict)
    assert strict.minimum_version == ssl.TLSVersion.TLSv1_3

    newest = ssl.create_default_context(cafile=str(FX / "ca.pem"))
    # MAXIMUM_SUPPORTED is -1, but OpenSSL resolves it on set: it reads
    # back as the newest version, so it is never mistaken for a weak floor
    newest.minimum_version = ssl.TLSVersion.MAXIMUM_SUPPORTED
    McpClient("127.0.0.1", 1, tls=newest)
    assert newest.minimum_version == ssl.TLSVersion.TLSv1_3
