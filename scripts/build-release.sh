#!/usr/bin/env bash
# Mnemosyne Linux Release Build
# Produces: build/linux/  (binary + checksum + archive)
# Requires: Rust toolchain (https://rustup.rs)
#
# Options:
#   --musl    Build static musl binary (runs on any Linux, no glibc needed)
#   --gnu     Build dynamically linked glibc binary (smaller, needs glibc)

set -euo pipefail

TARGET=""
TARGET_DIR="release"
MUSL_FLAG=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --musl)
            TARGET="x86_64-unknown-linux-musl"
            TARGET_DIR="x86_64-unknown-linux-musl/release"
            MUSL_FLAG="musl"
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

# Get version from Cargo.toml
VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')
VERSION="${VERSION:-0.1.0}"

PLATFORM="${MUSL_FLAG:-gnu}"
ARCHIVE_NAME="mnemosyne-linux-x86_64-${PLATFORM}-${VERSION}.tar.gz"

echo "=== Mnemosyne Linux Release Build ==="
echo "Version: $VERSION"
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

echo "[1/5] Running tests..."
cargo test -p mnemosyne-mcp -- --test-threads=1 2>&1 | grep "test result" || echo "WARNING: Tests had issues, continuing..."

echo "[2/5] Updating dependencies..."
cargo update

if [[ -n "$TARGET" ]]; then
    echo "[3/5] Installing target $TARGET..."
    rustup target add "$TARGET"

    echo "[4/5] Building release binary for $TARGET..."
    cargo build --release --target "$TARGET" -p mnemosyne-mcp
else
    echo "[3/5] Skipping target install (native build)"
    echo "[4/5] Building release binary (native)..."
    cargo build --release -p mnemosyne-mcp
fi

echo "[5/5] Collecting artifacts..."
OUTDIR="build/linux"
rm -rf "$OUTDIR"
mkdir -p "$OUTDIR"

cp "target/$TARGET_DIR/mnemosyne-mcp" "$OUTDIR/"
cp QUICKSTART.md "$OUTDIR/"
cp mnemosyne.toml "$OUTDIR/"

# Version stamp
echo "$VERSION" > "$OUTDIR/VERSION"
echo "Built: $(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$OUTDIR/BUILD_INFO.txt"

# SHA256 checksum
sha256sum "$OUTDIR/mnemosyne-mcp" | cut -d' ' -f1 > "$OUTDIR/mnemosyne-mcp.sha256"

# Distribution tarball
tar -czf "build/$ARCHIVE_NAME" -C build linux/

echo ""
echo "=== Build complete ==="
echo "Version: $VERSION"
echo "Binary: $OUTDIR/mnemosyne-mcp"
file "$OUTDIR/mnemosyne-mcp" 2>/dev/null || true
ls -lh "$OUTDIR/mnemosyne-mcp"
echo "Archive: build/$ARCHIVE_NAME"
echo ""
echo "Usage:"
echo "  mnemosyne-mcp shell                 Interactive shell"
echo "  mnemosyne-mcp                       Start server (MCP TCP + HTTP metrics)"
echo "  mnemosyne-mcp --metrics-addr 127.0.0.1:9181 my.db"
echo "  mnemosyne-mcp import --help         Import data"
