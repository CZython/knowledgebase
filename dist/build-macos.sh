#!/usr/bin/env bash
# build-macos.sh — release build of `kb` (package `kb-user`) *and*
# `kb-hardened` (package `kb-hardened`) for macOS.
#
# Run this *on a macOS machine* (or a macOS CI runner; cross-compiling the C
# dependencies to Apple from Windows is not a supported path). On macOS the
# default toolchain works out of the box — `cargo build` compiles the C
# build-dependencies (e.g. `zstd` via Turso's Tantivy) with Apple's clang.
#
# Output (both binaries, for the host arch):
#   target/<arch>-apple-darwin/release/kb
#   target/<arch>-apple-darwin/release/kb-hardened
# For Apple Silicon on an Intel host (or vice-versa), use
# `cargo build --target aarch64-apple-darwin` / `x86_64-apple-darwin`.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

case "$(uname)" in
  Darwin)
    ARCH="$(uname -m)"
    case "$ARCH" in
      arm64)  TARGET="aarch64-apple-darwin" ;;
      x86_64) TARGET="x86_64-apple-darwin"  ;;
      *) echo "build-macos.sh: unknown arch $ARCH" >&2; exit 1 ;;
    esac
    for PKG in kb-user kb-hardened; do
      echo "==> cargo build --release --target $TARGET -p $PKG (native macOS/$ARCH)"
      cargo build --release --target "$TARGET" -p "$PKG"
    done
    ;;
  Linux)
    echo "build-macos.sh: macOS target requires running on macOS (native Apple toolchain)" >&2
    echo "                (cross from Linux needs the Apple SDK — not supported here)" >&2
    exit 1
    ;;
  MINGW*|MSYS*|CYGWIN*)
    echo "build-macos.sh: macOS target requires macOS" >&2
    exit 1
    ;;
  esac

echo
echo "==> done. Binaries: target/${TARGET}/release/kb (package kb-user)"
echo "                    target/${TARGET}/release/kb-hardened (package kb-hardened)"
