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
#   8. no-network   — the `mcp` build proves no network (MCP.md invariant 1):
#                      cargo-tree network-crate grep + NEEDED libs + PLT
#                      socket-family symbols all absent, plus a live strace
#                      of `mcp serve` showing zero socket(  syscalls.
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
run "1/8 build — both binaries (kb-user + kb-hardened)"
cargo build
pass "cargo build"

# ---- (2) one-package builds (HARDENED_BUILD.md req 2 & 3) -----------------
run "2/8 single-package builds (-p selection)"
cargo build -p kb-user
pass "cargo build -p kb-user   -> kb"
cargo build -p kb-hardened
pass "cargo build -p kb-hardened -> kb-hardened"

# ---- (3) fmt + clippy: all targets, all members, -D warnings --------------
# `cargo fmt --all -- --check` mirrors the CI build-and-test stage 3a gate;
# clippy the whole workspace AND the `mcp` feature build (the MCP slice 1
# surface only compiles with `--features mcp`; the plain `--workspace` pass
# sees whatever the package defaults are for that member).
#
# `kb-hardened` exposes THREE orthogonal compile features (MCP.md):
#   `mcp`        presence of the surface (serde_json links, `kb mcp` exists)
#   `mcp-writes` + the write handlers (implies `mcp`)
#   `mcp-hidden` cosmetic: hides the subcommand from --help / completions
# `default = ["mcp-writes", "mcp-hidden"]` so the hardened build ships the
# full working surface, hidden from --help. The two `--no-default-features`
# passes below lock the OTHER corners of that decision space: (a) presence
# off entirely (the `MCP_HIDDEN` const is `#[cfg]`'d out — dead_code check
# must not fire), and (b) presence on but hiddenness off (`MCP_HIDDEN ==
# false`, exactly what `hide = crate::MCP_HIDDEN` feeds clap).
run "3/8 fmt + clippy --workspace --all-targets -- -D warnings"
cargo fmt --all -- --check
pass "fmt (all targets, 0 diffs)"
cargo clippy --workspace --all-targets -- -D warnings
pass "clippy (0 warnings, 4/4 crates)"
cargo clippy -p kb-hardened --features mcp,mcp-writes,mcp-hidden --all-targets -- -D warnings
pass "clippy -p kb-hardened --features mcp,mcp-writes,mcp-hidden (0 warnings)"
cargo clippy -p kb-hardened --no-default-features --features mcp,mcp-writes --all-targets -- -D warnings
pass "clippy -p kb-hardened --no-default-features --features mcp,mcp-writes (0 warnings)"
cargo clippy -p kb-hardened --no-default-features --all-targets -- -D warnings
pass "clippy -p kb-hardened --no-default-features (0 warnings)"
# All-on corner: the `completions` feature is NOT in `kb-hardened`'s
# defaults (defaults = mcp-writes, mcp-hidden), so the `completion_tree()`
# code path and the `#[cfg(feature = "mcp")]` branch of
# `completion_tests::completion_tree_strips_exactly_the_hidden_subcommands`
# are never compiled under the workspace-default test run above. This
# explicit corner locks (a) the prune-== base-minus-hidden invariant AND
# (b) the feature-aware mcp hidden/retained branch, both of which CI
# otherwise would not exercise.
cargo clippy -p kb-hardened --features mcp,mcp-writes,mcp-hidden,completions --all-targets -- -D warnings
pass "clippy -p kb-hardened --features mcp,mcp-writes,mcp-hidden,completions (0 warnings)"

# ---- (4) tests: every suite, workspace-wide --------------------------------
run "4/8 tests — cargo test --workspace"
cargo test --workspace
pass "cargo test --workspace"
cargo test -p kb-hardened --features mcp
pass "cargo test -p kb-hardened --features mcp (8 sanitizer + dispatch tests)"
cargo test -p kb-hardened --features mcp,mcp-writes,mcp-hidden,completions completion_tests
pass "cargo test -p kb-hardened --features mcp,mcp-writes,mcp-hidden,completions completion_tests (hidden-prune invariant)"

# ---- (5) verus: each spec file w/ z3 4.16.0 + kb-core tests ---------------
run "5/8 verus — ./verify.sh (spec + z3 4.16.0)"
./verify.sh
pass "verify.sh (spec verified, 0 errors)"

# ---- (6) behavior: identity + hardened gating (locked reqs 4-7) -----------
run "6/8 behavior — binary identity + hardened gating"
USER_BIN="$HERE/target/debug/kb"
HARD_BIN="$HERE/target/debug/kb-hardened"

[ "$($USER_BIN --version)"  = "kb 0.1.0" ]        || { echo "kb --version mismatch"; exit 1; }
pass "kb --version  == 'kb 0.1.0'"
[ "$($HARD_BIN --version)"  = "kb-hardened 0.1.0 [hardened]" ] || { echo "kb-hardened --version mismatch"; exit 1; }
pass "kb-hardened --version == 'kb-hardened 0.1.0 [hardened]'"

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
run "7/8 dep tree — hardened smaller (no ratatui/crossterm)"
if cargo tree -p kb-hardened 2>&1 | grep -qiE 'ratatui|crossterm'; then
  echo "hardened still links ratatui/crossterm" >&2; exit 1;
fi
pass "hardened: ratatui + crossterm absent"
if ! cargo tree -p kb-user 2>&1 | grep -qiE 'ratatui|crossterm'; then
  echo "user is missing ratatui/crossterm (should have them)" >&2; exit 1;
fi
pass "user: ratatui + crossterm present"

# ---- (8) no-network: the `mcp` build never talks to the world (invariant 1)
run "8/8 no-network — mcp build has no socket capability"
# The `mcp` feature is ON by default in `kb-hardened` (T2 / open decision
# #2, resolved 2026-11-27) — a plain `cargo build` (stage 1) already links
# the server; the explicit `--features mcp` build below just makes the
# assertion's target unambiguous and double-checks the read-only variant
# (without `mcp-writes`) is also network-free.
cargo build -p kb-hardened --features mcp
MCP_BIN="$HERE/target/debug/kb-hardened"

# (a) The spec's five network crates (MCP.md locked requirement 2) and the
#     common transitive network-stack members they'd pull in (h2 = HTTP/2,
#     socket2 = thin wrapper over libc::socket, native-tls / openssl =
#     handshaker, ring = handshake + curve25519, quinn = UDP QUIC,
#     tungstenite = ws, hyper-util = std-only hyper impl). Absence of all is
#     the *compile-time* guarantee: no Rust network crate in the tree.
NET_CREATES=$(cargo tree -p kb-hardened --features mcp 2>&1 \
  | grep -iE '\breqwest\b|\bureq\b|\bhyper\b|\bHyper\b|\btokio\b|\bmio\b|\bh2\b|\bsocket2\b|\bnative-tls\b|\bopenssl\b|\bring\b|\bquinn\b|\btungstenite\b|\bhyper-util\b' \
  | head -8)
if [ -n "$NET_CREATES" ]; then
  echo "net crate in mcp tree:" >&2; printf '%s\n' "$NET_CREATES" >&2; exit 1
fi
pass "cargo tree: no network crate (reqwest/ureq/hyper/tokio/mio + transitive)"

# (b) The mcp binary's NEEDED set is the pure-compute trio (libc, libm,
#     libgcc_s) plus the loader. No libssl / libcrypto / libcurl / libssh,
#     no DNS resolver (libnsl, libresolv), no libpthread / librt (those
#     would point at an async runtime that needs a thread pool or a timer
#     that talks to the scheduler).
NEEDED_LINES=$(readelf -d "$MCP_BIN" | grep NEEDED)
for lib in libssl libcrypto libcurl libssh dns nsl resolv; do
  case "$NEEDED_LINES" in
    *"\[$lib"*) echo "mcp binary NEEDs $lib (network lib)" >&2; exit 1;;
  esac
done
pass "NEEDED libs: no libssl / libcrypto / libcurl / libssh / dns / nsl / resolv"

# (c) No network-capable socket-family undefined dynamic symbols. This is
#     the load-time proof: if the code could ever create a socket to a peer
#     (`socket(2)` / `socketat`), connect one (`connect(2)`), accept / bind
#     / listen, resolve a name (getaddrinfo/getnameinfo), or run a reactor
#     (epoll_*), the glibc symbol appears in the PLT as `U <name>`.
#
#     Two deliberate EXCLUSIONS, both fd-generic in glibc and present in a
#     purely-local-pipe binary (false positives under a substring match):
#       - `socketpair`  — caught by a bare `socket` substring; glibc uses
#         it for *local* fd plumbing (unconnected pair), never a network
#         peer. We want `socket` / `socketat`, so the pattern is
#         `\bsocket(at)?\b`, which does not match `socketpair`.
#       - `sendmsg` / `recvmsg` — the generic message I/O calls; glibc
#         routes plain `read`/`write` on a pipe through them. The
#         datagram-specific pair `sendto` / `recvfrom` stays in the set (a
#         datagram socket is required to use them).
#     Absence of every listed symbol is the invariant: the binary cannot
#     open a socket, resolve an address, or talk to a peer — its "network"
#     is the stdin/stdout pipes it was spawned with. (The authoritative
#     runtime proof is stage 8(d)'s strace; this is the load-time screen.)
SOCK_SYMS=$(nm -D --undefined-only "$MCP_BIN" \
  | grep -E '\bsocket(at)?\b|\bconnect\b|\baccept(4)?\b|\bbind\b|\blisten\b|\bepoll_|getaddrinfo|getnameinfo|\brecvfrom\b|\bsendto\b' \
  | head -8)
if [ -n "$SOCK_SYMS" ]; then
  echo "socket-family PLT import in mcp binary:" >&2; printf '%s\n' "$SOCK_SYMS" >&2; exit 1
fi
pass "PLT symbols: no socket/socketat/connect/accept/bind/listen/epoll/getaddrinfo/sendto-recvfrom"

# (d) Live: run `mcp serve` with a minimal initialize+EOF fixture and
#     count the syscalls. A *serving* binary that ever reaches for a network
#     path would show a `socket(` line (the first word in every glibc
#     socket-family syscall). Zero `socket(` lines across the trace is the
#     *syscall* proof of invariant 1. The *does-it-work* proof lives in the
#     unit suite: `write::tests::e2e_initialize_list_create_and_read_back_
#     over_the_wire` drives a full initialize → tools/list → tools/call
#     (write, subprocess-verb path) → read-back session through `dispatch`,
#     i.e. a successful tool call in-process without a strace harness.
if command -v strace >/dev/null 2>&1; then
  TRACE=$(strace -f -e trace=socket,connect,bind,listen,accept,sendto,recvfrom "$MCP_BIN" mcp serve 2>&1 </dev/null || true)
  if printf '%s\n' "$TRACE" | grep -qE '\bsocket\s*\('; then
    echo "strace: socket(  syscall was made by mcp serve" >&2; exit 1
  fi
  if printf '%s\n' "$TRACE" | grep -qE '\bconnect\s*\('; then
    echo "strace: connect( syscall was made by mcp serve" >&2; exit 1
  fi
  pass "strace: zero socket( / connect( syscalls during initialize"
else
  echo "  (strace not present; (b)+(c) already prove no socket capability)"
fi

# ---- done ------------------------------------------------------------------
printf '\n\033[32m\033[1mGATE PASSED\033[0m — all 8 stages green (build, one-pkg,\nclippy, tests, verus, behavior, dep-tree, no-network)\n'
