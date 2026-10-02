#!/usr/bin/env bash
# D-17 (launch plan §29) — mutation harness for the five SDK wire
# primitives. The review's rule: a surviving mutation is a missing test.
# Each mutation applies ONE surgical damage to a detached-worktree copy of
# HEAD and runs its named killer — that SDK's §16 corpus pin (D-16), the
# frozen cross-language classification spec. Exit propagation:
#
#   non-zero = the mutant IS killed (that exit is the RED the archive
#              captures via scripts/red-archive.sh)
#   zero     = SURVIVED — the estate has a hole, not a success
#
#   sdk-mutation-harness.sh <id>   apply one mutation, run its killer
#   sdk-mutation-harness.sh all    run all; exit 0 iff all are killed
#
# The twelve §29 mutations target the frozen wire truths the corpus pins:
# parse_version per SDK, the error-code normalization, the §3.3 correlation
# split, and the notify stream filter. m-k06 (TS) and m-k09 (Py) remove the
# stream_id clause wherever it lives — the loop line at the RED, the
# extracted verdict primitive after the GREEN — so the same mutation code
# holds across the arc.
#
# The Python killer sets PYTHONPATH to the worktree's source: the pin
# imports aikoql bare, which resolves to the INSTALLED package in normal
# runs (D-16's green pin tested site-packages, not the repo) — a repo
# drift would otherwise never redden the pin.
#
# CARGO_TARGET_DIR is shared with the main tree so dependency artifacts
# are reused — CONSEQUENCE: the mutant's aikoql-sdk artifacts land in the
# shared target dir and a later LIVE-tree cargo run can reuse them. After
# any harness run, `cargo clean -p aikoql-sdk` before live verification.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

mutations() {
  echo "m-k01-go-version m-k02-go-error m-k03-go-classify \
m-k04-ts-version m-k05-ts-error m-k06-ts-notify \
m-k07-py-version m-k08-py-error m-k09-py-notify \
m-k10-java-version m-k11-java-error m-k12-rust-version"
}

# pymut <file> <python-code> — the same string-surgery mechanism as the
# storage harness (scripts/storage-mutation-harness.sh); its PYTHONUTF8
# and backslash notes apply here identically. A pymut crash is a harness
# error (exit 3), never a kill or a survivor.
pymut() {
  f="$1"
  code="$2"
  PYTHONUTF8=1 python - "$f" <<PY
import sys
f = sys.argv[1]
s = open(f, encoding="utf-8").read()
orig = s
$code
assert s != orig, "mutation did not apply — anchor moved or drifted"
open(f, "w", encoding="utf-8", newline="\n").write(s)
PY
  [ $? -eq 0 ] || { echo "pymut failed for $id" >&2; exit 3; }
}

mutate() {
  id="$1"
  tmp="$(mktemp -d)"
  tree="$tmp/tree"
  trap 'git worktree remove --force "$tree" 2>/dev/null || true; rm -rf "$tmp"' EXIT
  git worktree add --detach "$tree" HEAD >/dev/null 2>&1
  case "$id" in
    m-k01-go-version)
      # the Atoi failure arm returns 0 instead of -1 — a refused segment
      # reads as version 0, never "older"
      pymut "$tree/crates/sdk/go/aikoql.go" '
s = s.replace("""parts = append(parts, -1)""", """parts = append(parts, 0)""", 1)'
      (cd "$tree/crates/sdk/go" && go test -run TestCorpusPin . 2>&1 | tail -15)
      ;;
    m-k02-go-error)
      # the empty-code INTERNAL default drops — "" reaches the caller
      pymut "$tree/crates/sdk/go/aikoql.go" '
s = s.replace("""code = "INTERNAL\"""", """code = code""", 1)'
      (cd "$tree/crates/sdk/go" && go test -run TestCorpusPin . 2>&1 | tail -15)
      ;;
    m-k03-go-classify)
      # a larger id skips instead of PROTOCOL_ERROR — a foreign response
      # is swallowed as noise
      pymut "$tree/crates/sdk/go/aikoql.go" '
s = s.replace("""return corrProtocol""", """return corrSkip""", 1)'
      (cd "$tree/crates/sdk/go" && go test -run TestCorpusPin . 2>&1 | tail -15)
      ;;
    m-k04-ts-version)
      # the numeric-segment regex drops — Number("") reads 0 and
      # Number("0x10") reads 16 where the regex refused
      pymut "$tree/crates/sdk/typescript/src/client.ts" '
s = s.replace(r"""return /^[+-]?\d+$/.test(s) ? Number(s) : -1;""", """return Number(s);""", 1)'
      (cd "$tree/crates/sdk/typescript" && node --test tests/corpus-pin.test.ts 2>&1 | tail -15)
      ;;
    m-k05-ts-error)
      # the null-code normalization drops — String(null) reads "null"
      # where the frozen verdict is INTERNAL
      pymut "$tree/crates/sdk/typescript/src/client.ts" '
s = s.replace("""e.code === undefined || e.code === null ? "" : String(e.code)""", """e.code === undefined ? "" : String(e.code)""", 1)'
      (cd "$tree/crates/sdk/typescript" && node --test tests/corpus-pin.test.ts 2>&1 | tail -15)
      ;;
    m-k06-ts-notify)
      # the stream filter drops — a foreign stream_id notify is yielded
      pymut "$tree/crates/sdk/typescript/src/client.ts" '
s = s.replace("|| p.stream_id !== streamId", "", 1)'
      (cd "$tree/crates/sdk/typescript" && node --test tests/corpus-pin.test.ts 2>&1 | tail -15)
      ;;
    m-k07-py-version)
      # int(seg) gains the auto-base — int("0x10", 0) reads 16 where
      # base-10 refuses (the frozen uniform refusal)
      pymut "$tree/crates/sdk/python/python/aikoql/mcp_client.py" '
s = s.replace("""parts.append(int(seg))""", """parts.append(int(seg, 0))""", 1)'
      (cd "$tree/crates/sdk/python" && PYTHONPATH="$tree/crates/sdk/python/python" python -m pytest tests/test_corpus_pin.py -q 2>&1 | tail -15)
      ;;
    m-k08-py-error)
      # the missing-code INTERNAL default drops — "" reaches the caller
      pymut "$tree/crates/sdk/python/python/aikoql/mcp_client.py" '
s = s.replace("""            code=err.get("code", "INTERNAL"),
            message=err.get("message", "unknown error"),""", """            code=err.get("code", ""),
            message=err.get("message", "unknown error"),""", 1)'
      (cd "$tree/crates/sdk/python" && PYTHONPATH="$tree/crates/sdk/python/python" python -m pytest tests/test_corpus_pin.py -q 2>&1 | tail -15)
      ;;
    m-k09-py-notify)
      # the stream filter drops — a foreign stream_id notify is yielded
      pymut "$tree/crates/sdk/python/python/aikoql/mcp_client.py" '
s = s.replace("""            if p.get("stream_id") != stream_id:
                continue
""", "", 1)'
      (cd "$tree/crates/sdk/python" && PYTHONPATH="$tree/crates/sdk/python/python" python -m pytest tests/test_corpus_pin.py -q 2>&1 | tail -15)
      ;;
    m-k10-java-version)
      # the parse failure arm returns 0 instead of -1
      pymut "$tree/crates/sdk/java/src/main/java/io/aikoql/client/Connection.java" '
s = s.replace("""out[i] = -1;""", """out[i] = 0;""", 1)'
      (cd "$tree/crates/sdk/java" && mvn -Dtest=CorpusPinTest test 2>&1 | tail -15)
      ;;
    m-k11-java-error)
      # the empty-code INTERNAL default drops — "" reaches the caller
      pymut "$tree/crates/sdk/java/src/main/java/io/aikoql/client/Connection.java" '
s = s.replace("""code.isEmpty() ? "INTERNAL" : code""", """code""", 1)'
      (cd "$tree/crates/sdk/java" && mvn -Dtest=CorpusPinTest test 2>&1 | tail -15)
      ;;
    m-k12-rust-version)
      # the parse failure arm returns 0 instead of -1
      pymut "$tree/crates/sdk/rust/src/fuzz.rs" '
s = s.replace(""".map(|seg| seg.parse::<i64>().unwrap_or(-1))""", """.map(|seg| seg.parse::<i64>().unwrap_or(0))""", 1)'
      (cd "$tree/crates/sdk/rust" && CARGO_TARGET_DIR="$root/target" cargo test --test fuzz_corpus_pin 2>&1 | tail -15)
      ;;
    *)
      echo "unknown mutation: $id" >&2
      exit 2
      ;;
  esac
}

case "${1:-}" in
  all)
    fail=0
    for m in $(mutations); do
      if mutate "$m"; then
        echo "SURVIVED: $m" >&2
        fail=1
      else
        echo "killed: $m"
      fi
    done
    if [ $fail -ne 0 ]; then
      echo "sdk mutation harness: a mutant survived its killer" >&2
      exit 1
    fi
    echo "sdk mutation harness: all twelve killed"
    ;;
  "")
    echo "usage: sdk-mutation-harness.sh <id>|all" >&2
    exit 2
    ;;
  *)
    mutate "$1"
    ;;
esac
