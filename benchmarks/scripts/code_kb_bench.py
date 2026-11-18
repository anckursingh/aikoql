#!/usr/bin/env python3
"""Code knowledge-base benchmark: index a Rust codebase with and without the embedding
model, then measure what an agent cares about. No LLM calls, deterministic given --seed.

  ingest   wall time, entities stored, size on disk (embeddings on vs off)
  quality  automatic questions with a known answer: a documented function is chosen at
           random and asked about by its doc comment, WITH the words of its own name
           removed (so a plain name match cannot win); the answer is a hit if the
           function appears in compile_context's markdown, and its rank is recorded
  speed    server start, first (cold) query, warm queries (median / p95 / max)
  size     compile_context markdown per answer (tokens ~ bytes / 4) and a rough grep baseline

usage: code_kb_bench.py --binary BIN --repo DIR [--subdir crates] [--n 40] [--seed 1]
                        [--budget 800] [--configs embed,noembed] [--json OUT.json] [--keep]
Honest limits: a self-generated question set measures "doc comment -> function", an
approximation of natural-language lookup; the grep baseline is a crude proxy; one machine.
"""
import argparse, json, os, random, re, shutil, statistics, subprocess, sys, tempfile, time
from pathlib import Path

DOC = re.compile(r"((?:[ \t]*///[^\n]*\n)+)[ \t]*(?:#\[[^\n]*\]\s*)*(?:pub(?:\([a-z]+\))?\s+)?(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?fn\s+([a-zA-Z_][a-zA-Z0-9_]*)")
STOP = set("the a an of to in is it and or for on with as by be this that from at are was when which if not no its into than then can will a".split())


def rust_files(root: Path):
    for p in root.rglob("*.rs"):
        if "target" in p.parts or "tests" in p.parts or p.name.endswith("_test.rs"):
            continue
        yield p


def build_questions(root: Path, n: int, seed: int):
    defs = {}
    for p in rust_files(root):
        try:
            src = p.read_text(errors="ignore")
        except OSError:
            continue
        for m in DOC.finditer(src):
            name = m.group(2)
            doc = " ".join(l.strip().lstrip("/").strip() for l in m.group(1).splitlines())
            defs.setdefault(name, []).append((str(p.relative_to(root)), doc))
    pool = []
    for name, occ in defs.items():
        if len(occ) != 1 or len(name) < 5 or name.startswith("test") or name in ("new", "default", "from", "fmt"):
            continue  # ambiguous or generic names cannot be graded
        file, doc = occ[0]
        first = re.split(r"(?<=[.!?])\s", doc)[0]
        name_words = set(re.split(r"_+", name.lower()))
        words = [w for w in re.findall(r"[A-Za-z][A-Za-z0-9]+", first) if w.lower() not in name_words and w.lower() not in STOP]
        if len(first) >= 40 and len(words) >= 4:
            pool.append({"name": name, "file": file, "question": " ".join(words)})
    random.Random(seed).shuffle(pool)
    return sorted(pool[:n], key=lambda q: q["name"])


def toks(b: int) -> int:
    return b // 4  # bytes/4 heuristic, stated in the output


class Mcp:
    def __init__(self, binary, kb, model_dir):
        t0 = time.perf_counter()
        self.p = subprocess.Popen([binary, "serve", kb, "--model-dir", model_dir], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, bufsize=1)
        self.n = 0
        self.call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}})
        self.start_ms = (time.perf_counter() - t0) * 1000

    def call(self, method, params):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            m = json.loads(self.p.stdout.readline())
            if m.get("id") == self.n:
                return m

    def context(self, koid, task, budget, as_ingest):
        args = {"koid": koid, "task": task, "token_budget": budget, "include_package": False}
        if as_ingest:  # servers before 0.5.2 own the ingested knowledge as "ingest-dir"
            args.update({"subject": "ingest-dir", "roles": ["admin"]})
        r = self.call("tools/call", {"name": "compile_context", "arguments": args})
        try:
            d = json.loads(r["result"]["content"][0]["text"])
            return (d.get("data", d)).get("context_markdown", "")
        except Exception:
            return ""

    def close(self):
        self.p.kill()


def grep_baseline(root: Path, q, subdir):
    kws = sorted({w.lower() for w in re.findall(r"[A-Za-z]{4,}", q["question"])}, key=len, reverse=True)[:3]
    out = subprocess.run(["grep", "-rniE", "|".join(kws), "--include=*.rs", str(root / subdir)], capture_output=True, text=True).stdout
    return len(out), (q["file"] in out)


def run_config(name, a, root, questions, work):
    models = work / f"models-{name}"
    models.mkdir(parents=True, exist_ok=True)
    model_dir = str(models) if name == "noembed" else os.path.expanduser("~/.aikoql/models")
    kb = str(work / f"kb-{name}")
    shutil.rmtree(kb, ignore_errors=True)
    t0 = time.perf_counter()
    r = subprocess.run([a.binary, "ingest-dir", str(root / a.subdir), kb, "--parallel", "--model-dir", model_dir], capture_output=True, text=True)
    ingest_s = time.perf_counter() - t0
    out = r.stdout + "\n" + r.stderr  # the "Embedded n/n entities" line is on stderr
    koid = next((l.strip()[6:] for l in out.splitlines() if l.strip().startswith("KOID: ")), None)
    ents = re.search(r"Stored (\d+) entity KOs, (\d+) file KOs and (\d+) relationships", out)
    emb = re.search(r"Embedded (\d+)/(\d+) entities.*? in ([0-9.]+)s", out)
    size_mb = sum(f.stat().st_size for f in Path(kb).rglob("*") if f.is_file()) / 1048576 + \
        sum(f.stat().st_size for f in Path(kb + ".artifacts").rglob("*") if f.is_file()) / 1048576 if Path(kb + ".artifacts").exists() else \
        sum(f.stat().st_size for f in Path(kb).rglob("*") if f.is_file()) / 1048576
    res = {"config": name, "ingest_s": round(ingest_s, 2), "koid": koid, "disk_mb": round(size_mb, 1),
           "entities": int(ents.group(1)) if ents else None, "files": int(ents.group(2)) if ents else None,
           "relationships": int(ents.group(3)) if ents else None,
           "embedded": f"{emb.group(1)}/{emb.group(2)}" if emb else "none", "embed_s": float(emb.group(3)) if emb else 0.0}
    if not koid:
        res["error"] = "ingest produced no KOID: " + out[-600:]
        return res
    m = Mcp(a.binary, kb, model_dir)
    lat, hits, ranks, mdbytes, gbytes, ghits = [], 0, [], [], [], 0
    for q in questions:
        s = time.perf_counter()
        md = m.context(koid, q["question"], a.budget, not a.no_ingest_identity)
        lat.append((time.perf_counter() - s) * 1000)
        mdbytes.append(len(md))
        bullets = [l for l in md.splitlines() if l.startswith("- **")]
        rank = next((i + 1 for i, l in enumerate(bullets) if re.search(r"\b" + re.escape(q["name"]) + r"\b", l)), None)
        if rank:
            hits += 1
            ranks.append(rank)
        gb, gh = grep_baseline(root, q, a.subdir)
        gbytes.append(gb)
        ghits += gh
    m.close()
    warm = lat[1:] or lat
    n = len(questions)
    res.update({
        "server_start_ms": round(m.start_ms), "first_query_ms": round(lat[0]),
        "warm_median_ms": round(statistics.median(warm)), "warm_p95_ms": round(sorted(warm)[int(0.95 * (len(warm) - 1))]),
        "warm_max_ms": round(max(warm)), "questions": n,
        "hit_rate": round(hits / n, 3), "mrr": round(sum(1 / r for r in ranks) / n, 3),
        "hit_at_3": round(sum(1 for r in ranks if r <= 3) / n, 3),
        "answer_tokens_median": toks(int(statistics.median(mdbytes))),
        "grep_tokens_median": toks(int(statistics.median(gbytes))), "grep_found_file": round(ghits / n, 3)})
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--subdir", default="crates")
    ap.add_argument("--n", type=int, default=40)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--budget", type=int, default=800)
    ap.add_argument("--configs", default="embed,noembed")
    ap.add_argument("--json")
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--no-ingest-identity", action="store_true", help="the server is 0.5.2 or newer")
    a = ap.parse_args()
    root = Path(a.repo)
    qs = build_questions(root / a.subdir, a.n, a.seed)
    qs = [{**q, "file": str(Path(a.subdir) / q["file"])} for q in qs]
    work = Path(tempfile.mkdtemp(prefix="code-kb-bench-"))
    version = subprocess.run([a.binary, "--version"], capture_output=True, text=True).stdout.strip()
    results = [run_config(c, a, root, qs, work) for c in a.configs.split(",")]
    meta = {"binary": version, "repo": str(root), "subdir": a.subdir, "questions": len(qs), "seed": a.seed, "budget_tokens": a.budget,
            "token_estimate": "bytes/4", "machine": os.uname().machine + " " + os.uname().sysname, "date": time.strftime("%Y-%m-%d")}
    print(json.dumps({"meta": meta, "results": results}, indent=2))
    if a.json:
        Path(a.json).write_text(json.dumps({"meta": meta, "results": results, "questions": qs}, indent=2))
    if not a.keep:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
