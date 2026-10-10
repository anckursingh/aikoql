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

# 6. api-v1.json: the frozen canonical surface — valid JSON, api_major 1,
#    the §4.1 lifecycle names, the §4.2 operation set; every name must
#    appear in docs/DATABASE-API.md (JSON = machine pin, doc = prose
#    contract — a rename in one but not the other is the exact drift).
apiv1="$root/protocol/api-v1.json"
[ -f "$apiv1" ] || { echo "SDK COMPAT: missing $apiv1" >&2; exit 1; }
if ! python3 - "$apiv1" "$root/docs/DATABASE-API.md" <<'PY'
import json, re, sys
api = json.load(open(sys.argv[1], encoding="utf-8"))
doc = open(sys.argv[2], encoding="utf-8").read()
LIFECYCLE = ["Database", "Connection", "Session", "Transaction",
             "Statement", "Result", "Row"]
OPS = ["connect", "close", "ping", "health", "remember", "get", "update",
       "delete", "execute", "query", "prepare", "begin", "commit",
       "rollback", "batch", "find_similar", "relate", "traverse",
       "create_schema", "discover_schema", "create_index", "drop_index",
       "explain", "trace", "prove", "backup", "restore", "metrics"]
errs = []
if api.get("api_major") != 1:
    errs.append(f"api_major {api.get('api_major')!r} != 1")
life = api.get("lifecycle")
if sorted(life) != sorted(LIFECYCLE):
    errs.append(f"lifecycle {life!r} != the 4.1 set")
names = sorted(o.get("name") for o in api.get("operations", []))
if names != sorted(OPS):
    errs.append(f"operations {names!r} != the 4.2 set")
st = api.get("streaming", {})
if st.get("query") != "ResultSet" or "query_stream" not in st or \
        st.get("cancellation") is not True:
    errs.append(f"streaming {st!r} must carry query/query_stream/cancellation (4.3)")
for name in LIFECYCLE + OPS:
    if not re.search(rf"\b{name}\b", doc):
        errs.append(f"{name} missing from docs/DATABASE-API.md")
if errs:
    for e in errs:
        print(f"SDK COMPAT: api-v1: {e}", file=sys.stderr)
    sys.exit(1)
print("api-v1 contract OK")
PY
then
  fail=1
fi

# 7. errors.json: the frozen SDK-012 taxonomy — exactly the 14 codes,
#    each carrying code/message/retryable/suggestion; every code must
#    appear in docs/DATABASE-API.md's error table.
errf="$root/protocol/errors.json"
[ -f "$errf" ] || { echo "SDK COMPAT: missing $errf" >&2; exit 1; }
if ! python3 - "$errf" "$root/docs/DATABASE-API.md" <<'PY'
import json, re, sys
er = json.load(open(sys.argv[1], encoding="utf-8"))
doc = open(sys.argv[2], encoding="utf-8").read()
CODES = ["AUTHENTICATION_FAILED", "AUTHORIZATION_FAILED", "NOT_FOUND",
         "INVALID_ARGUMENT", "INVALID_QUERY", "CONFLICT",
         "VERSION_MISMATCH", "TIMEOUT", "CANCELLED", "RESOURCE_EXHAUSTED",
         "UNAVAILABLE", "INTERNAL", "PROTOCOL_ERROR", "DATA_CORRUPTION"]
errs = []
if er.get("api_major") != 1:
    errs.append(f"api_major {er.get('api_major')!r} != 1")
codes = [c.get("code") for c in er.get("codes", [])]
if sorted(codes) != sorted(CODES):
    errs.append(f"codes {codes!r} != the SDK-012 taxonomy")
for c in er.get("codes", []):
    for k in ("code", "message", "suggestion"):
        if not isinstance(c.get(k), str) or not c[k].strip():
            errs.append(f"{c.get('code')}: {k} missing or empty")
    if not isinstance(c.get("retryable"), bool):
        errs.append(f"{c.get('code')}: retryable must be a bool")
for code in CODES:
    if not re.search(rf"\b{code}\b", doc):
        errs.append(f"{code} missing from docs/DATABASE-API.md")
if errs:
    for e in errs:
        print(f"SDK COMPAT: errors: {e}", file=sys.stderr)
    sys.exit(1)
print("error taxonomy OK")
PY
then
  fail=1
fi

# 8. D-05: protocol/test-vectors/ — the shared language-neutral operation
#    vectors (§7) plus the tests/sdk-conformance/ skeleton. Every vector is
#    valid JSON with a name and a non-empty operations list; every op must
#    sit in the frozen api-v1 set and every expect_error in the frozen
#    SDK-012 taxonomy — a vector referencing an unfrozen op is drift. All
#    13 conformance category dirs must exist.
tvd="$root/protocol/test-vectors"
[ -d "$tvd" ] || { echo "SDK COMPAT: missing $tvd" >&2; exit 1; }
if ! python3 - "$tvd" "$root/tests/sdk-conformance" <<'PY'
import json, os, sys
tv, cs = sys.argv[1], sys.argv[2]
OPS = ["connect", "close", "ping", "health", "remember", "get", "update",
       "delete", "execute", "query", "prepare", "begin", "commit",
       "rollback", "batch", "find_similar", "relate", "traverse",
       "create_schema", "discover_schema", "create_index", "drop_index",
       "explain", "trace", "prove", "backup", "restore", "metrics"]
CODES = ["AUTHENTICATION_FAILED", "AUTHORIZATION_FAILED", "NOT_FOUND",
         "INVALID_ARGUMENT", "INVALID_QUERY", "CONFLICT",
         "VERSION_MISMATCH", "TIMEOUT", "CANCELLED", "RESOURCE_EXHAUSTED",
         "UNAVAILABLE", "INTERNAL", "PROTOCOL_ERROR", "DATA_CORRUPTION"]
errs = []
vecs = [os.path.join(dp, f) for dp, _, fs in os.walk(tv)
        for f in fs if f.endswith(".json")]
if not vecs:
    errs.append("no vectors in protocol/test-vectors/")
for path in sorted(vecs):
    rel = os.path.relpath(path, tv).replace(os.sep, "/")
    try:
        v = json.load(open(path, encoding="utf-8"))
    except Exception as e:
        errs.append(f"{rel}: not valid JSON ({e})")
        continue
    if not isinstance(v.get("name"), str) or not v["name"].strip():
        errs.append(f"{rel}: missing vector name")
    ops = v.get("operations")
    if not isinstance(ops, list) or not ops:
        errs.append(f"{rel}: operations must be a non-empty list")
        continue
    for i, o in enumerate(ops):
        op = o.get("op") if isinstance(o, dict) else None
        if op not in OPS:
            errs.append(f"{rel}: operation {i} op {op!r} not in the frozen api-v1 set")
        if isinstance(o, dict) and "expect_error" in o and \
                o["expect_error"] not in CODES:
            errs.append(f"{rel}: operation {i} expect_error "
                        f"{o['expect_error']!r} not in the SDK-012 taxonomy")
CATS = ["protocol", "crud", "query", "transaction", "graph", "vector",
        "schema", "errors", "auth", "streaming", "cancellation",
        "versioning", "lifecycle"]
if not os.path.isdir(cs):
    errs.append("tests/sdk-conformance/ missing")
else:
    for c in CATS:
        if not os.path.isdir(os.path.join(cs, c)):
            errs.append(f"tests/sdk-conformance/{c}/ missing")
if errs:
    for e in errs:
        print(f"SDK COMPAT: test-vectors: {e}", file=sys.stderr)
    sys.exit(1)
print(f"test-vectors OK ({len(vecs)} vectors, {len(CATS)} conformance dirs)")
PY
then
  fail=1
fi

[ "$fail" -eq 0 ] && echo "SDK COMPAT: workspace/go/python all pinned to $min"
exit "$fail"
