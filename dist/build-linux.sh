#!/usr/bin/env bash
# build-linux.sh — static musl build of `kb`, the Linux distribution artifact.
#
# Produces a single self-contained file that runs on ANY distro (including
# Alpine, minimal containers, BusyBox) — no FUSE required (unlike AppImage),
# no shared libs, no system SQLite.
#
# Prerequisites (Ubuntu/Debian; equivalent packages elsewhere):
#   * a Rust toolchain (any recent stable; see `rust-toolchain.toml`)
#   * the `x86_64-unknown-linux-musl` rust target
#       rustup target add x86_64-unknown-linux-musl
#   * musl-tools (Ubuntu/Debian: `sudo apt-get install musl-tools`)
#     which provides `x86_64-linux-musl-gcc` (the musl C compiler the bundled
#     SQLite is built against).
#
# Usage:
#   ./dist/build-linux.sh                 # build + tar + sha256
#   OUT=/some/dir ./dist/build-linux.sh   # where to place the artifacts
#
# Output (in $OUT, default `dist/`):
#   kb-x86_64-linux-musl                  # the self-contained binary
#   kb-x86_64-linux-musl.tar.zst          # compressed (tar + zstd); install
#                                         #   with: tar -xf … && mv kb-x86_64-linux-musl /usr/local/bin/
#   kb-x86_64-linux-musl.sha256           # integrity check

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TARGET="x86_64-unknown-linux-musl"   # rust target
BIN="kb"                             # the binary name (package is "kb-user")
SUFFIX="x86_64-linux-musl"           # marketing name for the artifact suffix

# ---- prereq check ---------------------------------------------------------
# musl-tools ships the C compiler as `x86_64-linux-musl-gcc` (note: NOT the
# rust-target spelling). Allow an override via MUSL_GCC.
MUSL_GCC="${MUSL_GCC:-x86_64-linux-musl-gcc}"
command -v "$MUSL_GCC" >/dev/null 2>&1 || {
  echo "build-linux.sh: $MUSL_GCC not found — install musl-tools:" >&2
  echo "    sudo apt-get install musl-tools" >&2
  exit 1
}

# ---- build ----------------------------------------------------------------
echo "==> cargo build --release --target $TARGET -p kb-user (bin: $BIN)"
CC="$MUSL_GCC" \
AR="${AR:-gcc-ar}" \
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$MUSL_GCC" \
RUSTFLAGS="-C target-feature=+crt-static" \
  cargo build --release --target "$TARGET" -p kb-user

# ---- package --------------------------------------------------------------
OUT="${OUT:-$ROOT/dist}"
mkdir -p "$OUT"
DEST="$OUT/kb-$SUFFIX"
cp "target/$TARGET/release/$BIN" "$DEST"
strip "$DEST" 2>/dev/null || echo "(note: strip unavailable; binary left unstripped)"

# tar.zst if zstd is available; tar.gz otherwise (still self-extracting).
if command -v zstd >/dev/null 2>&1; then
  echo "==> tar + zstd -> kb-$SUFFIX.tar.zst"
  ( cd "$OUT" && tar -cI zstd -f "kb-$SUFFIX.tar.zst" "kb-$SUFFIX" )
else
  echo "==> tar + gzip -> kb-$SUFFIX.tar.gz (zstd not on PATH; install it for a smaller artifact)"
  ( cd "$OUT" && tar czf "kb-$SUFFIX.tar.gz" "kb-$SUFFIX" )
fi

# ---- checksum -------------------------------------------------------------
( cd "$OUT" && \
  sha256sum "kb-$SUFFIX" > "kb-$SUFFIX.sha256" && \
  cat "kb-$SUFFIX.sha256" )

# ---- verify ----------------------------------------------------------------
echo "==> verify (static link + smoke run):"
ldd "$DEST" 2>&1 | grep -qi "not a dynamic executable\|statically linked" \
  && echo "   OK: statically linked"
"$DEST" --version | sed 's/^/   /'

echo
echo "==> done. Artifact(s) in: $OUT"
