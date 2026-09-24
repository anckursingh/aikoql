"""P5-M17 — benchmark scale-out + honest cells (measurement-first milestone:
the harness's own oracles are the correctness pins — a wrong oracle result
fails the run).

Three sections, one result.json:
  scale        aikoql embedded at 100k/1M — the three degrading cells
               (structured_filter, vector_recall, graph) + point_read
               (point_write/transactions measured too: same 6-cell shape
               as the main report)
  multi_op_txn aikoql-mcp txn_begin/stage×N/commit vs PG
               BEGIN/INSERT×N/COMMIT, N in {10, 100} — the P5-M10 surface
  mcp_mode     the full 6-cell column through localhost aikoql-mcp on the
               same seed-42 dataset as the main report (embedded vs wire)

Usage:
  python scale.py --quick    smoke run: 1000 notes, n=10 cells, temp output
  python scale.py            full run: 100k + 1M + txn cells + MCP column
"""

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import psutil

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))

import bench as B  # noqa: E402  (shared dataset + cell machinery)
from bench import Agent, QUERY_VEC, gen_dataset, run_engine  # noqa: E402

SCALE_DIR = REPO / "docs" / "certification" / "competitors" / "scale"
MCP_ADDR = ("127.0.0.1", 9090)
TOKEN = "bench-token"
TXN_BATCHES = (10, 100)


def rows_of(out):
    """MCP aikoql returns {"results": [...]}; embedded returns a list."""
    return out.get("results") if isinstance(out, dict) else out


def hits_of(out):
    """MCP traverse returns {"hits": [...]}; embedded returns a list."""
    return out.get("hits") if isinstance(out, dict) else out


# ---------------------------------------------------------------- (a) scale

def scale_run(n_notes, n_ops):
    ds = gen_dataset(n_notes=n_notes, n_events=n_notes // 2)
    kb = Path(tempfile.mkdtemp(prefix=f"aikoql-scale-{n_notes}-"))
    print(f"aikoql embedded {n_notes} notes: ingest + cells ...", flush=True)
    res = B.bench_aikoql(ds, kb, n=n_ops)
    res["n_notes"] = n_notes
    res["n_events"] = n_notes // 2
    shutil.rmtree(kb, ignore_errors=True)
    print(f"  done: ingest {res['ingest_s']}s, rss {res['rss_kb']} KiB", flush=True)
    return res


# ---------------------------------------------------------------- (b) txn cells

def start_server(db_dir):
    exe = REPO / "target" / "release" / \
        ("aikoql-mcp.exe" if os.name == "nt" else "aikoql-mcp")
    # Bench config: the default 120 calls/min limit is far below ingest rate.
    cfg = db_dir / "aikoql.toml"
    cfg.write_text("[rate_limit]\nmax_calls_per_minute = 10000000\n")
    env = dict(os.environ, AIKOQL_BACKEND="aikoql-v2")
    proc = subprocess.Popen(
        [str(exe), "serve", "--listen", f"{MCP_ADDR[0]}:{MCP_ADDR[1]}",
         "--tcp-token", f"{TOKEN}::bench", "--config", str(cfg), str(db_dir)],
        env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(120):
        if proc.poll() is not None:
            raise RuntimeError("aikoql-mcp exited during startup")
        try:
            a = Agent.connect(MCP_ADDR, token=TOKEN)
            a.close()
            return proc
        except Exception:
            time.sleep(0.5)
    proc.terminate()
    raise RuntimeError("aikoql-mcp did not come up on 9090")


def txn_cell_pair(connect, close, build, n_ops):
    cold, warm = B.cell(connect, close, build, n_ops)
    return {
        "n": n_ops, "cold": cold, "warm": warm,
        "p50_ms": warm["p50_ms"], "p95_ms": warm["p95_ms"],
        "p99_ms": warm["p99_ms"],
        "throughput_ops_s": warm["throughput_ops_s"],
        "correct": cold["correct"] and warm["correct"],
    }


def multi_op_txn(n_ops):
    from aikoql.mcp_client import McpClient

    out = {"batch_sizes": list(TXN_BATCHES), "n_per_cell": n_ops}

    # aikoql: one server txn = begin + N staged creates + one commit.
    client = McpClient(MCP_ADDR[0], MCP_ADDR[1], TOKEN)
    client.connect()
    client.initialize()
    out["aikoql_mcp"] = {}
    for n_batch in TXN_BATCHES:
        state = {"counter": 0, "committed": []}

        def build(c, nb=n_batch):
            def op():
                state["counter"] += 1
                # txn ids are the kernel's idempotency keys — they must stay
                # unique ACROSS cells, or the kernel correctly dedupes a
                # re-used id as a recorded no-op (the oracle would see the
                # old batch's results — the exact bug the smoke caught).
                txn_id = f"bench-{n_batch}-{state['counter']}"
                c.call_tool("txn_begin", {"txn_id": txn_id})
                for i in range(nb):
                    c.call_tool("txn_stage", {"txn_id": txn_id, "op": {
                        "action": "create", "type_name": "note",
                        "properties": {"topic": "pet", "body": "txn",
                                       "seq": 9000 + state["counter"] * 1000 + i}}})
                res = c.call_tool("txn_commit", {"txn_id": txn_id})
                results = res.get("results") or []
                state["committed"].append(results[0]["koid"] if results else None)
                return (len(results) == nb and not res.get("deduped", True))
            return op

        print(f"aikoql-mcp txn cell: batch {n_batch} ...", flush=True)
        out["aikoql_mcp"][f"txn_{n_batch}"] = txn_cell_pair(
            lambda: client, lambda c: None, build, n_ops)
    # Visibility oracle: the last committed row must read back.
    last = state["committed"][-1]
    assert last and client.get(last).get("koid") == last, "txn commit not visible"
    client.close()

    # PostgreSQL: BEGIN + N INSERTs + COMMIT on the same tables as the report.
    print("postgresql txn cells ...", flush=True)
    B.bench_pg(gen_dataset())  # (re)creates the seeded tables; cells discarded
    import psycopg
    conn = psycopg.connect(B.PG_DSN)
    conn.autocommit = True
    out["postgresql"] = {}
    pg_counter = [0]
    for n_batch in TXN_BATCHES:
        def build_pg(c, nb=n_batch):
            def op():
                pg_counter[0] += 1
                base = pg_counter[0] * 1000
                c.execute("BEGIN")
                for i in range(nb):
                    c.execute(
                        "INSERT INTO notes (koid, topic, body) VALUES (%s,%s,%s)",
                        (f"pg-txn-{base+i}", "pet", "txn"))
                c.execute("COMMIT")
                return True
            return op

        out["postgresql"][f"txn_{n_batch}"] = txn_cell_pair(
            lambda: conn, lambda c: None, build_pg, n_ops)
    # Visibility oracle: the last inserted row must read back.
    row = conn.execute("SELECT koid FROM notes WHERE koid = %s",
                       (f"pg-txn-{pg_counter[0] * 1000}",)).fetchone()
    assert row is not None, "pg txn commit not visible"
    conn.close()
    return out


# ---------------------------------------------------------------- (c) MCP column

def mcp_column(ds, server, db_dir, n_ops):
    agent = Agent.connect(MCP_ADDR, token=TOKEN)
    note_koids, event_koids = [], []
    t0 = time.perf_counter()
    for n in ds["notes"]:
        r = agent.remember("note", {"topic": n["topic"], "body": n["body"],
                                    "seq": n["i"]},
                           semantic={"embedding": n["vec"],
                                     "embedding_model": "bench-2d"})
        note_koids.append(r["koid"])
    for e in ds["events"]:
        r = agent.remember("event", {"label": e["label"], "seq": e["i"]})
        event_koids.append(r["koid"])
    for a, b in ds["mentions"]:
        agent.relate(note_koids[a], event_koids[b], "mentions")
    for a, b in ds["derived"]:
        agent.relate(note_koids[a], note_koids[b], "derived_from")
    # P5-M17b: PG's CREATE INDEX parity — declared (and analyzed) exactly
    # where PG builds notes_topic_idx, inside the timed ingest window.
    agent.create_index("by_topic", "note", ["topic"])
    ingest_s = time.perf_counter() - t0
    agent.close()

    cats_set = {note_koids[n["i"]] for n in ds["notes"] if n["body"] == "cats"}
    read_pairs = [(note_koids[i], ds["notes"][i]) for i in ds["read_ids"]]
    write_triples = [(note_koids[i], ds["notes"][i]) for i in ds["write_ids"]]
    graph_expect = {
        note_koids[i]: {event_koids[b] for a, b in ds["mentions"] if a == i}
        for i in ds["graph_ids"]
    }

    def connect():
        return Agent.connect(MCP_ADDR, token=TOKEN)

    def build_read(agent):
        rng = random.Random(7)

        def op():
            koid, note = rng.choice(read_pairs)
            ko = agent.get(koid)
            return (ko["koid"] == koid
                    and ko["properties"]["topic"] == note["topic"])

        return op

    def build_write(agent):
        rng = random.Random(7)

        def op():
            koid, note = rng.choice(write_triples)
            r = agent.remember("note", {"topic": note["topic"],
                                        "body": "cats.v2", "seq": note["i"]},
                               koid=koid)
            return r["koid"] == koid and r.get("version", 0) >= 2

        return op

    def build_filter(agent):
        # P5-M17b: re-declared per cell connect. The M9 stats contract makes
        # ANY later write (the write cell runs first) stale the stats, so the
        # declaration's rebuild + analyze must run here for the optimizer to
        # price the index. Refresh cost stays OUTSIDE the measured op — PG
        # pays it once at ingest, we pay it per connect. See REPORT.md.
        agent.create_index("by_topic", "note", ["topic"])

        def op():
            out = agent.aikoql('MATCH note WHERE topic == "pet" RETURN *')
            return len(rows_of(out)) == len(ds["notes"]) // 2

        return op

    def build_txn(agent):
        rng = random.Random(7)

        def op():
            r = agent.remember("note", {"topic": "pet", "body": "txn",
                                        "seq": 9000 + rng.randrange(1000)})
            return "koid" in r

        return op

    def build_graph(agent):
        rng = random.Random(7)

        def op():
            koid, expect = rng.choice(list(graph_expect.items()))
            hits = hits_of(agent.traverse(koid, "mentions", 1))
            got = {h["koid"] for h in hits}
            return got == expect and len(got) > 0

        return op

    def build_vector(agent):
        def op():
            hits = agent.find_similar(vector=QUERY_VEC, k=10,
                                      fusion="vector_only")
            return (len(hits) == 10
                    and all(h["koid"] in cats_set for h in hits))

        return op

    print("aikoql-mcp column: ingest + 6 cells ...", flush=True)
    workloads = run_engine(
        connect, lambda c: c.close(),
        [build_read, build_write, build_filter, build_txn,
         build_graph, build_vector], n_ops)

    rss_kb = psutil.Process(server.pid).memory_info().rss // 1024
    disk = sum(f.stat().st_size for f in db_dir.rglob("*") if f.is_file())
    return {"workloads": workloads, "rss_kb": rss_kb, "disk_bytes": disk,
            "ingest_s": round(ingest_s, 2), "server": "aikoql-mcp",
            "transport": "localhost TCP", "token": TOKEN}


# ---------------------------------------------------------------- main

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true",
                    help="smoke run: 1000 notes, n=10 cells")
    ap.add_argument("--out", default=None, help="result path override")
    args = ap.parse_args()

    n_ops = 10 if args.quick else 50
    out = Path(args.out) if args.out else SCALE_DIR / "result.json"
    result = {
        "commit": B.git_commit(),
        "started_at": int(time.time() * 1000),
        "seed": 42,
        "note": "published report, not a CI gate",
    }

    # (a) scale — embedded aikoql.
    sizes = [(1000, "1k")] if args.quick \
        else [(100_000, "100k"), (1_000_000, "1m")]
    result["scale"] = {tag: scale_run(n_notes, n_ops)
                       for n_notes, tag in sizes}

    # (b) + (c) — one aikoql-mcp server instance.
    mcp_dir = Path(tempfile.mkdtemp(prefix="aikoql-mcp-bench-"))
    server = start_server(mcp_dir)
    try:
        result["mcp_mode"] = mcp_column(gen_dataset(), server, mcp_dir, n_ops)
        result["multi_op_txn"] = multi_op_txn(n_ops)
    finally:
        server.terminate()
        server.wait()
        shutil.rmtree(mcp_dir, ignore_errors=True)
        # mcp audit.rs derives `{db_path}.audit.log` BESIDE the db dir — the
        # rmtree above never reaches it.
        Path(f"{mcp_dir}.audit.log").unlink(missing_ok=True)

    # The harness's own oracles are the correctness pins: any FAIL fails the run.
    bad = []
    for tag, e in result["scale"].items():
        bad += [f"scale/{tag}/{w['name']}" for w in e["workloads"]
                if w.get("n") and not w["correct"]]
    for eng in ("aikoql_mcp", "postgresql"):
        bad += [f"txn/{eng}/{k}" for k, v in result["multi_op_txn"][eng].items()
                if not v["correct"]]
    bad += [f"mcp_mode/{w['name']}" for w in result["mcp_mode"]["workloads"]
            if w.get("n") and not w["correct"]]
    if bad:
        print("ORACLE FAILURES: " + ", ".join(bad))
        sys.exit(2)

    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(result, indent=2) + "\n")
    print(f"wrote {out}")
    for tag, e in result["scale"].items():
        wl = ", ".join(f"{w['name']}:{w['correct']}"
                       for w in e["workloads"] if w.get("n"))
        print(f"  scale {tag}: ingest {e['ingest_s']}s | {wl}")
    for eng in ("aikoql_mcp", "postgresql"):
        cells = result["multi_op_txn"][eng]
        print(f"  txn {eng}: " + ", ".join(
            f"{k}:{v['correct']}" for k, v in cells.items()))
    m = result["mcp_mode"]
    print(f"  mcp_mode: ingest {m['ingest_s']}s | " + ", ".join(
        f"{w['name']}:{w['correct']}" for w in m["workloads"] if w.get("n")))


if __name__ == "__main__":
    sys.exit(main())
