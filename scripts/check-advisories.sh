#!/usr/bin/env bash
# RustSec advisories: `cargo audit` on every lockfile the repo ships
# (the workspace and the SDK fuzz crate, which has its own). Any
# vulnerability fails, except those listed in .cargo/audit.toml — each with
# the reason it cannot reach aikoql and the trigger that removes it.
# Unmaintained-crate notices are reported, not fatal.
#
# Locally cargo-audit is optional (install: cargo install cargo-audit
# --locked); in CI it is required.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
if ! cargo audit --version >/dev/null 2>&1; then
  if [ -n "${CI:-}" ]; then
    echo "ADVISORIES: cargo-audit is not installed in CI" >&2
    exit 1
  fi
  echo "advisories: cargo-audit not installed — skipped (cargo install cargo-audit --locked)"
  exit 0
fi
fail=0
for dir in "$root" "$root/crates/sdk/rust/fuzz"; do
  # one ignore list for every lockfile
  if ! (cd "$dir" && cargo audit -q -c never --file Cargo.lock \
        $(sed -n 's/^ *"\(RUSTSEC-[0-9]*-[0-9]*\)".*/--ignore \1/p' "$root/.cargo/audit.toml" 2>/dev/null)); then
    rel="${dir#"$root"}"; rel="${rel#/}"
    echo "ADVISORIES: vulnerabilities in ${rel:+$rel/}Cargo.lock (see above)" >&2
    fail=1
  fi
done
[ $fail -eq 0 ] || exit 1
echo "advisories (workspace + fuzz lockfiles) — OK"
