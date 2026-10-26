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
    TARGET=x86_64-pc-windows-msvc
    cargo build --release --target x86_64-pc-windows-msvc -p kb-user
    ;;
  Linux)
    echo "==> cargo build --release --target x86_64-pc-windows-gnu -p kb-user (Linux→Windows cross)"
    # The bundled SQLite (the `-sys` crate) compiles a C translation unit
    # against a *C* cross-compiler, which must already be installed. On
    # Debian/Ubuntu that is a two-package job:
    #   sudo apt-get install -y gcc-mingw-w64-x86-64 binutils-mingw-w64-x86-64
    #   (optionally: mingw-w64-tools   → provides `x86_64-w64-mingw32-wine`
    #    to smoke-run the .exe)
    command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 || {
      echo "build-windows.sh: x86_64-w64-mingw32-gcc not found — install:" >&2
      echo "    sudo apt-get install -y gcc-mingw-w64-x86-64 binutils-mingw-w64-x86-64" >&2
      exit 1
    }
    rustup target list --installed 2>/dev/null | grep -q "x86_64-pc-windows-gnu" || {
      echo "build-windows.sh: rust target x86_64-pc-windows-gnu not installed — install:" >&2
      echo "    rustup target add x86_64-pc-windows-gnu" >&2
      exit 1
    }
    TARGET=x86_64-pc-windows-gnu
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

if [ -z "${TARGET:-}" ]; then
  echo "build-windows.sh: no build target selected (host/branch did not set TARGET)" >&2
  exit 1
fi

# ---- stage ------------------------------------------------------------------
OUT="$ROOT/dist"
DEST="$OUT/kb-x86_64-pc-windows.exe"
mkdir -p "$OUT"
SRC_EXE="$ROOT/target/$TARGET/release/kb.exe"
[ -f "$SRC_EXE" ] || { echo "build-windows.sh: build artifact $SRC_EXE not found" >&2; exit 1; }
cp -f "$SRC_EXE" "$DEST"
if command -v sha256sum >/dev/null 2>&1; then
  ( cd "$OUT" && sha256sum kb-x86_64-pc-windows.exe > kb-x86_64-pc-windows.exe.sha256 )
fi
echo "==> staged $DEST"

# ---- verify -----------------------------------------------------------------
# A Windows PE can only be executed under a Windows interpreter (wine) on
# Linux; the .bat/.ps1 variants run natively on Windows where this just works.
# Smoke-run when we can, otherwise confirm the artifact is a Windows PE.
if command -v wine >/dev/null 2>&1; then
  wine "$DEST" --version
elif command -v x86_64-w64-mingw32-wine >/dev/null 2>&1; then
  x86_64-w64-mingw32-wine "$DEST" --version
else
  file "$DEST" | grep -q "PE32+ executable" || {
    echo "build-windows.sh: staged artifact is not a Windows PE" >&2; exit 1
  }
  echo "==> (no wine on this host — verified $DEST is a Windows PE; run \`wine $DEST --version\` to smoke-test)"
fi
echo
echo "==> done."
