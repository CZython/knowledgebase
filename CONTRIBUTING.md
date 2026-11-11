# Contributing to `kb`

Thanks for helping. Read `PLAN.md` (design + command reference) and
`HARDENED_BUILD.md` (the two-build architecture and why) before doing
anything non-trivial — they are the source of truth for intent.

## Toolchain

- **Rust 1.98.1** — `rust-toolchain.toml` enforces this; it resolves to
  `1.98.1-<host-triple>` so it works on Linux, macOS, and Windows.
- **MSVC toolchain** is not required; a plain `gcc`/`clang` is enough.
- For the **static Linux artifact** (`dist/build-linux.sh`): the `x86_64-unknown-linux-musl`
  target + a musl linker.
- For **Verus** verification (`verify.sh`): the ARIA `verus` fork and `z3`
  4.16.0 on your `PATH`. If you only edit non-`kb-core` code you can run the
  other six gate stages and skip this one.

## Build + test (the loop)

```sh
./gate.sh        # one command: build, one-pkg builds, clippy -D warnings,
                 # all unit/integration/TUI tests, Verus, binary identity,
                 # and the dep-tree proof (hardened links NO TUI crates).
```

Stage by stage, if you need them separately:

 ```sh
 cargo build                          # both kb-user and kb-hardened
 cargo user                           # alias → cargo build -p kb-user
 cargo hardened                       # alias → cargo build -p kb-hardened
 cargo fmt --all                      # format (no --check; CI runs --check)
 cargo clippy --workspace --all-targets -- -D warnings
 cargo test --workspace               # 409 tests
 ./verify.sh                          # Verus + z3 pass over kb-core/spec
 ```
 
 Format is part of the gate — CI fails on `cargo fmt --check`. Run
 `cargo fmt --all` before committing; it reorders `use` blocks, wraps long
 `vec![…]` literals, and normalises whitespace.

### Feature flags

`kb-user` defaults to `["editor", "tui"]`; `kb-hardened` defaults to `[]`.
To build the hardened variant *with* TUI, or the user build *without* the
editor, pass flags through to cargo:

```sh
cargo build -p kb-user --no-default-features          # no editor, no TUI
cargo build -p kb-hardened --features tui             # hardened + TUI
```

## The shared body — important gotcha

`kb-user/src/main.rs` and `kb-hardened/src/main.rs` are each a one-line
`include!("../../kb/main.rs")`. **All CLI logic lives in `kb/main.rs`.**
There is no `kb/` package; it is a plain directory that both binaries share.

- Add a new subcommand or flag in `kb/main.rs` once; both binaries pick it up.
- Anything you want to gate behind a feature (`tui`, `editor`) is done in
  `kb/main.rs` with `#[cfg(feature = "...")]`.
- `kb-core` (the Verus-verified core) is a real standalone crate. Add specs
  in `kb-core/spec/<module>_spec.rs` and implement in
  `kb-core/src/<module>.rs`. Keep it dependency-free and std-I/O-free — that
  is what makes it verifiable.

## Layout

| Path                    | Role                                                    |
|-------------------------|---------------------------------------------------------|
| `kb-core/`              | Pure, Verus-verified core (frontmatter, identity, FTS grammar, etc.) |
| `kb-db/`                | Turso + FTS shell around `kb-core`                      |
| `kb/main.rs`            | Shared CLI body (~3 267 lines) — clap, TUI, `$EDITOR`, section ops |
| `kb-user/`              | Thin package → binary `kb` (TUI + editor + every verb)  |
| `kb-hardened/`          | Thin package → binary `kb-hardened` (no TUI, no editor) |
| `dist/`                 | Per-platform build scripts                              |
| `.github/workflows/`    | CI (per-PR) + release (4-platform matrix)               |
| `tools/mdtool.py`       | The Python prototype this was transcribed from (do not remove) |

## Style

- `.editorconfig` pins editor-level defaults (UTF-8, LF, 4-space indent,
  trimmed trailing whitespace, final newline). Most editors honour it out of
  the box; no install step needed.
- No new dependencies without a reason in the PR description. The hardened
  build exists specifically to keep the dependency tree small; pull in a TUI
  / editor / network crate and you defeat the point.
- Clippy with `-D warnings` is the bar — CI fails otherwise.
- `kb` is a CLI, not a library. Public API surface is the set of subcommands
  in `kb --help`. Change a verb and update the CHANGELOG + the
  agent-contract section of `README.md` in the same PR.
- Tests: integration suites live in `kb-user/tests/`; they exercise the real
  `kb` binary (`CARGO_BIN_EXE_kb`). Add a regression test for every bug fix,
  the same way the `SECURITY_REPORT.md` findings were each pinned to a test.

## Pull requests

1. Small, single-purpose. "Fix tag case-sensitivity" beats "refactor search +
   fix tags + add export".
2. Run `./gate.sh` to green before requesting review. If you couldn't run
   the Verus stage, say so in the PR body.
3. Update `CHANGELOG.md` under `## [Unreleased]` with what changed and why.
4. If you touch the CLI verb surface, update:
   - `kb --help` (it comes from `clap` — usually free),
   - `README.md` quickstart + agent contract,
   - `PLAN.md` §13 command reference.

## Reporting bugs / requesting features

Use the issue tracker (once the repo has one). For the classic `kb`
failure mode — "I ran this command and the file changed in a way I didn't
expect" — include:

- `kb --version` and `kb --help` output for the verb used,
- the exact command line,
- `kb section show <path>` before/after,
- for TUI bugs: the terminal you used (not "macos").

## License

By contributing you accept that your contributions are licensed under the
project's [MIT license](LICENSE).
