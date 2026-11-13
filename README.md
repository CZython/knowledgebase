# kb — a single-binary Markdown knowledge base over Turso + FTS

One Rust binary that indexes a tree of Markdown notes into an embedded
Turso (SQLite-compatible) / FTS database, then lets you **search**, **browse**, and **edit**
them — by section, by substring, or by applying external `diff -u` /
`git diff` patches. The core logic (identity, section splice, search
grammar, alias/tag semantics) lives in a **Verus-verified, zero-dependency
pure-Rust crate**; the Turso shell is a thin, audited boundary.

There is no server, no daemon, no external binary to keep in step — it is
one self-contained binary (statically linked on Linux; native Mach-O /
`.exe` on macOS / Windows) that reads Markdown and writes a Turso database file.

---

## Two builds, same code

The repo ships two variants, compiled from **the same source body**
(`kb/main.rs`) into two thin packages whose only real difference is a
feature flag (`tui` / `editor`):

| Package | Binary | TUI | Editor | Intended use |
|---------|--------|-----|--------|--------------|
| `kb-user` | `kb` | yes | yes | people: interactive TUI, in-editor flow, every verb |
| `kb-hardened` | `kb-hardened` | no | no | agents / CI: pipe-able, deterministic, no TUI surface |

The hardened build has **no ratatui, no crossterm, no `unicode-width`** in
its dependency tree (see `HARDENED_BUILD.md`), and clap rejects `tui` /
`edit` subcommands outright with `unrecognized subcommand` (exit 2).

---

## Install

### From this repository (source)

```sh
git clone <url> && cd knowledgebase

# the full build (TUI + in-editor flow + every verb) → `kb`
cargo install --path kb-user
# → installs `~/.cargo/bin/kb`
kb --version

# the headless agent build (same verbs, TUI + editor compiled out) → `kb-hardened`
cargo install --path kb-hardened
# → installs `~/.cargo/bin/kb-hardened`
kb-hardened --version
```

Both share one binary body (`kb/main.rs`) and one default DB
(`~/.local/share/knowledgebase/kb.db`) — so you can build a note with
`kb` and search it with `kb-hardened` (or in CI) with no extra setup.

Or, from the repo root, just run `cargo build` and grab
`target/debug/kb` and `target/debug/kb-hardened` (for release binaries,
`cargo build --release`).

### Prebuilt binaries (Releases)

Every tagged release (`v*`) publishes a native artifact per platform —
**both the full `kb` and the headless `kb-hardened`** — built *natively* by
CI so the fully pure-Rust build is exercised on each OS:

| Platform      | Artifact                          | Notes                                        |
|---------------|-----------------------------------|----------------------------------------------|
| Linux (all)   | `kb-x86_64-linux-musl`        | **static**, runs on any distro / container — no FUSE, no libs |
| Linux (all)   | `kb-hardened-x86_64-linux-musl`| same static build; TUI + editor compiled out  |
| Windows       | `kb-x86_64-pc-windows.exe`    | native, no runtime deps                       |
| Windows       | `kb-hardened-x86_64-pc-windows.exe` | native, headless                       |
| macOS (ARM)   | `kb-aarch64-apple-darwin`     | Apple Silicon                                 |
| macOS (ARM)   | `kb-hardened-aarch64-apple-darwin` | headless                          |
| macOS (Intel) | `kb-x86_64-apple-darwin`      | pre-Apple-Silicon                             |
| macOS (Intel) | `kb-hardened-x86_64-apple-darwin`| headless                                  |

Download from the [Releases](../../releases) tab and put it on your
`$PATH`. The Linux `kb` one is a single self-contained ELF:

```sh
chmod +x kb-x86_64-linux-musl
./kb-x86_64-linux-musl search "rust async"
```

The `kb-hardened-*` artifact is the same engine with no TUI/editor surface —
use it in agents and CI:

```sh
./kb-hardened-x86_64-linux-musl search "rust async"
./kb-hardened-x86_64-linux-musl tui   # → error: unrecognized subcommand (exit 2)
```

To build a platform artifact yourself, run the matching script (each
detects the host OS): `dist/build-linux.sh`, `dist/build-macos.sh`, and for
Windows either a stock-Windows entry point (`dist/build-windows.bat` /
`dist/build-windows.ps1`, the `.ps1` adding an `-Install` switch) or the
cross build from Linux/macOS (`dist/build-windows.sh`).

### Prerequisites

A working Rust toolchain (`stable`) and a C compiler are the build-time
requirements. The database is [Turso](https://turso.tech), a pure-Rust
SQLite-compatible engine, so **no system SQLite is needed** — but the build
pulls in C dependencies (notably `zstd` via Turso's Tantivy), which are
compiled with a C compiler (the platform toolchain ships one by default).

---

## Quickstart

```sh
# 1. Create the DB in the default location (or pass --db PATH).
kb --help

# 2. Scaffold a first note (the <NAME> is used for both filename and H1).
kb create "first-note" --type note

# 3. Or register a whole tree of notes.
kb add /path/to/notes/

# 4. Search (with filters).
kb search "rust async"                     # plain
kb search "type:note rust"                 # filter by type
kb search "tag:design -legacy"             # filter by tag, exclude
kb search "~foo ~bar"                       # OR
kb search "\"exact phrase\""               # phrase

# 5. Inspect one page.
kb toc notes.md                            # outline only (agent-friendly)
kb show notes.md 3.2 --body                # body of section 3.2 (up to first subsection)
kb show notes.md 3.2 --full                # body + all subsections
kb show notes.md 3.2 --etag                # FNV-1a 64 etag (16 hex chars)
kb info /path/to/note.md                   # the DB record

# 6. Edit by section — the write surface.
kb section --file notes.md set      3.2 --text "new body"
kb section --file notes.md append   3.2 --text "more"
kb section --file notes.md replace  3.2 --text "replacement"
kb replace notes.md --find x --new y       # substring across the file
kb patch notes.md --patch-file diff.patch  # external diff -u applied

# Every write op supports: --dry-run, --json, --yes (skip the prompt),
# --if-unchanged <etag> (FNV-1a 64 over the target section's bytes).

# 7. Agent / automation surface.
kb search "rust async" --json           # one clean JSON document
kb cron --json                          # paste-ready cron line
kb add /root --json
kb tag-alias --add "rust async" --canonical "async"

# 8. TUI (user build only).
 kb tui                          # browse / read / edit, ratatui-powered (colorful default)
kb tui --palette colorblind     # red→magenta, green→cyan (no red/green pair)
kb tui --palette ~/.kb_config/default.toml   # a custom TOML file (every role must be present)
kb tui --list-palettes          # swatch table of the shipped palettes
```

Every verb has `--help`; every write is idempotent under a matching
`--if-unchanged`, and `--json` suppresses the interactive diff prompt so
stdout is **one clean JSON object — on success and on failure alike** (an
etag mismatch, for instance, still emits a `{"ok": false, …}` document, so
`json.loads` never hits an empty stream).

---

## Agent contract (locked)

The contract is locked below — one-paragraph version:

* **Read** is scoped + minimal: `toc <file>` (outline only);
  `show <file> <ref> [--body|--full] [--json] [--etag]`.
* **Write** takes the *complete new text* (never a patch as input):
  `section --file <file> {set|append|insert|replace} <ref> --text / --from-file / --stdin`.
  `patch --patch-file | --patch-stdin | --patch-text` is the explicit
  exception: it *takes* a patch (as file / stdin / string) and *applies* it —
  the same write-path contract as the section-level ones.
* **Diff is output for review, never input**, except via the explicit
  `patch` verb.
* **Concurrency**: `--if-unchanged <etag>` = FNV-1a 64-bit hex over the
  target section's bytes at the time of read; mismatch → exit code **3** and,
  under `--json`, a `{"ok": false, "applied": false, "op": "etag_check",
  "code": 3, "scope": <ref|"*">, "error": …, "etag_expected": …,
  "etag_actual": …}` document on stdout (the human message still goes to
  stderr); the file is left untouched.
* **Exit codes**: `0` ok · `1` generic · `2` section/resolve ·
   `3` etag · `2` (also) unrecognized subcommand in the hardened build.
* **JSON envelope**: every `--json` response is exactly one well-formed
  document on stdout (an **array** for the list verbs — `roots`, `browse`,
  `tag`, `search`, `tag-alias --list`; a **single object** for every
  record-mutating verb — `add` / `drop-root` / `refresh` / `full-rebuild` /
  `info` / `links` / `stats` / write-ops / `resolve` / `tag-alias
  --add/--rm/--import`). The envelope is **hand-rolled** string
  building (the `jstr` / `jbool` helpers in `kb/main.rs`) rather than a
  serialization crate — so the agent path has no `serde` dependency and
  a hardened build drops it cleanly. Errors go to stderr via the same
  `(String, i32)` contract that plain output uses; the JSON envelope +
  the (unchanged) exit code are the two halves of the result.

---

## Verified core

The `kb-core` crate (no std I/O, no database, no clap) is checked by
**Verus** against a per-module spec (`kb-core/spec/*`). `gate.sh` runs
the full verification surface on every push — that is the guarantee this
project stands on: the pure logic is *correct by proof*, and the shell
around it is small and audited.

Run it locally:

```sh
./gate.sh        # build, one-pkg, clippy -D warnings, tests, verus,
                 # behavior, dep-tree, in that order — fails fast on any red.
```

Prereq for the `verus` stage: a Verus install on `$PATH` and `z3` at
`4.16.0` (see `verify.sh`).

---

## Repo layout

```
.
├── kb/                  # the shared CLI *body* (main.rs, diff.rs, tui/)
│                        #   included by both packages via include!()
│   └── tui/             #   the TUI module: input/, render/, palette.rs,
│                        #   + palettes/{default,colorblind}.toml
├── kb-user/             # the full build: default = ["editor","tui"]
├── kb-hardened/         # the agent build: default = []   (TUI + editor compiled out)
├── kb-core/             # pure + Verus-verified core (no I/O, no deps)
│   ├── src/             #   one module per concern (frontmatter, section,
│   │                    #   identity, tags, aliases, links, search_grammar, …)
│   └── spec/            #   Verus specs, one per module
 ├── kb-db/               # Turso/FTS shell (pure Rust, sync, no tokio)
├── dist/                # per-platform build scripts (linux musl / windows / macos)
│                        #   artifacts are git-ignored; the .sh scripts are tracked
├── tools/               # origin artifacts (e.g. `mdtool.py` — the Python
│                        #   prototype `kb-core::section` was ported from)
├── gate.sh              # full verification (build, clippy, tests, verus, behavior)
├── verify.sh            # Verus + z3 stage (called by gate.sh)
 ├── HARDENED_BUILD.md    # why two packages + the lock on the hardened surface
 ├── PLAN.md              # long-form design notes, module-by-module
 ├── FEATURES-links.md    # the one link/reference grammar + its UX surfaces
 ├── FEATURES-verify.md   # the `kb verify` page lint (contract + reuse map)
 ├── CHANGELOG.md         # per-release notes
 ├── CONTRIBUTING.md      # build/test/contribute
├── .github/             # ISSUE_TEMPLATE/ (bug + feature forms)
│   └── workflows/       #   ci.yml (build+clippy+test+musl) and release.yml
│                        #   (4-runner matrix → GitHub Release assets)
└── LICENSE              # MIT
```

Both binaries are produced from `kb/main.rs`; their `src/main.rs` is a
one-line `include!("../../kb/main.rs")`. See the root `Cargo.toml`
comments for the full rationale of the two-package split.

---

## Further reading

The repo root holds the supporting docs and the verification entry points —
one-line summaries of each:

| File | What it is |
|------|------------|
| [`PLAN.md`](PLAN.md) | Long-form design notes — principles, locked decisions, schema, CLI reference, and the §9 "proven vs. assumed" split. |
| [`FEATURES-links.md`](FEATURES-links.md) | The one link/reference grammar (`[[slug]]` / `[path]` / `[label](…)` / `<uri>` / `#section`) and how it appears in `verify`, `links`, and the TUI. |
| [`FEATURES-verify.md`](FEATURES-verify.md) | The `kb verify` page lint: checks a–e, exit codes, JSON shape, reuse map. |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | The dev loop (`./gate.sh`), feature-flag matrix, and shared-body gotchas. |
| [`HARDENED_BUILD.md`](HARDENED_BUILD.md) | What `kb-hardened` strips (no ratatui, no crossterm, no unicode-width) and why. |
| [`CHANGELOG.md`](CHANGELOG.md) | Semver history, including every fix batch in this session. |
| [`gate.sh`](gate.sh) | Seven-stage gate: build, single-package build, clippy -D warnings, tests, Verus + z3, behavior, dep-tree. |
| [`verify.sh`](verify.sh) | Verus + z3 spec pass over `kb-core/spec`. |
| [`LICENSE`](LICENSE) | MIT. |

The two `.sh` scripts are the *only* entry points you need to trust the build.
Everything else is documentation *about* the build.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the dev loop (`./gate.sh`), the
feature-flag matrix, and the shared-body gotcha. [CHANGELOG.md](CHANGELOG.md)
tracks every release. Report bugs / request features via the issue templates
in `.github/ISSUE_TEMPLATE/`.

## License

MIT — see [LICENSE](LICENSE).

## Project metadata

* **License:** MIT
* **Rust:** 1.98.1 (see `rust-toolchain.toml` — host-agnostic pin)
* **Binary size:** ~4 MB (release, LTO, static musl)
* **Runtime deps (Linux, musl):** none — statically linked, no `libc` at runtime
* **Platforms:** Linux (static musl) · Windows (native) · macOS (ARM + Intel) — all via prebuilt Release binaries or CI
