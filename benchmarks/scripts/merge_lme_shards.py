#!/usr/bin/env python3
"""AG-61 — merge LongMemEval shard results into the committed result.

Shards are round-robin question slices of the same dataset file, so the
metrics pool: rows are concatenated and the per-type summaries recomputed
with the harness's own summary code. Fails on a dataset-hash mismatch.

    python3 benchmarks/scripts/merge_lme_shards.py SHARD.json... [--out FILE]
"""
import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from longmemeval import summary  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("shards", nargs="+")
    ap.add_argument("--out", required=True)
    ap.add_argument("--profile", default="debug", help="build profile the runs used")
    args = ap.parse_args()

    shards = [json.load(open(p)) for p in args.shards]
    hashes = {s["dataset"]["sha256"] for s in shards}
    assert len(hashes) == 1, f"shards disagree on the dataset hash: {hashes}"
    rows = [r for s in shards for r in s["rows"]]
    by_type = {"overall": summary(rows)}
    for t in sorted({r["type"] for r in rows}):
        by_type[t] = summary([r for r in rows if r["type"] == t])
    merged = {
        "benchmark": shards[0]["benchmark"],
        "dataset": shards[0]["dataset"],
        "system": dict(shards[0]["system"], shards=len(shards),
                       binary_profile=args.profile),
        "machine": shards[0]["machine"],
        "turns_stored": sum(s["turns_stored"] for s in shards),
        "wall_seconds": max(s["seconds"] for s in shards),
        "results": by_type,
    }
    with open(args.out, "w") as f:
        json.dump(merged, f, indent=2)
        f.write("\n")
    for t, s in by_type.items():
        print(json.dumps({"type": t, **s}))


if __name__ == "__main__":
    main()
