#!/usr/bin/env python3
"""AG-61 — the memory cells long-term-memory benchmarks do not measure.

Driven through `aikoql-mcp serve` over stdio, like longmemeval.py:

- contradiction: a fact, then a conflicting value for the same key in a
  later conversation (remember_facts). Pass = a Conflict is recorded AND the
  original stays readable unchanged (never a silent overwrite); a verbatim
  repeat must come back as a duplicate, not a second object.
- provenance: every fact explain()s with an evidence record naming its
  source (the conversation), method and location (the message range).
- time_travel: an object updated twice; `MATCH ... AS_OF <commit time>`
  must return the value as it was at each version.

    python3 benchmarks/scripts/memory_cells.py [--n 100] [--out FILE]
"""
import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from longmemeval import Mcp  # noqa: E402


def contradiction_and_provenance(m, n):
    conflicts = kept = dupes = prov = 0
    for i in range(n):
        key = f"pref.{i}"
        first = m.tool("remember_facts", {"conversation_id": f"c{i}-a", "facts": [
            {"statement": f"User {i} prefers tea in the morning", "key": key, "value": "tea",
             "messages": [2, 3]}]})["results"][0]
        koid = first["koid"]
        again = m.tool("remember_facts", {"conversation_id": f"c{i}-b", "facts": [
            {"statement": f"user {i} prefers tea in the morning."}]})["results"][0]
        dupes += again["status"] == "duplicate" and again["koid"] == koid
        later = m.tool("remember_facts", {"conversation_id": f"c{i}-c", "facts": [
            {"statement": f"User {i} prefers coffee in the morning", "key": key, "value": "coffee"}]})["results"][0]
        conflicts += later["status"] == "conflict" and bool(later.get("conflict"))
        original = m.tool("get", {"koid": koid})
        kept += original.get("properties", {}).get("value") == "tea"
        rec = (m.tool("explain", {"koid": koid}).get("evidence_records") or [{}])[0]
        prov += (rec.get("source_artifact") == f"conversation:c{i}-a"
                 and rec.get("method") == "llm_inference"
                 and rec.get("location") == "messages 2-3")
    return {
        "contradiction": {"n": n, "conflict_recorded": conflicts / n, "original_kept": kept / n,
                          "repeat_deduplicated": dupes / n},
        "provenance": {"n": n, "complete_evidence": prov / n},
    }


def time_travel(m, n):
    ok = 0
    for i in range(n):
        r = m.tool("remember", {"type_name": "Ledger", "properties": {"tag": f"tt{i}", "balance": 0}})
        koid, versions = r["koid"], []
        for v in (0, 1, 2):
            if v:
                r = m.tool("remember", {"koid": koid, "expected_version": r["version"],
                                        "type_name": "Ledger",
                                        "properties": {"tag": f"tt{i}", "balance": v}})
            versions.append((m.tool("get", {"koid": koid})["commit_ts"], v))
        good = True
        for ts, v in versions:
            rows = m.tool("aikoql", {"query": f'MATCH Ledger WHERE tag == "tt{i}" AS_OF {ts} RETURN *'})
            seen = [r["properties"]["balance"] for r in rows.get("results", [])]
            good &= seen == [v]  # exactly the version at ts — one row, its value
        ok += good
    return {"time_travel": {"n": n, "versions_per_object": 3, "as_of_correct": ok / n}}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=100)
    ap.add_argument("--out")
    ap.add_argument("--binary", default="target/release/aikoql-mcp")
    a = ap.parse_args()
    work = tempfile.mkdtemp(prefix="aikoql-cells-")
    m = Mcp(a.binary, os.path.join(work, "db"), os.path.join(work, "memory"))
    try:
        res = contradiction_and_provenance(m, a.n)
        res.update(time_travel(m, a.n // 2))
    finally:
        m.close()
        shutil.rmtree(work, ignore_errors=True)
    out = {"benchmark": "aikoql memory cells (AG-61)",
           "binary": subprocess.run([a.binary, "--version"], capture_output=True, text=True).stdout.strip(),
           "machine": f"{platform.system()} {platform.machine()}", "results": res}
    print(json.dumps(res))
    if a.out:
        with open(a.out, "w") as f:
            json.dump(out, f, indent=2)
            f.write("\n")


if __name__ == "__main__":
    main()
