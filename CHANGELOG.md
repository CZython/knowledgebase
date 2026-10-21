# Changelog

All notable changes to `kb` are recorded here. Dates are first-release dates.
This project follows [Semantic Versioning](https://semver.org/). Until `1.0`,
the API is still moving; breaking changes are called out explicitly.

## [Unreleased]

### Added
- **TUI "edit" is now a deliberate, zsh-completion-style three-stage flow
  (renamed to `edit metadata`).** Opening the metadata editor from the
  browse/read view now lands on a *field picker* first (a deliberate step, not
  implicit): select one of `title` / `description` / `tags` / `search_terms`,
  each showing a *preview* of what's on disk right now (so a field is never a
  mystery before you edit it), then `Enter` opens that field's editor (scalars
  pre-filled for an in-place edit; lists empty, with a live token menu), and
  `Tab` toggles to the completion menu — type to filter, `↑`/`↓` to choose, and
  Tab back fills the highlight into the buffer exactly like zsh. `Esc` pops one
  stage (Completions → Editor → Picker → browse).
- **`v` key in the browse/read views opens the selected page in `$EDITOR`**
  (`$VISUAL` → `$EDITOR` → `vi`), reusing the existing `edit in place` path —
  in-editor flow from the TUI without leaving it. Gated on the `editor`
  feature (absent in the hardened build).

### Changed
- **`edit in place` (`-e`) / `v` failures now tell you *why*.** A failed
  *launch* (no editor found) and a non-zero *exit* are reported distinctly, and
  the launch case names the env var tried (`$VISUAL`/`$EDITOR`) and that the
  fallback `vi` isn't on `$PATH`, pointing at the fix instead of a bare
  `launch …: No such file`. The file is left untouched either way.
- **Agent contract — `--json` now holds on the failure path too.** An
- **Agent contract — `--json` now holds on the failure path too.** An
  `--if-unchanged <etag>` mismatch (any of `section set/append/insert/replace`,
  `patch`, `replace`, or the whole-frontmatter ops) previously exited `3` with
  only a human line on **stderr** and an **empty** stdout. Under `--json` the
  contract promises "stdout is one clean JSON object" on every path, and that
  promise now actually holds: a mismatch emits
  `{"ok": false, "applied": false, "op": "etag_check", "scope": <ref|`*`>,
  "code": 3, "error": …, "etag_expected": …, "etag_actual": …}` on stdout (the
  human message still goes to stderr) and leaves the file untouched. Agents
  doing `serde_json::from_str(&stdout)` no longer get a silent empty string on
  the retry path. See the README's agent-contract section.

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
- `kb-core` and `kb-db` are publish-ready (the intra-project path deps now
  carry explicit versions and the binary packages are marked
  `publish = false`, so only library crates attempt an index hit).
  `kb-user` / `kb-hardened` are intentionally *not* published — they are
  binary crates, installed via `cargo install --path kb-user` from a source
  checkout, or via the prebuilt GitHub Release artifacts. `cargo add
  kb-user` is not the intended install path for a binary crate.

<!-- Fill in the repo base URL once a remote is added, e.g.
     https://github.com/<owner>/knowledgebase/compare/v0.1.0 -->
[0.1.0]: compare/v0.1.0
