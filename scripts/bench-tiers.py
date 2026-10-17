#!/usr/bin/env python
"""D-20 §30 — tier×transport certification: protocol/serialization/SDK
tiers, honest labels, one §13-schema result.json (artifact_schema.py's
validate_tiers arm — an unlabeled number is uncommittable).

Tiers measured HERE (the launch plan's D-20 RED state: no SDK benchmark):
  serialization  pure JSON encode/decode cost of the REAL payload shapes
                 the wire exchanges (transportless: the wire-format floor)
  protocol       raw wire round-trips, NO SDK in the path: the MCP
                 JSON-RPC newline frames + the D-15 §6 native frames
                 (AKQL header + CRC32-IEEE), hand-rolled against the real
                 aikoql-mcp binary
  SDK            the python SDK clients: embedded (PyO3 Agent) + MCP
                 (McpClient with a byte-counting socket) — every SDK cell
                 carries the §30 claim: SDK latency, never engine latency
Tiers covered ELSEWHERE (cited, not re-measured — the engine tier is
already the §13 competitor matrix, and competitor runs never block PRs):
  engine         benchmarks/ criterion benches + docs/certification/
                 competitors/result.json
  application    scale.py's mcp_mode column (docs/certification/
                 competitors/scale/result.json)
  REST           N/A — no REST transport exists in the codebase

Metrics per cell (structural counters first, §30): p50/p95/p99 ns,
throughput, allocs (sys.getallocatedblocks delta, client process), wire
bytes req+resp, JSON documents decoded, CPU delta + RSS of the named
process (proc_scope: server for the protocol legs — what the wire costs
the server — harness for the SDK/serialization legs). The oracle asserts
every op's result: a wrong answer fails the run (scale.py's pattern).

Usage:
  python bench-tiers.py --quick    smoke: n=10, temp output
  python bench-tiers.py            full: n=100, docs/certification/tiers/
"""

import argparse
import json
import os
import platform
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import zlib
from pathlib import Path

import psutil

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "crates" / "sdk" / "python" / "python"))

MCP_ADDR = ("127.0.0.1", 9090)
NATIVE_ADDR = ("127.0.0.1", 9091)
TOKEN = "bench-token"

# §6 (D-15): "AKQL" + version u16 BE + flags u16 BE + request_id u64 BE +
# msg_type u16 BE + payload_len u32 BE, then the JSON payload, then the
# CRC32-IEEE over header+payload as a little-endian u32 (zlib.crc32 IS the
# IEEE polynomial — no hand-rolled table needed).
NAT_MAGIC = b"AKQL"
NAT_FLAG_RESPONSE = 1
NAT_HELLO, NAT_AUTH, NAT_EXECUTE, NAT_QUERY, NAT_CLOSE = 1, 2, 8, 9, 13


def _pct(xs, p):
    xs = sorted(xs)
    k = (len(xs) - 1) * p / 100.0
    lo = int(k)
    hi = min(lo + 1, len(xs) - 1)
    return int(xs[lo] + (xs[hi] - xs[lo]) * (k - lo))


def rows_of(out):
    """MCP aikoql returns {"results": [...]}; embedded returns a list."""
    return out.get("results") if isinstance(out, dict) else out


def env_block():
    sha = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip()
    return {
        "os": platform.platform(),
        "cpu": platform.processor() or platform.machine(),
        "ram_mb": psutil.virtual_memory().total // (1024 * 1024),
        "cache_state": ("warm — each leg pre-populates its dataset before "
                        "the measured loops"),
        "git_sha": sha,      # check_fresh compares THIS one to tested HEAD
        "harness_sha": sha,  # the §13 harness field
    }


# ------------------------------------------------------------- wire clients

class McpRaw:
    """The MCP JSON-RPC newline protocol, hand-rolled (mirrors the python
    SDK's _send/_recv byte-for-byte: json.dumps default separators + \\n).
    No SDK import — this is the protocol tier, not the SDK tier."""

    def __init__(self, sock):
        self.sock = sock
        self.buf = b""
        self.nid = 0
        self.sent = 0
        self.recvd = 0

    def call(self, method, params=None):
        self.nid += 1
        req = {"jsonrpc": "2.0", "id": self.nid, "method": method}
        if params is not None:
            req["params"] = params
        frame = json.dumps(req, default=str).encode("utf-8") + b"\n"
        self.sock.sendall(frame)
        self.sent += len(frame)
        return self.recv()

    def recv(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise RuntimeError("mcp-raw: the server closed the connection")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        self.recvd += len(line) + 1
        return json.loads(line)


def _read_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise RuntimeError("native: the server closed mid-frame")
        buf += chunk
    return buf


class Native:
    """The §6 framed protocol, hand-rolled (serde_json::to_vec = compact
    separators — the rust SDK's exact wire shape)."""

    def __init__(self, sock):
        self.sock = sock
        self.nid = 0
        self.sent = 0
        self.recvd = 0

    def send(self, mtype, payload):
        self.nid += 1
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        header = (NAT_MAGIC + (1).to_bytes(2, "big") + (0).to_bytes(2, "big")
                  + self.nid.to_bytes(8, "big") + mtype.to_bytes(2, "big")
                  + len(body).to_bytes(4, "big"))
        crc = zlib.crc32(header + body).to_bytes(4, "little")
        frame = header + body + crc
        self.sock.sendall(frame)
        self.sent += len(frame)
        return self.nid

    def recv(self):
        header = _read_exact(self.sock, 22)
        assert header[:4] == NAT_MAGIC, "native: bad magic"
        plen = int.from_bytes(header[18:22], "big")
        payload = _read_exact(self.sock, plen)
        crc = _read_exact(self.sock, 4)
        assert zlib.crc32(header + payload) == int.from_bytes(crc, "little"), \
            "native: checksum mismatch"
        self.recvd += 22 + plen + 4
        return (int.from_bytes(header[6:8], "big") & NAT_FLAG_RESPONSE,
                int.from_bytes(header[16:18], "big"), json.loads(payload))


class WireSock:
    """Byte-counting socket wrapper on an McpClient — the SDK-MCP leg's
    wire metric (the client dials its own socket; we count what it moves)."""

    def __init__(self, sock):
        self._s = sock
        self.sent = 0
        self.recvd = 0

    def sendall(self, b):
        self.sent += len(b)
        return self._s.sendall(b)

    def recv(self, n):
        b = self._s.recv(n)
        self.recvd += len(b)
        return b

    def settimeout(self, t):
        return self._s.settimeout(t)

    def close(self):
        return self._s.close()


def mcp_data(resp):
    """Unwraps a tools/call response the way the SDK does: the content
    envelope's text is {ok, data...}."""
    text = resp["result"]["content"][0]["text"]
    data = json.loads(text)
    if not data.get("ok", True):
        raise RuntimeError(f"server error: {data.get('error')}")
    return data.get("data", data)


def native_data(payload):
    if not payload.get("ok", True):
        raise RuntimeError(f"native error: {payload.get('error')}")
    return payload.get("data", payload)


# ------------------------------------------------------------------ server

def start_server(db_dir):
    exe = REPO / "target" / "release" / \
        ("aikoql-mcp.exe" if os.name == "nt" else "aikoql-mcp")
    # scale.py's pattern: the config sits BESIDE the db dir (S-02 refuses a
    # pre-existing db path; the config must not live inside it), and the
    # default 120 calls/min limit is far below bench rate.
    cfg = db_dir.parent / "aikoql.toml"
    cfg.write_text("[rate_limit]\nmax_calls_per_minute = 10000000\n")
    proc = subprocess.Popen(
        [str(exe), "serve", "--listen", f"{MCP_ADDR[0]}:{MCP_ADDR[1]}",
         "--native-port", f"{NATIVE_ADDR[0]}:{NATIVE_ADDR[1]}",
         "--tcp-token", f"{TOKEN}::bench", "--config", str(cfg), str(db_dir)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(120):
        if proc.poll() is not None:
            raise RuntimeError("aikoql-mcp exited during startup")
        try:
            s = socket.create_connection(MCP_ADDR, timeout=2.0)
            c = McpRaw(s)
            info = c.call("initialize", {
                "protocolVersion": "2024-11-05", "capabilities": {},
                "clientInfo": {"name": "bench-tiers", "version": "0"},
                "token": TOKEN})["result"].get("serverInfo", {})
            return proc, c, info
        except Exception:
            time.sleep(0.5)
    proc.terminate()
    raise RuntimeError("aikoql-mcp did not come up on 9090")


# -------------------------------------------------------------------- cells

def cell_metrics(lats, n, correct, req_bytes, resp_bytes, decodes, allocs,
                 cpu_sec, rss_mb, proc_scope):
    return {
        "n": n,
        "p50_ns": _pct(lats, 50),
        "p95_ns": _pct(lats, 95),
        "p99_ns": _pct(lats, 99),
        "throughput_ops_s": round(n / (sum(lats) / 1e9), 1),
        "allocs_per_op": allocs,
        "wire_bytes_req": req_bytes,
        "wire_bytes_resp": resp_bytes,
        "decodes_per_op": decodes,
        "cpu_seconds": round(cpu_sec, 4),
        "rss_mb": rss_mb,
        "proc_scope": proc_scope,
        "correct": correct,
    }


def run_cell(op_fn, n, proc):
    """Runs op_fn n times; op_fn(i) -> (ok, req_bytes, resp_bytes,
    decodes) with i the iteration index."""
    lats = []
    req = resp = dec = 0
    correct = True
    blocks0 = sys.getallocatedblocks()
    cpu0 = proc.cpu_times()
    for i in range(n):
        t0 = time.perf_counter_ns()
        res = op_fn(i)
        lats.append(time.perf_counter_ns() - t0)
        correct = correct and res[0]
        req += res[1]
        resp += res[2]
        dec += res[3]
    cpu1 = proc.cpu_times()
    cpu_sec = (cpu1.user + cpu1.system) - (cpu0.user + cpu0.system)
    return cell_metrics(
        lats, n, correct, req // n, resp // n, dec // n,
        max(0, (sys.getallocatedblocks() - blocks0) // n), cpu_sec,
        round(proc.memory_info().rss / (1024 * 1024), 1),
        "server" if proc.pid != os.getpid() else "harness")


def pop_note(c, i):
    return {"type_name": c["type"],
            "properties": {"title": f"note-{i}", "body": "b" * 120, "seq": i}}


# ------------------------------------------------------ protocol tier: MCP

def mcp_raw_leg(raw, server, n):
    server_p = psutil.Process(server.pid)
    cells = {}
    claims = {
        "remember": "client-observed MCP JSON-RPC round-trip, hand-rolled "
                    "frames — no SDK in the path",
        "get": "client-observed MCP JSON-RPC round-trip, hand-rolled "
               "frames — no SDK in the path",
        "scan": "client-observed MCP JSON-RPC round-trip, hand-rolled "
                "frames — no SDK in the path",
    }
    ctx = {"type": f"bench_note_mcpraw_{n}"}
    # The remember cell writes a distinct type so the scan oracle counts
    # exactly the prep's n rows, not prep + writes.
    ctx_w = {"type": f"{ctx['type']}_w"}

    def prep():
        koids = []
        for i in range(n):
            out = mcp_data(raw.call("tools/call", {
                "name": "remember", "arguments": pop_note(ctx, i)}))
            koids.append(out["koid"])
        return koids

    koids = prep()

    def remember(i):
        out = mcp_data(raw.call("tools/call", {
            "name": "remember", "arguments": pop_note(ctx_w, i)}))
        return (bool(out.get("koid")), raw.sent, raw.recvd, 1)

    def get(i):
        out = mcp_data(raw.call("tools/call", {
            "name": "get", "arguments": {"koid": koids[i]}}))
        return (out["properties"]["title"] == f"note-{i}", raw.sent,
                raw.recvd, 1)

    def scan(i):
        out = mcp_data(raw.call("tools/call", {
            "name": "aikoql",
            "arguments": {"query": f"MATCH {ctx['type']} RETURN *"}}))
        return (len(rows_of(out)) == n, raw.sent, raw.recvd, 1)

    # The wire bytes are per-socket cumulative — snap before/after each
    # cell so every cell reports its own share.
    for name, fn in (("remember", remember), ("get", get), ("scan", scan)):
        raw.sent = raw.recvd = 0
        cells[name] = run_cell(lambda i: fn(i), n, server_p)
        cells[name]["claim"] = claims[name]
    return cells


# ---------------------------------------------------- protocol tier: native

def native_leg(server, n):
    server_p = psutil.Process(server.pid)
    s = socket.create_connection(NATIVE_ADDR, timeout=2.0)
    nat = Native(s)
    # HELLO (the version gate) then AUTH (the token frame) — the D-15
    # handshake the rust SDK performs.
    nat.send(NAT_HELLO, {"protocol_version": 1, "capabilities": [],
                         "client": {"name": "bench-tiers", "version": "0"}})
    _, mt, _ = nat.recv()
    assert mt == NAT_HELLO, "native: HELLO not answered with HELLO"
    nat.send(NAT_AUTH, {"token": TOKEN})
    _, mt, auth = nat.recv()
    assert mt == NAT_AUTH and auth.get("ok") is True, "native: AUTH failed"

    cells = {}
    claim = ("client-observed §6 native round-trip (AKQL frames, hand-"
             "rolled) — no SDK in the path")
    ctx = {"type": f"bench_note_native_{n}"}
    ctx_w = {"type": f"{ctx['type']}_w"}

    def exec_tool(name, args):
        nat.send(NAT_EXECUTE, {"tool": name, "args": args})
        flags, mt, payload = nat.recv()
        assert flags == NAT_FLAG_RESPONSE and mt == NAT_EXECUTE, \
            "native: EXECUTE not answered with a response frame"
        return native_data(payload)

    koids = []
    for i in range(n):
        koids.append(exec_tool("remember", pop_note(ctx, i))["koid"])

    def remember(i):
        out = exec_tool("remember", pop_note(ctx_w, i))
        return (bool(out.get("koid")), nat.sent, nat.recvd, 1)

    def get(i):
        out = exec_tool("get", {"koid": koids[i]})
        return (out["properties"]["title"] == f"note-{i}", nat.sent,
                nat.recvd, 1)

    def scan(i):
        nat.send(NAT_QUERY, {"query": f"MATCH {ctx['type']} RETURN *",
                             "stream": False})
        flags, mt, payload = nat.recv()
        assert flags == NAT_FLAG_RESPONSE and mt == NAT_QUERY, \
            "native: QUERY not answered with a QUERY frame"
        return (len(payload.get("results", [])) == n, nat.sent, nat.recvd, 1)

    for name, fn in (("remember", remember), ("get", get), ("scan", scan)):
        nat.sent = nat.recvd = 0
        cells[name] = run_cell(lambda i: fn(i), n, server_p)
        cells[name]["claim"] = claim
    nat.send(NAT_CLOSE, {"ok": True})
    s.close()
    return cells


# ---------------------------------------------------- serialization tier

def serialization_leg(n):
    """Encodes/decodes the wire's own payload shapes — built exactly the
    way the protocol legs build them (a remember request payload, a get
    response, a scan response with n rows). Transportless: the wire-format
    floor, measured in-process."""
    self_p = psutil.Process()
    claim = ("JSON encode/decode cost of the wire's own payload shapes — "
             "the format floor, no transport, no server")
    req_obj = {"type_name": "bench_note_x",
               "properties": {"title": "note-0", "body": "b" * 120,
                              "seq": 0}}
    resp_text = json.dumps({"ok": True, "data": {
        "koid": "x" * 32, "properties": req_obj["properties"]}})
    scan_text = json.dumps({"ok": True, "data": {
        "results": [{"koid": "x" * 32, "title": "note-0"}
                    for _ in range(n)]}})

    cells = {}

    def encode(i):
        b = json.dumps(req_obj).encode("utf-8")
        return (True, len(b), 0, 0)

    def decode(i):
        json.loads(scan_text)
        json.loads(resp_text)
        return (True, 0, 0, 2)

    cells["encode"] = run_cell(encode, n, self_p)
    cells["encode"]["claim"] = claim
    cells["decode"] = run_cell(decode, n, self_p)
    cells["decode"]["claim"] = claim
    return cells


# ------------------------------------------------------------ SDK tier

def sdk_mcp_leg(server, n):
    self_p = psutil.Process()
    from aikoql.mcp_client import McpClient
    c = McpClient(*MCP_ADDR, token=TOKEN)
    c.connect()
    c.initialize(client_name="bench-tiers", client_version="0")
    ws = WireSock(c._sock)  # the SDK's socket, byte-counted (benchmark-only)
    c._sock = ws
    cells = {}
    claim = ("SDK latency — the python McpClient, measured at the client "
             "process (the engine is not what this number measures)")
    ctx = {"type": f"bench_note_sdkmcp_{n}"}
    ctx_w = {"type": f"{ctx['type']}_w"}

    koids = [c.remember(pop_note(ctx, i)["type_name"],
                        pop_note(ctx, i)["properties"])["koid"]
             for i in range(n)]

    def remember(i):
        out = c.remember(pop_note(ctx_w, i)["type_name"],
                         pop_note(ctx_w, i)["properties"])
        return (bool(out.get("koid")), ws.sent, ws.recvd, 1)

    def get(i):
        out = c.get(koids[i])
        return (out["properties"]["title"] == f"note-{i}", ws.sent,
                ws.recvd, 1)

    def scan(i):
        out = c.aikoql(f"MATCH {ctx['type']} RETURN *")
        return (len(rows_of(out)) == n, ws.sent, ws.recvd, 1)

    for name, fn in (("remember", remember), ("get", get), ("scan", scan)):
        ws.sent = ws.recvd = 0
        cells[name] = run_cell(lambda i: fn(i), n, self_p)
        cells[name]["claim"] = claim
    c.close()
    return cells


def sdk_embedded_leg(n):
    self_p = psutil.Process()
    from aikoql import Agent
    parent = Path(tempfile.mkdtemp(prefix="aikoql-tiers-"))
    kb = parent / "kb"
    try:
        agent = Agent.connect(str(kb))
        cells = {}
        claim = ("SDK latency — the python embedded Agent, measured in-"
                 "process (the engine is not what this number measures)")
        ctx = {"type": f"bench_note_emb_{n}"}
        ctx_w = {"type": f"{ctx['type']}_w"}

        koids = [agent.remember(pop_note(ctx, i)["type_name"],
                                pop_note(ctx, i)["properties"])["koid"]
                 for i in range(n)]

        def remember(i):
            out = agent.remember(pop_note(ctx_w, i)["type_name"],
                                 pop_note(ctx_w, i)["properties"])
            return (bool(out.get("koid")), 0, 0, 1)

        def get(i):
            out = agent.get(koids[i])
            return (out["properties"]["title"] == f"note-{i}", 0, 0, 1)

        def scan(i):
            out = agent.aikoql(f"MATCH {ctx['type']} RETURN *")
            return (len(rows_of(out)) == n, 0, 0, 1)

        for name, fn in (("remember", remember), ("get", get),
                         ("scan", scan)):
            cells[name] = run_cell(lambda i: fn(i), n, self_p)
            cells[name]["claim"] = claim
            cells[name]["wire_bytes_note"] = \
                "embedded — no wire (0 by definition)"
        return cells
    finally:
        agent.close()
        shutil.rmtree(parent, ignore_errors=True)


# -------------------------------------------------------------------- main

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true",
                    help="smoke run: n=10, temp output")
    ap.add_argument("--out", default=None, help="result path override")
    args = ap.parse_args()

    n = 10 if args.quick else 100
    out = Path(args.out) if args.out else \
        REPO / "docs" / "certification" / "tiers" / "result.json"

    parent = Path(tempfile.mkdtemp(prefix="aikoql-tiers-"))
    db_dir = parent / "db"
    try:
        print("starting aikoql-mcp (MCP + native listeners) ...", flush=True)
        server, raw, server_info = start_server(db_dir)

        print(f"protocol/MCP x{3 * n} ...", flush=True)
        mcp_cells = mcp_raw_leg(raw, server, n)

        print(f"serialization x{2 * n} ...", flush=True)
        ser_cells = serialization_leg(n)

        print(f"protocol/native x{3 * n} ...", flush=True)
        native_cells = native_leg(server, n)

        print(f"SDK/MCP x{3 * n} ...", flush=True)
        sdk_mcp_cells = sdk_mcp_leg(server, n)

        print(f"SDK/embedded x{3 * n} ...", flush=True)
        sdk_emb_cells = sdk_embedded_leg(n)
    finally:
        if "server" in dir():
            server.terminate()
            server.wait()
        if "raw" in dir():
            raw.sock.close()
        shutil.rmtree(parent, ignore_errors=True)
        # mcp audit.rs derives {db_path}.audit.log BESIDE the db dir.
        Path(f"{db_dir}.audit.log").unlink(missing_ok=True)

    cells = []
    for tier, transport, cells_map in (
            ("protocol", "mcp", mcp_cells),
            ("protocol", "native", native_cells),
            ("serialization", "transportless", ser_cells),
            ("sdk", "mcp", sdk_mcp_cells),
            ("sdk", "embedded", sdk_emb_cells)):
        for op, row in cells_map.items():
            row.update({"tier": tier, "transport": transport, "op": op})
            cells.append(row)

    # The harness's own oracles are the correctness pins (scale.py's
    # pattern): any FAIL fails the run.
    bad = [f"{c['tier']}/{c['transport']}/{c['op']}" for c in cells
           if not c["correct"]]
    if bad:
        print("ORACLE FAILURES: " + ", ".join(bad))
        sys.exit(2)

    result = {
        "commit": env_block()["harness_sha"],
        "started_at": int(time.time() * 1000),
        "seed": 42,
        "engine_version": server_info.get("version", "unknown"),
        "tiers": {
            "engine": {"covered_by": "benchmarks/ criterion benches + "
                                     "docs/certification/competitors/"
                                     "result.json (the §13 matrix)"},
            "application": {"covered_by": "docs/certification/competitors/"
                                          "scale/result.json (mcp_mode)"},
            "rest": {"status": "N/A — no REST transport exists"},
        },
        "environment": env_block(),
        "cells": cells,
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(result, indent=2) + "\n")
    print(f"tiers: {len(cells)} cells -> {out}")


if __name__ == "__main__":
    main()
