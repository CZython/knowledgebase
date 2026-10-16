#!/usr/bin/env bash
# build-macos.sh — release build of `kb` (package `kb-user`) for macOS.
#
# Run this *on a macOS machine* (or a macOS CI runner; cross-compiling SQLite
# + Rust to Apple from Windows is not a supported path). On macOS the default
# toolchain works out of the box — `cargo build` compiles the bundled SQLite
# with Apple's clang.
#
# Output: target/<arch>-apple-darwin/release/kb   (a Mach-O binary for the
# host arch; for Apple Silicon on an Intel host, or vice-versa, use
# `cargo build --target aarch64-apple-darwin` / `x86_64-apple-darwin`).

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
    echo "==> cargo build --release --target $TARGET -p kb-user (native macOS/$ARCH)"
    cargo build --release --target "$TARGET" -p kb-user
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
echo "==> done. Binary: target/${TARGET}/release/kb-user"
