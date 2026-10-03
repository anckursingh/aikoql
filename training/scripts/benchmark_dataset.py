"""benchmark_dataset.py — T-13 §27 cells: throughput, rates, size.

Runs the full generate pipeline against a live server (the 2-service
fixture end to end) and reports one JSON object with three cells:
throughput (wall seconds, example count, examples/second), rates
(the pipeline's derived rates from --metrics) and size (dataset bytes
on disk, per-split counts). Laptop scale by design — the
corpus-scale cell arrives with T-14's dataset.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time

_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_SRC = os.path.join(_ROOT, "src")


def _size(path):
    total = 0
    for dirpath, _, names in os.walk(path):
        for name in names:
            total += os.path.getsize(os.path.join(dirpath, name))
    return total


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--db", required=True, help="host:port of aikoql-mcp")
    ap.add_argument("--token", required=True, help="TCP access token")
    ap.add_argument("--out", required=True, help="dataset output directory")
    ap.add_argument("--seed", type=int, default=0)
    args = ap.parse_args(argv)

    env = dict(os.environ)
    env["PYTHONPATH"] = _SRC + os.pathsep + env.get("PYTHONPATH", "")
    metrics_path = os.path.join(args.out, "metrics.json")
    start = time.monotonic()
    proc = subprocess.run(
        [sys.executable, "-m", "aikoql_training.cli", "generate",
         "--db", args.db, "--token", args.token, "--out", args.out,
         "--seed", str(args.seed), "--metrics", metrics_path],
        env=env, capture_output=True, text=True)
    wall = time.monotonic() - start
    if proc.returncode != 0:
        print(proc.stderr, file=sys.stderr)
        return proc.returncode
    report = json.loads(proc.stdout)
    with open(metrics_path, encoding="utf-8") as fh:
        metrics = json.load(fh)
    n = report["example_count"]
    cells = {
        "throughput": {"wall_s": round(wall, 3), "examples": n,
                       "examples_per_second": round(n / wall, 3)},
        "rates": metrics["rates"],
        "size": {"bytes": _size(args.out),
                 "splits": {k: metrics["counts"].get(f"split_{k}", 0)
                            for k in ("train", "val", "test")}},
    }
    print(json.dumps(cells, sort_keys=True, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
