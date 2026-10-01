#!/usr/bin/env bash
# D-03: protocol/compatibility.json is the one machine-readable source of
# the SDK ↔ server version contract. Every SDK MIN_SERVER_VERSION and the
# workspace version must agree with it — a bump that misses a constant is
# the exact drift this gate catches (the review's own RED).
#   protocol/compatibility.json: {api_major, minimum_server_version}
#   Go  : crates/sdk/go/aikoql.go        const MIN_SERVER_VERSION
#   Py  : crates/sdk/python/python/aikoql/mcp_client.py  MIN_SERVER_VERSION
#   ws  : Cargo.toml [workspace.package] version
# The gate itself is wired into the dag job's check chain (ci.yml), so it
# runs on every PR.
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
contract="$root/protocol/compatibility.json"
[ -f "$contract" ] || { echo "SDK COMPAT: missing $contract" >&2; exit 1; }
fail=0

# 1. well-formed contract: valid JSON, api_major == 1, minimum_server_version set
min="$(python3 - "$contract" <<'PY'
import json, sys
try:
    c = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception as e:
    print(f"SDK COMPAT: {sys.argv[1]} is not valid JSON: {e}", file=sys.stderr)
    sys.exit(1)
if c.get("api_major") != 1:
    print(f"SDK COMPAT: api_major {c.get('api_major')!r} != 1", file=sys.stderr)
    sys.exit(1)
v = c.get("minimum_server_version")
if not isinstance(v, str) or not v:
    print("SDK COMPAT: minimum_server_version missing or empty", file=sys.stderr)
    sys.exit(1)
print(v)
PY
)" || exit 1

# 2. workspace version
ws="$(grep -A2 '^\[workspace\.package\]' "$root/Cargo.toml" | grep '^version' | head -1 | sed 's/.*"\([^"]*\)".*/\1/')"
[ -n "$ws" ] || { echo "SDK COMPAT: workspace.package version not found" >&2; fail=1; }

# 3. Go constant
gogo="$(grep -o 'MIN_SERVER_VERSION *= *"[^"]*"' "$root/crates/sdk/go/aikoql.go" | head -1 | sed 's/.*"\([^"]*\)".*/\1/')"
[ -n "$gogo" ] || { echo "SDK COMPAT: Go MIN_SERVER_VERSION not found" >&2; fail=1; }

# 4. Python constant
pymin="$(grep -o 'MIN_SERVER_VERSION *= *"[^"]*"' "$root/crates/sdk/python/python/aikoql/mcp_client.py" | head -1 | sed 's/.*"\([^"]*\)".*/\1/')"
[ -n "$pymin" ] || { echo "SDK COMPAT: Python MIN_SERVER_VERSION not found" >&2; fail=1; }

# 5. all three equal the contract (a bump that missed one constant is drift)
for src in "workspace:$ws" "go:$gogo" "python:$pymin"; do
  if [ "${src#*:}" != "$min" ]; then
    echo "SDK COMPAT: $src != contract minimum_server_version $min" >&2
    fail=1
  fi
done

[ "$fail" -eq 0 ] && echo "SDK COMPAT: workspace/go/python all pinned to $min"
exit "$fail"
