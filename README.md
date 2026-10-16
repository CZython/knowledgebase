# kb — a single-binary Markdown knowledge base over SQLite + FTS5

One Rust binary that indexes a tree of Markdown notes into an embedded
SQLite / FTS5 database, then lets you **search**, **browse**, and **edit**
them — by section, by substring, or by applying external `diff -u` /
`git diff` patches. The core logic (identity, section splice, search
grammar, alias/tag semantics) lives in a **Verus-verified, zero-dependency
pure-Rust crate**; the SQLite shell is a thin, audited boundary.

There is no server, no daemon, no external binary to keep in step — it is
one self-contained binary (statically linked on Linux; native Mach-O /
`.exe` on macOS / Windows) that reads Markdown and writes a SQLite file.

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
cargo install --path kb-user
# → installs `~/.cargo/bin/kb`
kb --version
```

Or, from the repo root, just run `cargo build` and grab
`target/debug/kb` (for a release binary, `cargo build --release`).

### Prebuilt binaries (Releases)

Every tagged release (`v*`) publishes a native artifact per platform, built
*natively* by CI so the bundled SQLite is compiled against each OS's libc:

| Platform      | Artifact                        | Notes                                        |
|---------------|---------------------------------|----------------------------------------------|
| Linux (all)   | `kb-x86_64-linux-musl`    | **static**, runs on any distro / container — no FUSE, no libs |
| Windows       | `kb-x86_64-pc-windows.exe`| native, no runtime deps                       |
| macOS (ARM)   | `kb-aarch64-apple-darwin` | Apple Silicon                                 |
| macOS (Intel) | `kb-x86_64-apple-darwin`  | pre-Apple-Silicon                             |

Download from the [Releases](../../releases) tab and put it on your
`$PATH`. The Linux one is a single self-contained ELF:

```sh
chmod +x kb-x86_64-linux-musl
./kb-x86_64-linux-musl search "rust async"
```

To build a platform artifact yourself, run the matching script (each
detects the host OS): `dist/build-linux.sh`, `dist/build-windows.sh`,
`dist/build-macos.sh`.

### Prerequisites

A working Rust toolchain (`stable`) is the only build-time requirement.
`rusqlite` is built with the `bundled` feature, so **no system SQLite is
needed**. A C compiler is used transiently to compile the bundled SQLite
C sources.

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
kb tui
```

Every verb has `--help`; every write is idempotent under a matching
`--if-unchanged`, and `--json` suppresses the interactive diff prompt so
stdout is one clean JSON object.

---

## Agent contract (locked)

See `SECURITY_REPORT.md` and `PLAN.md` §13f for the full, audited
contract. The one-paragraph version:

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
  target section's bytes at the time of read; mismatch → exit code **3**.
* **Exit codes**: `0` ok · `1` generic · `2` section/resolve ·
  `3` etag · `2` (also) unrecognized subcommand in the hardened build.

---

## Verified core

The `kb-core` crate (no std I/O, no rusqlite, no clap) is checked by
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
├── kb-user/             # the full build: default = ["editor","tui"]
├── kb-hardened/         # the agent build: default = []   (TUI + editor compiled out)
├── kb-core/             # pure + Verus-verified core (no I/O, no deps)
│   ├── src/             #   one module per concern (frontmatter, section,
│   │                    #   identity, tags, aliases, links, search_grammar, …)
│   └── spec/            #   Verus specs, one per module
├── kb-db/               # SQLite/FTS5 shell (bundled, sync, no tokio)
├── tests/               # shared integration fixtures / e2e tests
├── dist/                # per-platform build scripts (linux musl / windows / macos)
│                        #   artifacts are git-ignored; the .sh scripts are tracked
├── tools/               # origin artifacts (e.g. `mdtool.py` — the Python
│                        #   prototype `kb-core::section` was ported from)
├── .github/workflows/   # ci.yml (build+clippy+test+musl) and release.yml
│                        #   (3-platform matrix → GitHub Release assets)
├── gate.sh              # full verification (build, clippy, tests, verus, behavior)
├── verify.sh            # Verus + z3 stage (called by gate.sh)
├── HARDENED_BUILD.md    # why two packages + the lock on the hardened surface
├── PLAN.md              # long-form design notes, module-by-module
├── SECURITY_REPORT.md   # threat model + audit + resolved findings
└── LICENSE              # MIT
```

Both binaries are produced from `kb/main.rs`; their `src/main.rs` is a
one-line `include!("../../kb/main.rs")`. See the root `Cargo.toml`
comments for the full rationale of the two-package split.

---

## License

MIT — see [LICENSE](LICENSE).

## Badges / metadata

* **License:** MIT
* **Rust:** 1.98+ (see `rust-toolchain.toml`)
* **Binary size:** ~4 MB (release, LTO, static musl)
* **Runtime deps (Linux, musl):** none — statically linked, no `libc` at runtime
* **Platforms:** Linux (static musl) · Windows (native) · macOS (ARM + Intel) — all via prebuilt Release binaries or CI
