# `kb` — two build variants: `kb-user` and `kb-hardened`

## Goal

Ship the knowledge base CLI as **two named build targets** from **one shared
source body**:

- **`kb-user`** (package → binary `kb`) — the full CLI: interactive TUI +
  `$VISUAL`/`$EDITOR` editor flow + every other verb.
- **`kb-hardened`** (package → binary `kb-hardened`) — the **headless** CLI
  for agents: **no TUI**, **no editor** (the `$EDITOR`-fork surface is
  compiled out), everything else identical. Later we can add stricter checks
  and compile out more dangerous verbs behind the same gate without
  re-architecting.

Agents drive the plain verbs (scriptable, pipe-able). The TUI is hard to read
by a machine and the `edit` flow forks a user-controlled editor — both belong
only in the full build (`kb`).

## Locked requirements (agreed)

1. `cargo build`             → builds **both** `kb` and `kb-hardened`.
2. `cargo build -p kb-user`  → builds **`kb`** only (alias: `cargo user`).
3. `cargo build -p kb-hardened` → builds **`kb-hardened`** only (alias: `cargo hardened`).
4. `kb-hardened tui`         → clap's normal **`unrecognized subcommand: 'tui'`**
   (stderr, exit **2**) — *not* the bare-string fall-through to search, and
   never mentions a flag.
5. `kb-hardened --version`   → `0.1.0 [hardened]`.
6. `kb --version`            → `0.1.0`.
7. `kb` still has `tui` + `edit` (day-to-day default is unchanged).
8. Hardened is **smaller**: `ratatui` + `crossterm` + `unicode-width` are
   absent from its dependency tree (verify with `cargo tree`).

> **On items 2 and 3.** cargo's built-in `build` command doesn't take a
> *positional* package name — `cargo build kb-user` errors with "unexpected
> argument." The canonical form is `-p`, and `.cargo/config.toml` registers the
> two single-package builds as `cargo user` / `cargo hardened` for the
> day-to-day shorthand (plus `cargo kb-user` / `cargo kb-hardened` for the
> full-name form).

**Not** done this iteration (kept out on purpose): stricter checks, and
dropping any verb other than `tui`/`edit`. Those are future extensions to the
*same* `kb-hardened` package.

## Why two packages, not two bins / `required-features`

- `required-features` only decides **whether** a target builds; it does not
  change *which* features a build uses. Since `kb-user` needs
  `{editor, tui}` **on** and `kb-hardened` needs them **off**, one package
  cannot express both (features are package-global). → **two packages.**
- Two `[[bin]]` in one package *can* share a `src/main.rs`, but still share one
  feature set — same dead end.
- **`include!` across packages works**: a one-line `src/main.rs` in each package
  `include!`s the shared body, and the body's `mod tui;` / `mod diff;` resolve
  relative to the **included file's** directory (verified empirically, incl.
  nested `tui/mod.rs` → `tui/input.rs`).
- `clap` accepts an **expression** for `#[command(name = …)]` and
  `version = concat!(…)`; `cfg!(feature = "…")` selects per-package
  (verified: `… [variant]` prints, rc 0; a removed subcommand → `unrecognized
  subcommand`, rc 2).

So: **one shared source dir compiled into two thin packages.**

## Layout

### Before

```
kb/
├── Cargo.toml                 # package "kb", bin "kb", features editor+tui
├── src/{main.rs, diff.rs, tui/*}
├── testdata/{section_old.md, section_new.md, standard.diff}
└── tests/{create_cli.rs, m7_multifile.rs, m9_agent_surface.rs,
          m9_json_fidelity.rs, section_cli.rs}
```

### After

```
kb/                            # SHARED SOURCE — no Cargo.toml, NOT a member
├── main.rs                    # the one real body (moved from src/main.rs)
├── diff.rs                    # moved from src/diff.rs
├── tui/{mod.rs,input.rs,render.rs}
└── testdata/…                 # unchanged, still here

kb-user/                        # package  →  bin "kb"
├── Cargo.toml                 # name kb-user, deps = today's kb deps, tui+editor deps
└── src/main.rs                # one line: include!("../../kb/main.rs")

kb-hardened/                    # package  →  bin "kb-hardened"
├── Cargo.toml                 # name kb-hardened, deps = kb deps MINUS ratatui/crossterm
└── src/main.rs                # one line: include!("../../kb/main.rs")

<tests>                        # kb/tests/*  →  kb-user/tests/*   (drive kb)
```

Workspace root `members` becomes
`["kb-core", "kb-db", "kb-user", "kb-hardened"]`
(`kb` is dropped because it is now a source-only dir).

## What changes in the shared body (`kb/main.rs`)

1. **Identity** — `name`/`version` become `cfg!`-selected so both packages
   compile the same file:
    ```rust
    #[command(
        name = if cfg!(feature = "editor") { "kb" } else { "kb-hardened" },
        version = if cfg!(feature = "editor") { env!("CARGO_PKG_VERSION") }
                  { concat!(env!("CARGO_PKG_VERSION"), " [hardened]") },
        about = "knowledgebase — single Rust CLI over embedded SQLite+FTS5",
    )]
    ```
   (`editor` is the proxy for "full variant": it is on only for `kb-user`.)
2. **`mod tui;`** stays `#[cfg(feature = "tui")]`; editor verbs stay
   `#[cfg(feature = "editor")]` — now resolved **per package**, exactly as
  intended. Because `kb-hardened` enables neither, both surfaces are compiled
  out *and* their deps are absent.
3. **Bare-string `SUBCMDS`** — `"tui"` and `"edit"` become unconditional in
   that list (it only feeds "is this a bare verb?"); clap rejects unknown
   subcommands on its own for a hardened build, so the list need not differ.
   *(Verify the fall-through is actually gone — see checklist.)*

## Test + fixture path fixes (the only real breakage)

- The full-variant bin is named `kb`, so the 5 integration-test helpers point
  at it via `std::env::var("CARGO_BIN_EXE_kb")` (read at **runtime**; the tests
  live in `kb-user/tests/`, package `kb-user`, bin `kb`). `kb-hardened` is the
  agent surface and is exercised by clippy + the dep-tree gate, not by these
  integration suites.
- `env!("CARGO_MANIFEST_DIR")/testdata/…` → the bin now reports the **package
  dir** (`kb-user/` or `kb-hardened/`), so point at the shared dir explicitly:
  `format!("{}/../kb/testdata/{}", env!("CARGO_MANIFEST_DIR"), name)`.
  Affected: `kb/main.rs` (the `mod json_tests` fixture helper) + `kb/diff.rs:488`
  + `section_cli.rs` (`--patch-file` paths).
- Integration tests belong to the **full** variant (they exercise the
  `add`/`search`/`section`/`diff` verbs, none of which run interactively) →
  live in `kb-user/tests/`.

## Verification checklist (all must hold)

```
cargo build                          # both kb and kb-hardened appear in target/debug/
cargo build -p kb-user               #   → kb only               (alias: cargo user)
cargo build -p kb-hardened           #   → kb-hardened only      (alias: cargo hardened)
cargo tree -p kb-hardened            #   → no ratatui, no crossterm, no unicode-width
./kb                  tui            #   → runs (init-fails w/o TTY is fine = verb exists)
./kb                  --version      # 0.1.0
./kb-hardened         tui            #   → "error: unrecognized subcommand 'tui'" (rc 2)
./kb-hardened         edit <…>       #   → unrecognized (rc 2); -e/--edit unexpected
./kb-hardened         --version      # 0.1.0 [hardened]
./gate.sh              # clippy -D warnings + all unit + integration suites green
cargo clippy -p kb-user --all-targets     # 0 warn
cargo clippy -p kb-hardened --all-targets # 0 warn
./verify.sh                     # kb-core spec still  verified / 0 errors
```

## Open decisions

- *(resolved)* hardened is a distinct package that drops exactly `{tui, edit}`;
  all other verbs kept.
- *(resolved)* structure = two packages sharing one `include!`d body; the
  `kb-user`/`kb-hardened` package names are the `cargo build -p <pkg>` selector
  (day-to-day aliases `cargo user` / `cargo hardened` in `.cargo/config.toml`).
- *(parked)* future `kb-hardened` additions go behind the same feature gates
  — no layout change needed then.
