#!/bin/sh
# Full project gate — one command, everything in one place.
#
# Runs the entire verification surface in deterministic order and fails fast
# (set -e) at the first red step:
#
#   1. build        — both binaries (kb-user + kb-hardened) link
#   2. one-package  — the two single-package builds also work (-p selection)
#   3. clippy       — all targets, all workspace members, -D warnings
#   4. tests        — every unit + integration suite, workspace-wide
#   5. verus        — ./verify.sh (each spec file w/ z3 4.16.0 + kb-core tests)
#   6. behavior     — binary identity + hardened gating (the locked reqs)
#   7. dep tree     — hardened has NO ratatui/crossterm; user has them
#
# Usage:
#   ./gate.sh                 # run the full gate
#
# Prereqs: same as verify.sh (verus + z3 4.16.0). Everything else just needs
# a working `cargo`.

set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

pass() { printf '  \033[32m\342\211\244\033[0m %s\n' "$1"; }
run()  { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }

# ---- (1) build: both binaries ---------------------------------------------
run "1/7 build — both binaries (kb-user + kb-hardened)"
cargo build
pass "cargo build"

# ---- (2) one-package builds (HARDENED_BUILD.md req 2 & 3) -----------------
run "2/7 single-package builds (-p selection)"
cargo build -p kb-user
pass "cargo build -p kb-user   -> kb"
cargo build -p kb-hardened
pass "cargo build -p kb-hardened -> kb-hardened"

# ---- (3) clippy: all targets, all members, -D warnings --------------------
run "3/7 clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings
pass "clippy (0 warnings, 4/4 crates)"

# ---- (4) tests: every suite, workspace-wide --------------------------------
run "4/7 tests — cargo test --workspace"
cargo test --workspace
pass "cargo test --workspace"

# ---- (5) verus: each spec file w/ z3 4.16.0 + kb-core tests ---------------
run "5/7 verus — ./verify.sh (spec + z3 4.16.0)"
./verify.sh
pass "verify.sh (spec verified, 0 errors)"

# ---- (6) behavior: identity + hardened gating (locked reqs 4-7) -----------
run "6/7 behavior — binary identity + hardened gating"
USER_BIN="$HERE/target/debug/kb"
HARD_BIN="$HERE/target/debug/kb-hardened"

[ "$($USER_BIN --version)"  = "kb 0.0.0" ]        || { echo "kb --version mismatch"; exit 1; }
pass "kb --version  == 'kb 0.0.0'"
[ "$($HARD_BIN --version)"  = "kb-hardened 0.0.0 [hardened]" ] || { echo "kb-hardened --version mismatch"; exit 1; }
pass "kb-hardened --version == 'kb-hardened 0.0.0 [hardened]'"

# the `kb` binary (package `kb-user`) must still show both gated verbs (req 7)
$USER_BIN --help 2>&1 | grep -qE '^\s+tui'      || { echo "kb missing 'tui'"; exit 1; }
pass "kb --help shows 'tui'"
$USER_BIN --help 2>&1 | grep -qE '^\s+section'  || { echo "kb missing 'section' (edit path)"; exit 1; }
pass "kb --help shows 'section'"

# kb-hardened must reject the gated verbs with clap's 'unrecognized subcommand' (req 4)
msg_tui=$($HARD_BIN tui 2>&1 | head -1); rc_tui=0; $HARD_BIN tui >/dev/null 2>&1 || rc_tui=$?
[ "$msg_tui" = "error: unrecognized subcommand 'tui'" ] || { echo "kb-hardened tui: wrong message ($msg_tui)"; exit 1; }
[ "$rc_tui" = "2" ] || { echo "kb-hardened tui: expected rc 2, got $rc_tui"; exit 1; }
pass "kb-hardened tui   -> 'error: unrecognized subcommand', rc 2"

msg_edit=$($HARD_BIN edit x 2>&1 | head -1); rc_edit=0; $HARD_BIN edit x >/dev/null 2>&1 || rc_edit=$?
[ "$msg_edit" = "error: unrecognized subcommand 'edit'" ] || { echo "kb-hardened edit: wrong message ($msg_edit)"; exit 1; }
[ "$rc_edit" = "2" ] || { echo "kb-hardened edit: expected rc 2, got $rc_edit"; exit 1; }
pass "kb-hardened edit  -> 'error: unrecognized subcommand', rc 2"

# ---- (7) dep tree: hardened strips ratatui/crossterm (req 8) ---------------
run "7/7 dep tree — hardened smaller (no ratatui/crossterm)"
if cargo tree -p kb-hardened 2>&1 | grep -qiE 'ratatui|crossterm'; then
  echo "hardened still links ratatui/crossterm" >&2; exit 1;
fi
pass "hardened: ratatui + crossterm absent"
if ! cargo tree -p kb-user 2>&1 | grep -qiE 'ratatui|crossterm'; then
  echo "user is missing ratatui/crossterm (should have them)" >&2; exit 1;
fi
pass "user: ratatui + crossterm present"

# ---- done ------------------------------------------------------------------
printf '\n\033[32m\033[1mGATE PASSED\033[0m — all 7 stages green (build, one-pkg,\nclippy, tests, verus, behavior, dep-tree)\n'
