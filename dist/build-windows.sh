#!/usr/bin/env bash
# build-windows.sh — release build of `kb` (package `kb-user`) for Windows.
#
# Native branch (run on a Windows / GHA windows-latest host):
#   Uses MSVC (the x86_64-pc-windows-msvc toolchain). No --target is needed
#   — cargo defaults to the host target. The bundled SQLite gets compiled
#   against MSVC's cl.exe / link.exe, which the GHA runner ships out of the
#   box.
#
#   Output: target/x86_64-pc-windows-msvc/release/kb.exe   (explicit --target
#   so the output path is deterministic for the release workflow's normalize
#   step, regardless of cargo's host-target layout choices).
#
# Cross branch (run on a Linux host with mingw-w64 installed):
#   CC=x86_64-w64-mingw32-gcc AR=x86_64-w64-mingw32-ar \
#   CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
#     cargo build --release --target x86_64-pc-windows-gnu -p kb-user
#
#   Output: target/x86_64-pc-windows-gnu/release/kb.exe
#
# Package (optional, on Windows):
#   (cd target/x86_64-pc-windows-msvc/release && 7z a -tzip kb-x86_64-pc-windows.zip kb.exe)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

case "$(uname)" in
  MINGW*|MSYS*|CYGWIN*|Windows_NT)
    echo "==> cargo build --release --target x86_64-pc-windows-msvc -p kb-user (native MSVC)"
    cargo build --release --target x86_64-pc-windows-msvc -p kb-user
    ;;
  Linux)
    echo "==> cargo build --release --target x86_64-pc-windows-gnu -p kb-user (Linux→Windows cross)"
    CC="${CC:-x86_64-w64-mingw32-gcc}" \
    AR="${AR:-x86_64-w64-mingw32-ar}" \
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="${CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER:-x86_64-w64-mingw32-gcc}" \
      cargo build --release --target x86_64-pc-windows-gnu -p kb-user
    ;;
  Darwin)
    echo "build-windows.sh: building Windows on macOS requires mingw + cargo (not supported here)" >&2
    exit 1
    ;;
  *)
    echo "build-windows.sh: unknown host $(uname)" >&2
    exit 1
    ;;
esac

echo
echo "==> done."
