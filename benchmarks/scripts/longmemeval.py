#!/usr/bin/env python3
"""AG-61 — LongMemEval retrieval through aikoql's agent-memory path.

LongMemEval (Wu et al., 2024; MIT; dataset xiaowu0162/longmemeval, file
longmemeval_s) gives each question a haystack of ~50 chat sessions and the
ids of the sessions that hold the evidence. This harness measures the
retrieval half with the benchmark's own session-level metrics — no LLM, no
API key, no judge: recall_any@k (any evidence session in the top k),
recall_all@k (all of them) and NDCG@k, k in {5, 10}, over the non-abstention
questions (ids ending in _abs have no evidence session).

The path measured is the product's: `aikoql-mcp serve` over stdio. Every
user turn of a question's haystack is stored with `memory_store` as a
private memory of a per-question subject (the kernel ACL isolates the
questions inside one store); the question runs as `memory_search` for that
subject, and ranked turns collapse to ranked sessions (first occurrence).

    python3 benchmarks/scripts/longmemeval.py DATA.json [--limit N] [--out FILE]
        [--shards K --shard I]

Prints one JSON line per question type plus "overall"; --out writes the
whole result with the dataset hash, binary version, machine and the raw
rows. --shards/--shard run a round-robin slice of the questions (they are
independent — per-question subjects in one store); merge the shard files
with merge_lme_shards.py.
"""
import argparse
import hashlib
import json
import math
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time

KS = (5, 10)


class Mcp:
    def __init__(self, binary, db, memdir):
        # the bulk load is ~130k calls: the per-connection rate limit (300
        # calls/min by default) is off for the benchmark — recorded in the result
        cfg = os.path.join(os.path.dirname(db), "aikoql.toml")
        with open(cfg, "w") as f:
            f.write("[rate_limit]\nenabled = false\n")
        self.p = subprocess.Popen(
            [binary, "serve", db, "--memory-dir", memdir, "--config", cfg],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        self.n = 0
        self.call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}})

    def call(self, method, params):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("server closed stdout")
            try:
                frame = json.loads(line)
            except ValueError:
                continue
            if frame.get("id") == self.n:
                return frame

    def tool(self, name, args):
        r = self.call("tools/call", {"name": name, "arguments": args})
        if "result" not in r:
            raise RuntimeError(f"{name}: {r.get('error')}")
        text = r["result"]["content"][0]["text"]
        if r["result"].get("isError"):
            raise RuntimeError(f"{name}: {text}")
        try:
            return json.loads(text)
        except ValueError:
            return text

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=60)


def unfence(text, fence):
    # AG-33: undeclared results come back inside the per-response fence;
    # strip it with the markers the response itself declares (never by
    # guessing) before parsing the harness's own name format.
    o, c = fence.get("open", ""), fence.get("close", "")
    if o and text.startswith(o) and text.endswith(c):
        return text[len(o):len(text) - len(c)].strip()
    return text


def _unfence_self_check():
    f = {"open": "⟦untrusted aabbccdd⟧", "close": "⟦/untrusted aabbccdd⟧"}
    assert unfence("⟦untrusted aabbccdd⟧ s-7-t3 ⟦/untrusted aabbccdd⟧", f) == "s-7-t3"
    assert unfence("s-7-t3", f) == "s-7-t3"  # trusted content passes through
    assert unfence("⟦untrusted aabbccdd⟧ s-7-t3", f) != "s-7-t3"  # never half-strip


def ndcg(ranked, gold, k):
    dcg = sum(1 / math.log2(i + 2) for i, s in enumerate(ranked[:k]) if s in gold)
    ideal = sum(1 / math.log2(i + 2) for i in range(min(len(gold), k)))
    return dcg / ideal if ideal else 0.0


def summary(sel):
    keys = [k for k in sel[0] if k != "type"]
    return {"n": len(sel), **{k: round(sum(r[k] for r in sel) / len(sel), 4) for k in keys}}


def run(args):
    _unfence_self_check()
    if args.shard >= args.shards:
        sys.exit(f"shard {args.shard} >= shards {args.shards}")
    data = json.load(open(args.data))
    questions = [q for q in data if not q["question_id"].endswith("_abs")][: args.limit]
    # sharding: questions are independent (per-question subjects in one
    # store), so a shard is a round-robin slice — the merge script pools rows
    questions = questions[args.shard::args.shards]
    work = tempfile.mkdtemp(prefix="aikoql-lme-")
    mcp = Mcp(args.binary, os.path.join(work, "db"), os.path.join(work, "memory"))
    rows, stored, t0 = [], 0, time.time()
    try:
        for qi, q in enumerate(questions):
            subject = f"lme-{q['question_id']}"
            for sid, session in zip(q["haystack_session_ids"], q["haystack_sessions"]):
                for ti, turn in enumerate(session):
                    if turn["role"] != "user" or not turn["content"].strip():
                        continue
                    mcp.tool("memory_store", {
                        "subject": subject,
                        "name": f"s-{sid}-t{ti}",
                        "description": turn["content"][:200],
                        "content": turn["content"],
                    })
                    stored += 1
            hits = mcp.tool("memory_search", {
                "subject": subject,
                "query": q["question"],
                "max_results": 50,
                "wait_for_freshness_ms": 60000,
            })
            fence = hits.get("fence", {})
            ranked = []
            for h in hits.get("results", []):
                sid = unfence(h["name"], fence).rsplit("-t", 1)[0][2:]
                if sid not in ranked:
                    ranked.append(sid)
            gold = set(q["answer_session_ids"])
            row = {"type": q["question_type"]}
            for k in KS:
                top = set(ranked[:k])
                row[f"recall_any@{k}"] = float(bool(top & gold))
                row[f"recall_all@{k}"] = float(gold <= top)
                row[f"ndcg@{k}"] = ndcg(ranked, gold, k)
            rows.append(row)
            if (qi + 1) % 25 == 0:
                print(f"  {qi + 1}/{len(questions)} questions, {stored} turns, {time.time() - t0:.0f}s", file=sys.stderr)
    finally:
        mcp.close()
        shutil.rmtree(work, ignore_errors=True)

    by_type = {"overall": summary(rows)}
    for t in sorted({r["type"] for r in rows}):
        by_type[t] = summary([r for r in rows if r["type"] == t])
    version = subprocess.run([args.binary, "--version"], capture_output=True, text=True).stdout.strip()
    result = {
        "benchmark": "LongMemEval-S retrieval (session-level, non-abstention)",
        "dataset": {"repo": "xiaowu0162/longmemeval", "file": "longmemeval_s",
                    "sha256": hashlib.sha256(open(args.data, "rb").read()).hexdigest()},
        "system": {"binary": version, "path": "memory_store / memory_search over MCP stdio",
                   "granularity": "user turns, collapsed to sessions",
                   "embedding": "all-MiniLM-L6-v2 (local)",
                   "config": "[rate_limit] enabled = false (bulk load); everything else default"},
        "machine": f"{platform.system()} {platform.machine()} {platform.processor()}",
        "turns_stored": stored,
        "seconds": round(time.time() - t0, 1),
        "shards": args.shards,
        "shard": args.shard,
        "rows": rows,
        "results": by_type,
    }
    for t, s in by_type.items():
        print(json.dumps({"type": t, **s}))
    if args.out:
        with open(args.out, "w") as f:
            json.dump(result, f, indent=2)
            f.write("\n")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("data")
    ap.add_argument("--limit", type=int, default=None)
    ap.add_argument("--out")
    ap.add_argument("--binary", default="target/release/aikoql-mcp")
    ap.add_argument("--shards", type=int, default=1)
    ap.add_argument("--shard", type=int, default=0)
    run(ap.parse_args())
