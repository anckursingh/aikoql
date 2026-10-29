#!/usr/bin/env python3
"""MCP over Streamable HTTP with a real client: the official `mcp` SDK's
streamable_http_client against a real aikoql-mcp server — initialize,
tools/list, remember + get, and session termination on close (DELETE).
Our own tests pin the wire; this proves an independent client agrees.

  pip install mcp
  python scripts/e2e-mcp-http.py [path/to/aikoql-mcp]
"""
import asyncio
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

import httpx2
from mcp import ClientSession
from mcp.client.streamable_http import streamable_http_client

TOKEN = "e2e-http"


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


async def run(url):
    async with httpx2.AsyncClient(headers={"Authorization": f"Bearer {TOKEN}"}) as http:
        async with streamable_http_client(url, http_client=http) as streams:
            read, write = streams[0], streams[1]
            async with ClientSession(read, write) as session:
                init = await session.initialize()
                print(f"initialized: server {init.server_info.name} {init.server_info.version}, "
                      f"protocol {init.protocol_version}")
                tools = await session.list_tools()
                names = {t.name for t in tools.tools}
                assert {"remember", "get"} <= names, f"tools: {sorted(names)[:10]}"
                print(f"tools/list: {len(names)} tools")
                put = await session.call_tool(
                    "remember", {"subject": "admin", "type_name": "note", "properties": {"via": "mcp-sdk"}})
                koid = json.loads(put.content[0].text)["koid"]
                got = await session.call_tool("get", {"subject": "admin", "koid": koid})
                via = json.loads(got.content[0].text)["properties"]["via"]
                assert via == "mcp-sdk", got
                print(f"remember + get: {koid} round-trips")


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else "target/debug/aikoql-mcp"
    port = free_port()
    db = tempfile.mkdtemp(prefix="e2e-mcp-http-")
    shutil.rmtree(db)
    proc = subprocess.Popen(
        [binary, "serve", "--metrics-addr", f"127.0.0.1:{port}", "--tcp-token", f"{TOKEN}::admin", db],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        deadline = time.time() + 20
        while True:
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.5).close()
                break
            except OSError:
                if time.time() > deadline or proc.poll() is not None:
                    sys.exit("e2e-mcp-http: FAIL the server never listened")
                time.sleep(0.1)
        asyncio.run(run(f"http://127.0.0.1:{port}/mcp"))
        print("e2e-mcp-http: PASS — the official MCP SDK speaks to aikoql over Streamable HTTP")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(db, ignore_errors=True)


if __name__ == "__main__":
    main()
