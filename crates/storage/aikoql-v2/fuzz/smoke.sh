#!/usr/bin/env bash
# The FZ-01..07 engine arm — Linux/macOS CI only (rustc ships the libFuzzer
# engine nowhere else; the default-build sweep in tests/fuzz_estate.rs
# covers the other platforms). Each target runs for AIKOQL_FUZZ_SECONDS
# (default 5) with no seed corpus: every check embeds its own valid
# inputs, so the engine reaches the deep paths from the first run.
set -euo pipefail
cd "$(dirname "$0")"

case "$(uname -s)" in
  Linux|Darwin) ;;
  *)
    echo "the libFuzzer engine is not available on $(uname -s) — the default-build sweep covers this platform" >&2
    exit 0
    ;;
esac

NIGHTLY="${AIKOQL_FUZZ_NIGHTLY:-nightly-2026-09-27}"
RUN_SECONDS="${AIKOQL_FUZZ_SECONDS:-5}"

rustup toolchain install "$NIGHTLY" --profile minimal >/dev/null 2>&1 || true
rustup component add rust-src --toolchain "$NIGHTLY" >/dev/null 2>&1 || true
command -v cargo-fuzz >/dev/null 2>&1 || cargo install cargo-fuzz --locked

for target in fuzz_current fuzz_manifest fuzz_wal_frame fuzz_wal_replay \
              fuzz_checkpoint fuzz_directories fuzz_envelope_snapshot; do
  echo "== $target =="
  cargo "+$NIGHTLY" fuzz run "$target" -- -max_total_time="$RUN_SECONDS"
done

echo "all seven FZ targets survived"
