#!/usr/bin/env python3
"""MCP over HTTP behind OAuth, with an independent client: the official
`mcp` SDK's OAuthClientProvider runs the whole client side against a real
aikoql-mcp server — the 401, protected resource + authorization server
discovery, dynamic client registration, S256 PKCE, the code exchange — and
then an MCP session (initialize, tools/list, remember + get). The only part
scripted here is the person at the browser: the redirect handler posts the
login form (as signing in on the consent page would).

  pip install mcp
  python scripts/e2e-mcp-oauth.py [path/to/aikoql-mcp]
"""
import asyncio
import json
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from urllib.parse import parse_qsl, urlsplit

import httpx2
from mcp import ClientSession
from mcp.client.auth import OAuthClientProvider
from mcp.client.streamable_http import streamable_http_client
from mcp.shared.auth import AuthorizationCodeResult, OAuthClientMetadata

PASSWORD = "e2e-oauth-pw"
REDIRECT = "http://localhost:3118/callback"


class Memory:
    """TokenStorage in memory."""

    def __init__(self):
        self.tokens = self.client = None

    async def get_tokens(self):
        return self.tokens

    async def set_tokens(self, tokens):
        self.tokens = tokens

    async def get_client_info(self):
        return self.client

    async def set_client_info(self, info):
        self.client = info


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


async def run(base):
    got = {}

    async def sign_in(authorization_url):
        # the person at the browser: the consent page's form, posted back
        params = dict(parse_qsl(urlsplit(authorization_url).query))
        params.update(username="admin", password=PASSWORD, approve="1")
        async with httpx2.AsyncClient(follow_redirects=False) as browser:
            page = await browser.get(authorization_url)
            assert page.status_code == 200 and "<form" in page.text, page.text[:200]
            r = await browser.post(f"{base}/oauth/authorize", data=params)
            assert r.status_code == 302, (r.status_code, r.text[:200])
            back = dict(parse_qsl(urlsplit(r.headers["location"]).query))
            got.update(code=back["code"], state=back.get("state"))

    async def callback():
        return AuthorizationCodeResult(code=got["code"], state=got.get("state"))

    storage = Memory()
    provider = OAuthClientProvider(
        server_url=f"{base}/mcp",
        client_metadata=OAuthClientMetadata(
            client_name="e2e-mcp-oauth",
            redirect_uris=[REDIRECT],
            grant_types=["authorization_code", "refresh_token"],
            response_types=["code"],
            token_endpoint_auth_method="none",
        ),
        storage=storage,
        redirect_handler=sign_in,
        callback_handler=callback,
    )
    async with httpx2.AsyncClient(auth=provider) as http:
        async with streamable_http_client(f"{base}/mcp", http_client=http) as streams:
            async with ClientSession(streams[0], streams[1]) as session:
                init = await session.initialize()
                print(f"signed in and initialized: {init.server_info.name}, protocol {init.protocol_version}")
                assert storage.tokens and storage.tokens.refresh_token, "a refresh token was issued"
                names = {t.name for t in (await session.list_tools()).tools}
                assert {"remember", "get"} <= names
                put = await session.call_tool(
                    "remember", {"type_name": "note", "properties": {"via": "oauth"}})
                koid = json.loads(put.content[0].text)["koid"]
                got_ko = await session.call_tool("get", {"koid": koid})
                assert json.loads(got_ko.content[0].text)["properties"]["via"] == "oauth"
                print(f"tools/list {len(names)} tools; remember + get {koid} round-trips")


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else "target/debug/aikoql-mcp"
    port = free_port()
    db = tempfile.mkdtemp(prefix="e2e-mcp-oauth-")
    shutil.rmtree(db)
    proc = subprocess.Popen(
        [binary, "serve", "--metrics-addr", f"127.0.0.1:{port}", db],
        env={"AIKOQL_ADMIN_PASSWORD": PASSWORD, "PATH": "/usr/bin:/bin"},
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        deadline = time.time() + 20
        while True:
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.5).close()
                break
            except OSError:
                if time.time() > deadline or proc.poll() is not None:
                    sys.exit("e2e-mcp-oauth: FAIL the server never listened")
                time.sleep(0.1)
        asyncio.run(run(f"http://127.0.0.1:{port}"))
        print("e2e-mcp-oauth: PASS — the official MCP SDK signs in over OAuth and uses the session")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(db, ignore_errors=True)


if __name__ == "__main__":
    main()
