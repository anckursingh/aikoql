#!/usr/bin/env bash
# D-19 §28 — the nightly SDK battery: the LONG arms the PR tier cannot
# afford (testing plan §28: long fuzz + state-machine + fault injection +
# cross-language corpus + pool/stream stress + large payloads). ONE script
# so the sdk-nightly job in benchmark.yml and a local Linux run certify
# identically.
#   Requires: the debug binaries at target/debug (aikoql-mcp + the §18
#   aikoql-fault-proxy — the job builds both), the toolchains on PATH
#   (python3, go, JDK 17 + mvn, node >= 24, rust), and — for the rust
#   engine arm — the nightly toolchain (fuzz/smoke.sh installs it).
#   Linux-shaped like the job it certifies: the python venv lives under
#   /tmp and the rust engine arm is the libFuzzer one.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

BIN="$(realpath "${AIKOQL_MCP_BIN:-$root/target/debug/aikoql-mcp}")"
PROXY="$(realpath "${AIKOQL_FAULT_PROXY:-$root/target/debug/aikoql-fault-proxy}")"
[ -x "$BIN" ] || { echo "sdk-nightly: no server at $BIN — build it first (cargo build -p aikoql-mcp -p aikoql-fault-proxy)" >&2; exit 1; }
[ -x "$PROXY" ] || { echo "sdk-nightly: no fault proxy at $PROXY — build it first" >&2; exit 1; }
export AIKOQL_MCP_BIN="$BIN" AIKOQL_FAULT_PROXY="$PROXY"

say()  { echo "::group::$1"; }
done_() { echo "::endgroup::"; }

# The python legs need the native module built (the estate imports
# MAX_FRAME etc. as a package, the pool/fault tests import the client);
# maturin develop needs VIRTUAL_ENV or it targets the wrong interpreter.
say "toolchain: python venv + native module"
python3 -m venv /tmp/aikoql-nightly-venv
/tmp/aikoql-nightly-venv/bin/pip install -q maturin pytest hypothesis
export VIRTUAL_ENV=/tmp/aikoql-nightly-venv
( cd crates/sdk/python && "$VIRTUAL_ENV/bin/maturin" develop )
done_

# 1. Cross-language corpus — the §16 golden corpus (sdk-fuzz-corpus/
#    corpus.json) through all five SDKs: same malformed input, same
#    classification, idiomatic exception types allowed. Offline wire
#    primitives (the PR contract job's pins, one battery).
say "1/6 cross-language corpus"
( cd crates/sdk/go && go test -run TestCorpusPin . )
( cd crates/sdk/python && "$VIRTUAL_ENV/bin/python" -m pytest tests/test_corpus_pin.py -q )
( cd crates/sdk/typescript && node --test tests/corpus-pin.test.ts )
( cd crates/sdk/java && mvn -B test -Dtest=CorpusPinTest )
cargo test -p aikoql-sdk --test fuzz_corpus_pin
done_

# 2. State machine — the §12 python model (DISCONNECTED→CONNECTED→
#    INITIALIZED→TRANSACTION→STREAMING→CLOSED) and the §17 TS fast-check
#    estate: illegal transitions must error deterministically, never
#    panic/deadlock/leak.
say "2/6 state machine"
( cd crates/sdk/python && "$VIRTUAL_ENV/bin/python" -m pytest tests/test_fuzz_estate.py tests/test_fuzz_estate_pin.py -q )
( cd crates/sdk/typescript && node --test tests/fuzz-estate.test.ts tests/fuzz-estate-pin.test.ts )
done_

# 3. Fault injection + large payloads — the §18 matrix (13 modes) through
#    three SDKs: drop/delay/duplicate/reorder/truncate/corrupt/…; the
#    oversized legs ARE the large-payload arm — a 64 MiB claimed frame is
#    rejected from the header before any buffering (§19 resource safety).
say "3/6 fault injection + large payloads"
( cd crates/sdk/python && "$VIRTUAL_ENV/bin/python" -m pytest tests/test_fault_matrix.py -q )
( cd crates/sdk/typescript && node --test tests/fault-matrix.test.ts )
cargo test -p aikoql-sdk --test fault
done_

# 4. Pool/stream stress — exhaustion, reuse-after-cancel, reconnect,
#    release-resets-txn, min-idle fill, through the three pool suites.
say "4/6 pool/stream stress"
( cd crates/sdk/go && go test -run TestPool . )
( cd crates/sdk/python && "$VIRTUAL_ENV/bin/python" -m pytest tests/test_pool.py -q )
( cd crates/sdk/typescript && node --test tests/pool.test.ts )
done_

# 5. Long fuzz — the native engines with nightly budgets (the PR gate's
#    5-second smokes only prove the harness runs): go 60s per target,
#    jazzer 30s per target.
say "5/6 long fuzz (go + java)"
( cd crates/sdk/go
  # go's -fuzz refuses a multi-target regex — one budget per target.
  for name in $(grep -o '^func Fuzz[A-Za-z0-9]*' fuzz_test.go | awk '{print $2}'); do
    go test -fuzz "^${name}\$" -fuzztime 60s .
  done )
( cd crates/sdk/java && AIKOQL_JAZZER_SECONDS=30 bash jazzer-smoke.sh )
done_

# 6. Rust engine arm — the libFuzzer targets the PR tier cannot run at
#    all (rustc ships libFuzzer only on Linux/macOS); 30s each. The
#    smoke script pins the nightly toolchain by date itself.
say "6/6 rust cargo-fuzz engine arm"
AIKOQL_FUZZ_SECONDS=30 bash crates/sdk/rust/fuzz/smoke.sh
done_

echo "sdk-nightly: every leg green for $(basename "$BIN")"
