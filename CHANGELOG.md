# Changelog

All notable changes to `kb` are recorded here. Dates are first-release dates.
This project follows [Semantic Versioning](https://semver.org/). Until `1.0`,
the API is still moving; breaking changes are called out explicitly.

## [Unreleased]

_Nothing yet._ (Add entries here as you work; promote to a numbered section
when cutting the next tag.)

## [0.1.0] — 2026-10-17

First open-source release.

Everything below shipped in this milestone, built and verified end-to-end
(`gate.sh` 7/7: build, single-package build, `clippy -D warnings`, the full
unit + integration + TUI test suites, the **Verus** spec pass, binary
identity, and the dependency-tree proof that the hardened build links no TUI
crates).

### Added
- **A single-binary Markdown knowledge base** over embedded SQLite + FTS5:
  index a tree of notes, then `search` (FTS with `type:`/`tag:` filters, `~`
  OR, `…` phrase), `browse`, `toc`/`show` (scoped section reads + etags), and
  section-level writes (`section {set|append|insert|replace}`), whole-substring
  `replace`, and `patch` (apply an external `diff -u`). 22 verbs total, every
  one with `--help`, and every write carrying the shared contract
  (`--dry-run`, `--yes`, `--json`, `--if-unchanged <etag>`).
- **Two build variants from one shared source body** (`kb/main.rs`, included
  into thin packages):
  - **`kb`** (package `kb-user`) — the full CLI: interactive TUI
    (ratatui/crossterm) + `$VISUAL`/`$EDITOR` editor flow + every verb.
  - **`kb-hardened`** (package `kb-hardened`) — the headless, agent-facing
    build: TUI + editor compiled out (`default = []`), no ratatui / crossterm /
    `unicode-width` in the dependency tree; `tui` / `edit` are rejected by clap
    with `unrecognized subcommand` (exit 2).
- **A Verus-verified core** (`kb-core`): 14 per-module specs
  (frontmatter, frontmatter ops, identity, tags, aliases, links, graph,
  labels, search grammar, semantics, fs-snapshot, section, resolve, meta)
  discharged against the ARIA Verus fork + z3 4.16.0. Zero std I/O, zero
  external dependencies.
- **Audit + security model** (`SECURITY_REPORT.md`): 17 findings
  (3.1–3.17) — each reproduced, mitigated, and re-checked. Highlight: the
  HIGH RCE via `$VISUAL`/`$EDITOR` fork (`kb section edit`) is removed at
  compile time in the hardened build, and the editor verbs are feature-gated
  in both.
- **Install + distribution** paths for open-source use:
  - `cargo install --path kb-user` → puts `kb` on `$PATH`.
  - Per-platform static/native artifacts: `dist/build-linux.sh` (static
    musl ELF — one file, no libs, runs in any distro/container),
    `dist/build-windows.sh`, `dist/build-macos.sh`.
  - `.github/workflows/ci.yml` (build, one-package builds, `clippy -D
    warnings`, full tests, dep-tree proof, plus a per-PR static-musl build)
    and `.github/workflows/release.yml` (4-runner matrix → GitHub Release
    assets: linux-musl, windows-gnu, macos-aarch64, macos-x86_64).
  - `[profile.release]` = `lto="fat"`, `codegen-units=1`, `panic="abort"`.
- **Provenance:** `tools/mdtool.py` (the Python prototype
  `kb-core::section` was transcribed from) is kept in-tree with a STATUS
  header; `Cargo.lock` is checked in for reproducible `cargo install` and
  release artifacts.

### Changed
- Workspace package names → **`kb-user`** / **`kb-hardened`** (the binary a
  person types is **`kb`**; the hardening variant is **`kb-hardened`** —
  same package-vs-binary split as `fd-find`→`fd`, `ripgrep`→`rg`).
- Host-agnostic toolchain pin (`rust-toolchain.toml`): a concrete version
  (`1.98.1`) rather than a Linux triple, so `cargo build` works on
  Linux / macOS / Windows alike.
- Public CLI metadata (`kb --version`, `--help`) for both builds; each
  package carries a distinct `description` + `rust-version` (1.98.1) so
  crates.io / docs.rs state the right thing.

### Not yet
- Publishing `kb-user` / `kb-hardened` (and `kb-core` / `kb-db`) to
  crates.io — deferred until the workspace's path-dep graph is reworked
  for publishing.

<!-- Fill in the repo base URL once a remote is added, e.g.
     https://github.com/<owner>/knowledgebase/compare/v0.1.0 -->
[0.1.0]: compare/v0.1.0
