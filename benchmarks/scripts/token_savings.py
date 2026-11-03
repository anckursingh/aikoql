#!/usr/bin/env python3
"""Deterministic token-savings harness: how many fewer tokens an agent
reads per turn when it pulls aikoql-compiled context slices instead of
re-reading the whole corpus. No LLM calls.

usage: token_savings.py --corpus DIR --queries FILE [--retriever overlap|aikoql]
"""

import argparse
import json
import subprocess
from pathlib import Path

try:
    import tiktoken

    ENC = tiktoken.get_encoding("cl100k_base")
    TOKENIZER = "cl100k_base"
except ImportError:  # offline fallback: word/1.3 heuristic, flagged in output
    ENC = None
    TOKENIZER = "heuristic"


def count_tokens(text: str) -> int:
    if not text:
        return 0
    if ENC is not None:
        return len(ENC.encode(text))
    return max(1, round(len(text.split()) / 1.3))


def corpus_files(corpus_dir: str):
    skip_dirs = {".git", "target", ".venv", "node_modules", ".superpowers", "kb.ckpt", "kb.artifacts"}
    for p in Path(corpus_dir).rglob("*"):
        if not p.is_file() or any(part in skip_dirs for part in p.parts):
            continue
        try:
            head = p.read_bytes()[:8192]
        except OSError:
            continue
        if b"\x00" in head:  # binary
            continue
        yield p


def baseline_tokens(corpus_dir: str) -> int:
    total = 0
    for p in corpus_files(corpus_dir):
        total += count_tokens(p.read_text(encoding="utf-8", errors="ignore"))
    return total


def overlap_retrieve(query: str, corpus_dir: str) -> str:
    """Fixture retriever: best keyword-overlap file. Deterministic, offline."""
    words = {w for w in query.lower().replace("?", "").split() if w.isalnum()}
    best, best_score = "", -1
    for p in corpus_files(corpus_dir):
        text = p.read_text(encoding="utf-8", errors="ignore")
        score = len(words.intersection({w.lower() for w in text.split()}))
        if score > best_score:
            best, best_score = text, score
    return best


def aikoql_retrieve(query: str, proc: subprocess.Popen) -> str:
    """One recall call (AG-02 hybrid retrieval) over MCP stdio."""
    call = {
        "jsonrpc": "2.0", "id": 100,
        "method": "tools/call",
        "params": {"name": "recall",
                   "arguments": {"query": query, "k": 10, "max_tokens": 2000,
                                 "subject": "ingest-dir"}},
    }
    proc.stdin.write(json.dumps(call) + "\n")
    proc.stdin.flush()
    line = proc.stdout.readline()
    if not line:
        raise RuntimeError("aikoql-mcp stdio closed during recall")
    result = json.loads(line)
    if "error" in result:
        raise RuntimeError(f"recall error: {result['error']}")
    # what enters the agent's context: the tool result's text content,
    # not the wire envelope (whose unicode escaping inflates the count)
    texts = [c.get("text", "") for c in result["result"].get("content", [])]
    return "\n".join(texts)


def start_aikoql(corpus_dir: str):
    """Ingest the corpus into a fresh db, then serve it over stdio."""
    import os
    import tempfile

    bin_cmd = os.environ.get("AIKOQL_BIN", "npx -y aikoql-mcp").split()
    # AIKOQL_DB: serve a pre-built db instead of ingesting (big-corpus reruns)
    db = os.environ.get("AIKOQL_DB")
    if db is None:
        db = tempfile.mkdtemp(prefix="aikoql-bench-") + "/kb"
        subprocess.run(
            bin_cmd + ["ingest-dir", corpus_dir, db],
            check=True, capture_output=True, text=True,
        )
    proc = subprocess.Popen(
        bin_cmd + ["serve", db],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
    )
    proc.stdin.write(json.dumps({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                   "clientInfo": {"name": "token-savings-harness", "version": "1.0"}},
    }) + "\n")
    proc.stdin.flush()
    line = proc.stdout.readline()
    if not line or "error" in json.loads(line):
        raise RuntimeError(f"initialize failed: {line}")
    proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    proc.stdin.flush()
    return proc


def harness(corpus_dir: str, queries_file: str, retriever=None) -> dict:
    """baseline = full-corpus tokens; memory = tokens of per-query retrieved slices."""
    if retriever is None:
        retriever = lambda q: overlap_retrieve(q, corpus_dir)
    queries = json.loads(Path(queries_file).read_text())
    corpus = baseline_tokens(corpus_dir)
    # Per-turn framing: without memory the agent re-reads the corpus every
    # turn; with it, one retrieved slice per turn.
    baseline = corpus * len(queries)
    memory = sum(count_tokens(retriever(q)) for q in queries)
    pct = round((1 - memory / baseline) * 100, 2) if baseline else 0.0
    import platform

    return {
        "baseline_tokens": baseline,
        "memory_tokens": memory,
        "reduction_pct": pct,
        "tokenizer": TOKENIZER,
        "queries": len(queries),
        "corpus_tokens": corpus,
        "machine": f"{platform.system()} {platform.machine()}",
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--queries", required=True)
    ap.add_argument("--retriever", choices=["overlap", "aikoql"], default="aikoql")
    args = ap.parse_args()
    if args.retriever == "aikoql":
        proc = start_aikoql(args.corpus)
        retriever = lambda q: aikoql_retrieve(q, proc)
        result = harness(args.corpus, args.queries, retriever)
        import os

        result["binary"] = (
            f"local binary: {os.environ['AIKOQL_BIN']}"
            if os.environ.get("AIKOQL_BIN")
            else "aikoql-mcp (npm, latest)"
        )
        proc.terminate()
    else:
        result = harness(args.corpus, args.queries)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
