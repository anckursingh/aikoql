#!/usr/bin/env python3
"""Compare two criterion `--save-baseline` directories and fail on regression.

Walks both directories for `estimates.json`, matches benchmarks by relative
path, and compares mean point estimates. Exits 1 if any benchmark regressed
by more than --threshold percent (relative, not absolute — R14 §Tasks 4/5).

Usage:
    python3 benchmarks/scripts/compare_benchmarks.py <previous_dir> <current_dir> [--threshold 20]

`previous_dir` may be missing (first run) — exits 0 with a note.
New benchmarks present only in `current_dir` are reported, not failed.
"""

import argparse
import json
import pathlib
import sys


def collect_estimates(root: pathlib.Path) -> dict:
    out = {}
    for path in sorted(root.rglob("estimates.json")):
        rel = path.relative_to(root)
        with path.open() as f:
            data = json.load(f)
        out[str(rel)] = data["mean"]["point_estimate"]
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("previous", type=pathlib.Path)
    parser.add_argument("current", type=pathlib.Path)
    parser.add_argument("--threshold", type=float, default=20.0)
    args = parser.parse_args()

    if not args.previous.is_dir():
        print(f"no previous baseline at {args.previous} — first run, saving baseline only")
        return 0
    if not args.current.is_dir():
        print(f"no current baseline at {args.current}", file=sys.stderr)
        return 2

    prev = collect_estimates(args.previous)
    curr = collect_estimates(args.current)

    new = sorted(set(curr) - set(prev))
    gone = sorted(set(prev) - set(curr))
    regressions = []

    for key in sorted(set(prev) & set(curr)):
        old, new_mean = prev[key], curr[key]
        if old <= 0.0:
            continue
        delta_pct = (new_mean - old) / old * 100.0
        marker = "REGRESSION" if delta_pct > args.threshold else "ok"
        line = f"{key:70s} {old:>14.3f} -> {new_mean:>14.3f} ({delta_pct:+8.2f}%) {marker}"
        print(line)
        if delta_pct > args.threshold:
            regressions.append(line)

    for key in new:
        print(f"{key:70s} new benchmark, no previous value")
    for key in gone:
        print(f"{key:70s} removed, no current value")

    print(f"\n{len(prev)} benchmarks compared, {len(regressions)} regressed >{args.threshold}%")
    if regressions:
        print("\nREGRESSIONS:\n" + "\n".join(regressions), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
