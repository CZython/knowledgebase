#!/bin/sh
# Verify the kb-core spec with Verus.
#
# Mirrors git-restricted_version_B/verify.sh: pin z3 4.16.0, put `verus`
# on PATH, verify a *single self-contained spec file* with the standalone
# `verus` binary (NOT `cargo verus` — this fork rejects user-defined types
# declared outside a `verus!` block, so all verified logic lives in one
# spec file with `&str`/`u8` parameters only).
#
# We also run `cargo test` on kb-core so the exec<->spec drift-check tests
# execute (the "gen_spec.py --check" step of the gr reference project).
#
# Usage:
#   ./verify.sh                        # uses default z3 4.16.0
#   VERUS_Z3_PATH=/path/to/z3 ./verify.sh
#
# Prereqs:
#   * verus (ARIA fork, v0.2026.09.06)
#   * z3 4.16.0 (e.g. ~/.local/z3-4.16.0)

set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"

# ---- z3 4.16.0 (required by this Verus build; distro z3 4.8 is rejected) ----
CANDIDATE="${HOME}/.local/z3-4.16.0/z3-4.16.0-x64-glibc-2.39/bin/z3"
Z3="${VERUS_Z3_PATH:-$CANDIDATE}"
if [ ! -x "$Z3" ]; then
  Z3="$(command -v z3 || true)"
fi
[ -n "$Z3" ] && [ -x "$Z3" ] || {
  echo "verify.sh: no z3 binary found (set VERUS_Z3_PATH)" >&2
  exit 1
}
export VERUS_Z3_PATH="$Z3"
case "$("$Z3" --version 2>&1)" in
  *4.16.0*) : ;;
  *) echo "verify.sh: expected z3 4.16.0, got: $("$Z3" --version)" >&2; exit 1 ;;
esac

# ---- verus ----
# Look for Verus in (in order of preference):
#   1. already on PATH (e.g. a system / `cargo install` / `~/.local/bin` verus);
#   2. $VERUS_DIR (user override) — either a dir holding a `verus` binary, or
#      the `verus-x86-linux` install dir (whose `verus` sits one level down);
#   3. two well-known development-machine locations under $HOME (last resort
#      — kept so the original author's box works without any env setup).
have_verus() { command -v verus >/dev/null 2>&1; }
if ! have_verus && [ -n "${VERUS_DIR:-}" ]; then
  [ -x "$VERUS_DIR/verus" ] && export PATH="$VERUS_DIR:$PATH"
  [ -x "$VERUS_DIR" ]       && export PATH="${VERUS_DIR%/}:$PATH"
fi
KNOWN_VERUS_DIRS="$HOME/.local/lib/Verus/verus-x86-linux
$HOME/programming/Rust/Verus/verus-x86-linux"
if ! have_verus; then
  for d in $KNOWN_VERUS_DIRS; do
    [ -x "$d/verus" ] && export PATH="$d:$HOME/.local/bin:$PATH" && break
  done
fi
have_verus || {
  echo "verify.sh: verus not found. Install it, or set VERUS_DIR=<dir> containing a 'verus' binary." >&2
  exit 1
}

# ---- (1) Z3: verify each spec file ----------------------------------------
# One spec file per module, each self-contained: `&str` / `u8` / `bool`
# parameters only (this Verus fork rejects user-defined types declared
# outside a `verus!` block — see `kb-core/src/lib.rs`).
for f in meta_spec.rs fm_spec.rs fm_ops_spec.rs identity_spec.rs \
         tags_spec.rs links_spec.rs graph_spec.rs labels_spec.rs \
          search_grammar_spec.rs semantics_spec.rs fs_snapshot_spec.rs \
          section_spec.rs resolve_spec.rs alias_spec.rs; do
  echo "verify.sh: verifying kb-core/spec/$f with z3"
  verus "$HERE/kb-core/spec/$f"
done

# ---- (2) exec<->spec drift-check: run the unit tests ----------------------
#         (gr calls this "drift-check" — the concrete cases in the spec are
#          re-asserted against the exec code so a future refactor that
#          changes the exec shape is caught even if the spec still stands.)
echo "verify.sh: running kb-core unit tests (exec<->spec drift-check)"
cargo test --manifest-path "$HERE/kb-core/Cargo.toml" -q
