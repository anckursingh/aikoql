"""Tiny MCP client over AikoQL's TCP transport (newline-delimited JSON-RPC)
plus a server launcher. Used by the C1-C8 memory certification harness."""
import json
import os
import pathlib
import socket
import subprocess
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[2]
BIN = os.environ.get("AIKOQL_BIN") or str(
    REPO / "target" / "debug" / ("aikoql-mcp.exe" if os.name == "nt" else "aikoql-mcp"))
# databases, logs and results go to a work dir, never into the repo
HERE = pathlib.Path(os.environ.get("AIKOQL_CERT_OUT")
                    or pathlib.Path(tempfile.gettempdir()) / "aikoql-agent-memory-cert")
HERE.mkdir(parents=True, exist_ok=True)


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


class Server:
    """One aikoql-mcp serve on a data dir. `tokens`: TOKEN:TENANT:ROLES."""

    def __init__(self, db, tokens, log, extra=()):
        self.port = free_port()
        cfg = HERE / "rate.toml"
        cfg.write_text("[rate_limit]\nenabled = false\n")
        self.proc = subprocess.Popen(
            [BIN, "--config", str(cfg), "serve", str(db),
             "--listen", f"127.0.0.1:{self.port}", *extra]
            + [a for t in tokens for a in ("--tcp-token", t)],
            # memory files and every relative path stay in the work dir
            cwd=HERE, env={**os.environ, "AIKOQL_MEMORY_DIR": str(HERE / "memory")},
            stdout=subprocess.DEVNULL, stderr=open(log, "ab"))
        deadline = time.time() + 120
        while time.time() < deadline:
            try:
                socket.create_connection(("127.0.0.1", self.port), 1).close()
                return
            except OSError:
                if self.proc.poll() is not None:
                    raise RuntimeError(f"server exited: see {log}")
                time.sleep(0.3)
        raise RuntimeError("server never listened")

    def stop(self):
        self.proc.kill()
        self.proc.wait()


class Session:
    """One MCP session: a TCP connection, initialized with a token."""

    def __init__(self, server, token):
        self.s = socket.create_connection(("127.0.0.1", server.port), 30)
        self.s.settimeout(300)
        self.r = self.s.makefile("r", encoding="utf-8")
        self.n = 0
        init = self.rpc("initialize", {"token": token,
                                       "protocolVersion": "2025-06-18",
                                       "capabilities": {}})
        if "result" not in init:
            raise RuntimeError(f"initialize failed: {init}")

    def rpc(self, method, params):
        self.n += 1
        self.s.sendall((json.dumps({"jsonrpc": "2.0", "id": self.n,
                                    "method": method, "params": params}) + "\n").encode())
        while True:
            line = self.r.readline()
            if not line:
                raise RuntimeError("connection closed")
            msg = json.loads(line)
            if msg.get("id") == self.n:
                return msg

    def tool(self, _tool, **args):
        """Call a tool; returns (ok, data). data is the parsed payload."""
        res = self.rpc("tools/call", {"name": _tool, "arguments": args})
        if "error" in res:
            return False, res["error"]
        r = res["result"]
        data = r.get("structuredContent")
        if data is None:
            text = (r.get("content") or [{}])[0].get("text", "")
            try:
                data = json.loads(text)
            except ValueError:
                data = text
        if isinstance(data, dict) and data.get("ok") is False:
            return False, data.get("error", data)
        return not r.get("isError", False), data

    def close(self):
        self.s.close()
