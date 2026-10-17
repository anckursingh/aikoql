#!/usr/bin/env bash
# L-23 (TDD-034): strong-claims evidence audit. The review class (P1
# evidence gaps): strong claims in the code — complexity (O(...)),
# boundedness, exactly-once delivery, zero allocation — that no test
# evidences; the claim sells what the suite never checks. Every claim in
# the audited scope (the launch's storage estate: crates/storage/
# aikoql-v2 + crates/kernel) must map, in tests/strong-claims.toml, to
# an evidence test that EXISTS — or the claim loses its claim (the text
# softened, its row removed with it). The gate:
#   1. sweeps the estate's comment lines for the claim vocabulary;
#   2. every hit must be pinned by a registry row (file:line pair);
#   3. every row's evidence test must exist in the tree — anchored at
#      the signature boundary, the L-20 dead-name lesson;
#   4. every row must still carry its claim (its pinned line still
#      matches the vocabulary) — no dead rows.
# The mutation harness mut-m10 damages the registry (drops a row); the
# sweep's uncovered-hit leg catches it.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
reg="$root/tests/strong-claims.toml"
[ -f "$reg" ] || { echo "STRONG CLAIMS: missing $reg" >&2; exit 1; }

python3 - "$root" "$reg" <<'PYEOF'
import re, subprocess, sys
# Windows consoles are cp1252 — claim text carries arrows/²/—; a hard
# UnicodeEncodeError mid-print would truncate the sweep (it died on the
# last hit at RED time). Reconfigure so output never crashes the gate.
if hasattr(sys.stdout, 'reconfigure'):
    sys.stdout.reconfigure(encoding='utf-8', errors='replace')
root, reg = sys.argv[1], sys.argv[2]
vocab = re.compile(r'\b(exactly.once|at most once|zero.alloc)\b|\bO\(|\bbounded\b')
comment = re.compile(r'^\s*(//|///|//!)')
bad = 0

# the sweep: every claim hit in the estate's comment lines
hits = {}
for f in subprocess.run(['git', '-C', root, 'ls-files', 'crates/storage/aikoql-v2', 'crates/kernel'],
                        capture_output=True, text=True).stdout.split():
    if not f.endswith('.rs'):
        continue
    try:
        lines = open(root + '/' + f, encoding='utf-8').read().splitlines()
    except OSError:
        continue
    for i, line in enumerate(lines, 1):
        if comment.match(line) and vocab.search(line):
            hits[(f, i)] = line.strip()

# the registry: [[claim]] file/line/text/evidence rows
rows = {}
toml = open(reg, encoding='utf-8').read()
for m in re.finditer(r'\[\[claim\]\]\s*file = "([^"]+)"\s*line = (\d+)\s*text = "((?:[^"\\]|\\.)*)"\s*evidence = "([^"]+)"', toml):
    rows[(m.group(1), int(m.group(2)))] = (m.group(3), m.group(4))

# 2. every hit pinned
for (f, l), text in sorted(hits.items()):
    if (f, l) not in rows:
        print(f"STRONG CLAIMS: uncovered claim at {f}:{l} - register it with an evidence test, or the claim loses its claim")
        print(f"  {text}")
        bad = 1

# 3+4. every row: its line still claims, and its evidence still exists
for (f, l), (text, ev) in sorted(rows.items()):
    try:
        line = open(root + '/' + f, encoding='utf-8').read().splitlines()[l - 1]
    except (OSError, IndexError):
        print(f"STRONG CLAIMS: dead row — {f}:{l} is gone")
        bad = 1
        continue
    if not (comment.match(line) and vocab.search(line)):
        print(f"STRONG CLAIMS: dead row — {f}:{l} no longer carries the claim")
        bad = 1
        continue
    if ev == "removed":
        continue
    ok = subprocess.run(['grep', '-rlF', f'fn {ev}(', root + '/crates',
                         '--include=*.rs', '--exclude-dir=target'],
                        capture_output=True, text=True).stdout.strip()
    if not ok:
        print(f"STRONG CLAIMS: evidence test '{ev}' for {f}:{l} no longer exists")
        bad = 1

if bad:
    sys.exit(1)
print("strong-claims audit — OK")
PYEOF
