"""Run c1_c8.py N times; per check: verdicts and whether the semantic leg ran."""
import collections
import json
import pathlib
import subprocess
import sys

SRC = pathlib.Path(__file__).parent
sys.path.insert(0, str(SRC))
from mcp import HERE  # noqa: E402  (the work dir)
n = int(sys.argv[1]) if len(sys.argv) > 1 else 5
runs = []
for i in range(n):
    (HERE / "results.json").unlink(missing_ok=True)  # never summarise a stale run
    done = subprocess.run([sys.executable, str(SRC / "c1_c8.py")], cwd=SRC, stdout=subprocess.DEVNULL,
                          stderr=subprocess.PIPE, text=True,
                          env={**__import__("os").environ, "PYTHONIOENCODING": "utf-8"})
    if done.returncode != 0:
        sys.exit(f"run {i + 1} failed: {done.stderr[-2000:]}")
    runs.append(json.loads((HERE / "results.json").read_text(encoding="utf-8")))
    print(f"run {i + 1}/{n} done", flush=True)
by = collections.OrderedDict()
for run in runs:
    for r in run:
        e = by.setdefault(r["id"], {"level": r["level"], "title": r["title"], "verdicts": [], "semantic": [],
                                    "evidence": r["evidence"]})
        e["verdicts"].append(r["verdict"])
        e["semantic"] += [x for x in (r.get("semantic") or [])]
summary = []
for cid, e in by.items():
    c = collections.Counter(e["verdicts"])
    sem = f"{sum(e['semantic'])}/{len(e['semantic'])}" if e["semantic"] else "-"
    summary.append({"level": e["level"], "id": cid, "title": e["title"], "pass": c["PASS"], "fail": c["FAIL"],
                    "gap": c["GAP"], "runs": len(e["verdicts"]), "semantic_calls": sem, "evidence": e["evidence"]})
    print(f"{e['level']} {cid:<15} pass {c['PASS']}/{len(e['verdicts'])} fail {c['FAIL']} gap {c['GAP']}  semantic {sem:<6} {e['title']}")
(HERE / "summary.json").write_text(json.dumps(summary, indent=1, ensure_ascii=False), encoding="utf-8")
