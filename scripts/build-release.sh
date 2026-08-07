#!/usr/bin/env bash
# Mnemosyne Linux Release Build
# Produces: build/linux/mnemosyne-mcp (static binary)
# Requires: Rust toolchain (https://rustup.rs)
#
# Options:
#   --musl    Build static musl binary (runs on any Linux, no glibc needed)
#   --gnu     Build dynamically linked glibc binary (smaller, needs glibc)

set -euo pipefail

TARGET=""
TARGET_DIR="release"
LINKER_CONFIG=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --musl)
            TARGET="x86_64-unknown-linux-musl"
            TARGET_DIR="x86_64-unknown-linux-musl/release"
            shift
            ;;
        --gnu)
            TARGET="x86_64-unknown-linux-gnu"
            TARGET_DIR="x86_64-unknown-linux-gnu/release"
            shift
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--musl|--gnu]"
            exit 1
            ;;
    esac
done

echo "=== Mnemosyne Linux Release Build ==="
if [[ -n "$TARGET" ]]; then
    echo "Target: $TARGET"
else
    echo "Target: native"
fi
echo ""

# Verify Rust is installed
if ! command -v cargo &>/dev/null; then
    echo "ERROR: Rust/Cargo not found. Install from https://rustup.rs"
    exit 1
fi

echo "[1/4] Updating dependencies..."
cargo update

if [[ -n "$TARGET" ]]; then
    echo "[2/4] Installing target $TARGET..."
    rustup target add "$TARGET"

    echo "[3/4] Building release binary for $TARGET..."
    cargo build --release --target "$TARGET" -p mnemosyne-mcp
else
    echo "[2/4] Skipping target install (native build)"
    echo "[3/4] Building release binary (native)..."
    cargo build --release -p mnemosyne-mcp
fi

echo "[4/4] Collecting artifacts..."
OUTDIR="build/linux"
mkdir -p "$OUTDIR"

cp "target/$TARGET_DIR/mnemosyne-mcp" "$OUTDIR/"
cp QUICKSTART.md "$OUTDIR/"
cp mnemosyne.toml "$OUTDIR/"

echo ""
echo "=== Build complete ==="
echo "Binary: $OUTDIR/mnemosyne-mcp"
file "$OUTDIR/mnemosyne-mcp" 2>/dev/null || true
ls -lh "$OUTDIR/mnemosyne-mcp"
echo ""
echo "Run: $OUTDIR/mnemosyne-mcp shell"
echo "Or:  $OUTDIR/mnemosyne-mcp serve --listen 127.0.0.1:9090"
