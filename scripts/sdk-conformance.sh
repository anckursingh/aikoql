#!/usr/bin/env bash
# D-11 shared conformance runner (§7, §23): ONE dispatcher for every SDK.
# Each language adapter spawns its own real server, runs the frozen
# protocol/test-vectors + tests/sdk-conformance/ vectors through its SDK,
# and asserts the identical expected results. Pure dispatch — no server
# management here.
#
#   scripts/sdk-conformance.sh --language {python,go,rust,typescript,java} [--bin PATH] \
#       [--vectors DIR] [--protocol DIR] [--token TOKEN]
#
# Path handling: MSYS2 skips arg conversion for python.exe (Python is
# special-cased), so the python arm cds to the repo root and hands
# cwd-relative paths; the go arm hands bash-form absolute paths, which
# MSYS2 converts for go.exe. AIKOQL_MCP_BIN/--bin pass through verbatim.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LANGUAGE=""
BIN="${AIKOQL_MCP_BIN:-}"
VECTORS=""
PROTOCOL=""
TRANSPORT="mcp" # D-15: --transport native rides the framed binary protocol

while [[ $# -gt 0 ]]; do
  case "$1" in
    --language)  LANGUAGE="$2"; shift 2 ;;
    --bin)       BIN="$2"; shift 2 ;;
    --vectors)   VECTORS="$2"; shift 2 ;;
    --protocol)  PROTOCOL="$2"; shift 2 ;;
    --token)     TOKEN="$2"; shift 2 ;;
    --transport) TRANSPORT="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

case "$LANGUAGE" in
  python|go|rust|typescript|java) ;;
  "") echo "usage: $0 --language {python,go,rust,typescript,java}" >&2; exit 1 ;;
  *) echo "unknown language: $LANGUAGE" >&2; exit 1 ;;
esac

case "$TRANSPORT" in
  mcp|native) ;;
  *) echo "unknown transport: $TRANSPORT (mcp|native)" >&2; exit 1 ;;
esac
if [[ "$TRANSPORT" == "native" && "$LANGUAGE" != "rust" ]]; then
  echo "native transport: the rust arm only (D-15 — the protocol lands in the reference SDK first)" >&2
  exit 1
fi

if [[ -z "$BIN" ]]; then
  # Default: relative to the repo root — each arm resolves it against the
  # cwd it sets for its adapter.
  if [[ -x "$ROOT/target/debug/aikoql-mcp" ]]; then
    BIN="target/debug/aikoql-mcp"
  elif [[ -x "$ROOT/target/debug/aikoql-mcp.exe" ]]; then
    BIN="target/debug/aikoql-mcp.exe"
  else
    echo "sdk-conformance: no server binary — build it or pass --bin" >&2
    exit 1
  fi
fi

VECTORS="${VECTORS:-tests/sdk-conformance}"
PROTOCOL="${PROTOCOL:-protocol/test-vectors}"
TOKEN="${TOKEN:-conformance}"

case "$LANGUAGE" in
  python)
    cd "$ROOT"
    if [[ -x "$ROOT/.venv/Scripts/python.exe" ]]; then
      PY=".venv/Scripts/python.exe"
    elif [[ -x "$ROOT/.venv/bin/python" ]]; then
      PY=".venv/bin/python"
    else
      PY="python3"
    fi
    exec "$PY" crates/sdk/python/tests/sdk_conformance.py \
      --bin "$BIN" --vectors "$VECTORS" --protocol "$PROTOCOL" --token "$TOKEN" ;;
  go)
    cd "$ROOT/crates/sdk/go"
    GBIN="$BIN"
    VECTORS_G="$VECTORS"
    PROTOCOL_G="$PROTOCOL"
    # Defaults are ROOT-relative; the adapter resolves vs its own cwd
    # (crates/sdk/go), so re-anchor them to the repo root.
    [[ "$GBIN" = /* || "$GBIN" = [A-Za-z]:* ]] || GBIN="$ROOT/$GBIN"
    [[ "$VECTORS_G" = /* || "$VECTORS_G" = [A-Za-z]:* ]] || VECTORS_G="$ROOT/$VECTORS_G"
    [[ "$PROTOCOL_G" = /* || "$PROTOCOL_G" = [A-Za-z]:* ]] || PROTOCOL_G="$ROOT/$PROTOCOL_G"
    exec go run ./cmd/sdk-conformance \
      -bin "$GBIN" -vectors "$VECTORS_G" -protocol "$PROTOCOL_G" -token "$TOKEN" ;;
  rust)
    cd "$ROOT/crates/sdk/rust"
    RBIN="$BIN"
    VECTORS_R="$VECTORS"
    PROTOCOL_R="$PROTOCOL"
    # Defaults are ROOT-relative; the adapter resolves vs its own cwd
    # (crates/sdk/rust), so re-anchor them to the repo root.
    [[ "$RBIN" = /* || "$RBIN" = [A-Za-z]:* ]] || RBIN="$ROOT/$RBIN"
    [[ "$VECTORS_R" = /* || "$VECTORS_R" = [A-Za-z]:* ]] || VECTORS_R="$ROOT/$VECTORS_R"
    [[ "$PROTOCOL_R" = /* || "$PROTOCOL_R" = [A-Za-z]:* ]] || PROTOCOL_R="$ROOT/$PROTOCOL_R"
    NATIVE_ARGS=()
    [[ "$TRANSPORT" == "native" ]] && NATIVE_ARGS=(--native)
    exec cargo run --quiet --bin sdk-conformance -- \
      --bin "$RBIN" --vectors "$VECTORS_R" --protocol "$PROTOCOL_R" --token "$TOKEN" \
      "${NATIVE_ARGS[@]}" ;;
  typescript)
    cd "$ROOT/crates/sdk/typescript"
    TBIN="$BIN"
    VECTORS_T="$VECTORS"
    PROTOCOL_T="$PROTOCOL"
    # Defaults are ROOT-relative; the adapter resolves vs its own cwd
    # (crates/sdk/typescript), so re-anchor them to the repo root. MSYS2
    # converts args for node.exe (like go.exe), so bash-form paths are fine.
    [[ "$TBIN" = /* || "$TBIN" = [A-Za-z]:* ]] || TBIN="$ROOT/$TBIN"
    [[ "$VECTORS_T" = /* || "$VECTORS_T" = [A-Za-z]:* ]] || VECTORS_T="$ROOT/$VECTORS_T"
    [[ "$PROTOCOL_T" = /* || "$PROTOCOL_T" = [A-Za-z]:* ]] || PROTOCOL_T="$ROOT/$PROTOCOL_T"
    exec node src/sdk-conformance.ts \
      --bin "$TBIN" --vectors "$VECTORS_T" --protocol "$PROTOCOL_T" --token "$TOKEN" ;;
  java)
    cd "$ROOT/crates/sdk/java"
    JBIN="$BIN"
    VECTORS_J="$VECTORS"
    PROTOCOL_J="$PROTOCOL"
    # Defaults are ROOT-relative; the adapter resolves vs its own cwd
    # (crates/sdk/java), so re-anchor them to the repo root. MSYS2 converts
    # args for java.exe (like node.exe), so bash-form paths are fine.
    [[ "$JBIN" = /* || "$JBIN" = [A-Za-z]:* ]] || JBIN="$ROOT/$JBIN"
    [[ "$VECTORS_J" = /* || "$VECTORS_J" = [A-Za-z]:* ]] || VECTORS_J="$ROOT/$VECTORS_J"
    [[ "$PROTOCOL_J" = /* || "$PROTOCOL_J" = [A-Za-z]:* ]] || PROTOCOL_J="$ROOT/$PROTOCOL_J"
    mvn -q -DskipTests compile
    exec java -cp target/classes io.aikoql.client.SdkConformance \
      --bin "$JBIN" --vectors "$VECTORS_J" --protocol "$PROTOCOL_J" --token "$TOKEN" ;;
esac
