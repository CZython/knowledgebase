#!/usr/bin/env bash
# build-linux.sh — static musl build of `kb` (package `kb-user`) *and*
# `kb-hardened` (package `kb-hardened`), the Linux distribution artifacts.
#
# Produces self-contained files that run on ANY distro (including Alpine,
# minimal containers, BusyBox) — no FUSE required (unlike AppImage), no
# shared libs, no system SQLite. Both artifacts share one DB
# (~/.local/share/knowledgebase/kb.db), so `kb`-created notes are searchable
# with `kb-hardened` out of the box.
#
# Prerequisites (Ubuntu/Debian; equivalent packages elsewhere):
#   * a Rust toolchain (any recent stable; see `rust-toolchain.toml`)
#   * the `x86_64-unknown-linux-musl` rust target
#       rustup target add x86_64-unknown-linux-musl
#   * musl-tools (Ubuntu/Debian: `sudo apt-get install musl-tools`)
#     which provides `x86_64-linux-musl-gcc` (a musl C compiler; the build
#     needs one to compile its C dependencies, e.g. `zstd` via Turso's
#     Tantivy — there is no system SQLite to link against).
#
# Usage:
#   ./dist/build-linux.sh                 # build + tar + sha256
#   OUT=/some/dir ./dist/build-linux.sh   # where to place the artifacts
#
# Output (in $OUT, default `dist/`): ONE set per binary — the self-contained
# binary, a .tar.zst (or .tar.gz if no zstd) archive, and a .sha256:
#   kb-x86_64-linux-musl                   # the full build (TUI + editor)
#   kb-hardened-x86_64-linux-musl          # the headless agent build
#   kb-x86_64-linux-musl.tar.zst           #   compressed (tar + zstd)
#   kb-hardened-x86_64-linux-musl.tar.zst
#   kb-x86_64-linux-musl.sha256            #   integrity check
#   kb-hardened-x86_64-linux-musl.sha256
# Install: tar -xf kb-x86_64-linux-musl.tar.zst && mv kb-x86_64-linux-musl /usr/local/bin/kb

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

# ---- build + package + checksum + verify, for both binaries ---------------
# Each entry is `package:binaryName` (the package to `-p` vs the binary it
# emits). Both land in the same target dir with the same RUSTFLAGS, so a
# single cargo invocation per package is enough.
OUT="${OUT:-$ROOT/dist}"
mkdir -p "$OUT"

for entry in kb-user:kb kb-hardened:kb-hardened; do
  PKG="${entry%%:*}"
  BIN="${entry##*:}"
  ART="$BIN-$SUFFIX"
  DEST="$OUT/$ART"

  echo "==> cargo build --release --target $TARGET -p $PKG (bin: $BIN)"
  CC="$MUSL_GCC" \
  AR="${AR:-gcc-ar}" \
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$MUSL_GCC" \
  RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --target "$TARGET" -p "$PKG"

  cp "target/$TARGET/release/$BIN" "$DEST"
  strip "$DEST" 2>/dev/null || echo "(note: strip unavailable; binary left unstripped)"

  # tar.zst if zstd is available; tar.gz otherwise (still self-extracting).
  if command -v zstd >/dev/null 2>&1; then
    ( cd "$OUT" && tar -cI zstd -f "$ART.tar.zst" "$ART" )
  else
    ( cd "$OUT" && tar czf "$ART.tar.gz" "$ART" )
  fi

  ( cd "$OUT" && sha256sum "$ART" > "$ART.sha256" )

  echo "==> verify $ART (static link + smoke run):"
  ldd "$DEST" 2>&1 | grep -qi "not a dynamic executable\|statically linked" \
    && echo "   OK: statically linked"
  "$DEST" --version | sed 's/^/   /'
  echo
done

echo "==> done. Artifacts in: $OUT"
