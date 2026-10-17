"""V-02: the default client identity comes from the installed distribution
(or the honest dev marker), never a stale literal restated in the source.

The 0.1.0 default this replaces was never this SDK's version — a fake pin
that no bump ever touches (bump-version.py only knows the MIN constant,
which is why the fake survived every release).
"""

import importlib.metadata as md
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))

from aikoql import McpClient  # noqa: E402
from aikoql.mcp_client import MIN_SERVER_VERSION  # noqa: E402
from scripted import ScriptedServer  # noqa: E402


def test_default_client_identity_is_not_the_stale_pin():
    sent = {}

    def respond(req):
        sent["version"] = req["params"]["clientInfo"]["version"]
        return [{"jsonrpc": "2.0", "id": req["id"],
                 "result": {"serverInfo": {"name": "aikoql-mcp",
                                           "version": MIN_SERVER_VERSION}}}]

    with ScriptedServer(respond=respond) as srv:
        c = McpClient("127.0.0.1", srv.port).connect()
        try:
            c.initialize()
        finally:
            c.close()
    assert sent["version"] != "0.1.0", "the default identity must not be the stale fake pin"
    try:
        assert sent["version"] == md.version("aikoql")
    except md.PackageNotFoundError:
        assert sent["version"] == "dev"
