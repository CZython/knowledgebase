#!/usr/bin/env bash
# build-windows.sh — release build of `kb` (package `kb-user`) for Windows.
#
# Run this *on a Windows machine* (or a Windows CI runner). Cross-compiling
# from Linux is possible but needs a mingw-w64 cross-toolchain for the
# bundled SQLite, so the canonical path is a native Windows build. On a
# Windows-hosted CI runner, the default `cargo` toolchain works out of the
# box (Windows ships a C compiler: `clang` on GitHub runners, or MSVC via
# `rustup`).
#
# If you're crossing from a *Linux* box with mingw-w64 installed, use this
# instead:
#
#   CC=x86_64-w64-mingw32-gcc AR=x86_64-w64-mingw32-ar \
#   CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
#     cargo build --release --target x86_64-pc-windows-gnu -p kb-user
#
# Output: target/x86_64-pc-windows-gnu/release/kb.exe
#
# Package (optional, on Windows):
#   (cd target/x86_64-pc-windows-gnu/release && 7z a -tzip kb-x86_64-pc-windows.zip kb.exe)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

case "$(uname)" in
  MINGW*|MSYS*|CYGWIN*|Windows_NT)
    echo "==> cargo build --release -p kb-user (native Windows)"
    cargo build --release -p kb-user
    ;;
  Linux)
    echo "==> cargo build --release --target x86_64-pc-windows-gnu -p kb-user (Linux→Windows cross)"
    CC="${CC:-x86_64-w64-mingw32-gcc}" \
    AR="${AR:-x86_64-w64-mingw32-ar}" \
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="${CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER:-x86_64-w64-mingw32-gcc}" \
      cargo build --release --target x86_64-pc-windows-gnu -p kb-user
    ;;
  Darwin)
    echo "==> building Windows on macOS requires mingw + cargo (see README)" >&2
    exit 1
    ;;
esac

echo
echo "==> done."
