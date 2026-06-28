#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
RELEASE="$ROOT/release"

OS_NAME="$(uname -s)"

case "$OS_NAME" in
  Darwin|Linux)
    # Portable binary only — no bundle packaging.
    ;;
  *)
    echo "Unsupported host OS for $0: $OS_NAME"
    exit 1
    ;;
esac

echo "Building xxterm portable executables..."
echo ""

echo "[1/3] Installing frontend dependencies..."
cd "$ROOT"
npm install
echo "Done."
echo ""

echo "[2/3] Building Tauri application (this may take a while)..."
cd "$ROOT"

npx tauri build --no-bundle
echo "Done."
echo ""

echo "[3/3] Copying binary..."
rm -rf "$RELEASE"
mkdir -p "$RELEASE"

TARGET_DIR="$ROOT/src-tauri/target/release"

if [ -f "$TARGET_DIR/xxterm" ]; then
  cp "$TARGET_DIR/xxterm" "$RELEASE/"
  echo "Copied binary to $RELEASE"
else
  echo "WARNING: Binary not found at $TARGET_DIR/xxterm"
fi

chmod -R a+rX "$RELEASE"

echo ""
echo "=============================================="
echo " Binary saved to: $RELEASE"
ls -la "$RELEASE" 2>/dev/null || true
echo "=============================================="
