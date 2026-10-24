# Changelog

All notable changes to `kb` are recorded here. Dates are first-release dates.
This project follows [Semantic Versioning](https://semver.org/). Until `1.0`,
the API is still moving; breaking changes are called out explicitly.

## [Unreleased]

### Added
- **The status bar now carries a severity glyph (`✓` for success, `!` for
  error; none for info).** In the *colorblind* palette, or in any user TOML
  where the user has collapsed `error` and `success` onto the same color, the
  glyph is the only thing that still tells a write-success apart from a
  validation-failure. Same "symbols carry the meaning" principle as the
  `◆`/`◇` gate markers — the `!` symbol is what a colorblind reader relies on.
  Test: `status_bar_glyph_distincts_success_from_error`.
- **The default palette is now *colorful* (the out-of-the-box look).**
  Previously it was the historical gray palette; it now uses:
  magenta/bold for headings and panel titles, white for body, cyan/bold for
  the active field, light-green for gated / success, light-red for errors,
  and black-on-blue for the selection bar. `default.toml` is a shipped
  starting point (users can copy it to `~/.kb_config/default.toml` and tweak
  it) and its tests now pin the *colorful* scheme, not the old gray one.
   `--list-palettes` now describes `default` as "the colorful,
   out-of-the-box look" — which is what it actually is.
- **Browse/picker pane titles now carry identity + count only; the
  keybinding legend lives in the status bar, the one place a user glances
  for "what do I press".** Before this pass the same hints were printed in
  two places at once (the pane title *and* the bottom status bar), so on
  real-width terminals both copies clip-doubled (`…e edit m…` up top, `…q q`
  below). Now `browse_pane_title` (identity + count) is the only string in a
  title, and the status bar is the only place the keybinding legend appears.
  Two new tests pin the contract: `browse_pane_title_carries_no_keybinding_legend`.
- **Read-pane outline is now a `List`, one heading per visible line (no
  mid-word wrapping).** The old `Paragraph` + `Wrap{trim:false}` broke `L3  H2
  section` into two visible lines (`sub` / `section`) on narrow panes;
  `outline_line` (pure palette + entry → `Line<'static>`) returns a single
  `Line` per heading — dim `L<n>` anchor + `label`-styled title — so a heading
  never splits. The pane is `p.selection`-ready when M8.2's Enter-to-jump
  lands. Two new tests pin the shape: `outline_line_is_single_line_per_heading`.

- **The TUI "edit" flow is now a clean two-stage, zsh-style model (renamed to
  `edit metadata`).** Opening the metadata editor from the browse/read view
  lands on a *field picker* first (a deliberate step): select one of
  `title` / `description` / `tags` / `search_required_phrase`, each showing a *preview*
  of what's on disk (a field is never a mystery before you edit it), then
  `Enter` opens that field's **value** (scalars pre-filled for an in-place
  edit; lists start empty with a live token menu). Once a field is open, the
  choice is **final** — the zsh *type → arrows-pick → Tab-fill → Enter-commit*
  idiom applies and the field never changes under you:
  - **Typing narrows the live menu in place** (a pure function of the buffer)
    — a filter change never drops the list, it just re-anchors it.
  - **`↑/↓` move the highlight across the candidates** (wrapping). They are
    *always* this — they no longer jump to the next/previous field (that was
    the source of the "I already picked this field, why did it change?"
    complaint). On a scalar field (no candidates) they are a clean no-op.
  - **`Tab` autocompletes the buffer with the highlighted candidate** (fills
    it with the full known token, zsh-style, and re-narrows the list — the
    highlight stays put so a second `Tab` / `Enter` is stable). On a scalar
    (no candidates) `Tab` is a clean no-op, leaving your text exactly as typed.
  - **`Enter` commits the buffer as typed** (with candidate-aware dedup for
    lists) and *stays on the same field* for the next token — the tag-list
    workflow (add `security`, then `data`, then `mtls`) flows without
    re-picking the field. A scalar `Enter` commits and stays.
  - **Every letter is just a letter in the value stage.** The old command-letter
    hijacks that killed real words are gone: `q` no longer quits, `a`/`r` no
    longer jump to the alias/roots managers, `x` no longer drops the list tail.
    You can type `query`, `data`, `security`, `tax` without fighting the keymap.
  - **`Backspace` on an *empty* buffer drops the list's tail item** (the "delete
    the last thing I added" semantic, replaced the old `x` key and is more
    discoverable). `Backspace` with text just deletes the last character. On an
    empty list it's a clean no-op.
   - **`Esc` pops one stage**: value → picker → browse. `C-c` is the one
     unconditional quit. `q`/`a`/`r` (quit / alias / roots) are reachable only in
     the picker, where no letter is being typed.
- **`←`/`→` are a text cursor on scalar fields.** A char-level caret moves
  mid-buffer (typing inserts at the cursor, `Backspace` deletes the char before
  it) — so a "mid-word" edit (e.g. fixing a typo in `HelloX World`) does not
  require re-typing the whole token. `←` at start / `→` at end are clean no-ops.
- **List fields get a *selection sub-mode* on top of the buffer.** In buffer
  mode, `←`/`→` (and only then) enter Selection: a combined list of
  `[tag₁, …, tagₙ, <new>]` where `↑/↓/←/→` all navigate the highlight
  (wrapping), `Del`/`Backspace` live-removes the highlighted item (a real
  `fm::remove_list_item`, no file write until `Enter`), `Enter` on a tag *loads
  it into the buffer* (a "refine" path; the next `Enter` will commit an atomic
  `RemoveItem(old)` + `AppendItem(new)` rename, not a duplicated append),
  `Enter` on `<new>` returns to buffer mode with the cached token preserved,
  and any typed letter exits Selection and appends that letter to the buffer.
  `Esc` also returns to buffer mode. The two sub-modes (Buffer / Selection)
  are visually distinct in the header and the block title.
- **Atomic rename-on-tag-edit for lists.** Committing a buffer that matches an
  existing tag (either freshly-typed or loaded via Selection `Enter`) emits
  `RemoveItem(old)` + `AppendItem(new)` in one plan — the file's list is
  re-serialized in one pass, no intermediate state ever appears. Dedup is
  still case-sensitive (a "mTLS" vs "mtls" commit adds a *new* token, it does
  not replace the old).
- **A clearer, color-coded value pane (presentation).** The edit-metadata
  value buffer was a single dim block where you couldn't tell what a key would
  touch. It now uses a **one-triangle** cue: the single green `▶` marks the
  one row your *current* keys act on (the **input buffer** in buffer/scalar
  mode, with a bright `▮` block caret at the exact insert point; the
  **highlighted item** in Selection mode — `Backspace` removes / `Enter` loads
  it). `»` marks the highlighted candidate in buffer mode (the one `Tab` fills
  from), and dim `·` marks read-only context (existing items, the cached
  buffer). All the `•` dots are gone. So you can always tell at a glance
  whether your arrows are typing, picking a candidate, or walking existing
  tags. Live-verified in a terminal for all three sub-modes.
- **`v` key in the browse/read views opens the selected page in `$EDITOR`**
  (`$VISUAL` → `$EDITOR` → `vi`), reusing the existing `edit in place` path —
  in-editor flow from the TUI without leaving it. Gated on the `editor`
   feature (absent in the hardened build).
- **`--palette <name|path>` + `--list-palettes` on the TUI.** The TUI's
  color vocabulary is now the *single source of truth* for every rendered
  style — `render.rs` no longer names any `Style` / `Color` / `Modifier`
  directly. Thirteen named roles (`label`, `default`, `dim`, `active`,
  `caret`, `hint`, `gated`, `open`, `info`, `success`, `error`,
  `selection`, `selection_ctx`) map to concrete styles via a tiny TOML
  file, and the roles are *stable* so a user-supplied file (e.g. a
   personal colorblind palette) only has to re-map colors without touching
   any render code. Two palettes ship: **`default`** (the colorful,
   out-of-the-box look) and **`colorblind`** (red→magenta, green→
   cyan/blue; for red–green colorblind users). Use `--palette path/to.toml`
   for a custom file.
   `kb tui --list-palettes` renders a small ANSI swatch table of the four
   most user-visible roles (`label`/`active`/`error`/`selection`) so you
   can compare without launching the TUI.
- **Gate markers in the browse list and the field picker.** Every browse
  row and every field-picker row now *double-encodes* "is this gated?" —
  a symbol (`◆` for gated / open, `◇` for un-gated) *and* a palette role
  (`gated` vs `open`) — so a colorblind reader sees the distinction in
  monochrome. A page is gated when it has one or more
  `search_required_phrase` entries (the secret-unlock-phrase semantic
  documented under the `search_required_phrase` rename above); a field is
  "populated" (and thus `◆`) when it already has a value on disk.
  Filter (`/`) now also matches *tags* and *gate phrases*, so you can
  surface a gated page without knowing its id / title / path.
- **Read-pane heading hierarchy.** ATX headings (`#` through `######`
  followed by a space) in the read body now render in the palette's
  `label` role, so the document structure is visually distinguishable
  from prose (and from a code block that happens to start with `#` — the
  detection is visual, stateless, and deterministic rather than carrying
  a markdown parser through the render path).
- **Palette-styled alias + roots managers.** The alias row splits into
  *alias* (`p.default`) + `→` (`p.label`) + *canonical* (`p.dim`), and
  the roots row splits into *path* (`p.default`) + any `[options]` /
  `(added: …)` trailing annotations (`p.dim`) — the primary identity
  stays the visual focus in both. Selection highlight on either list now
   uses `p.selection` (the default is black-on-blue).
- **Tests:** `browse_row_marks_gated_open_differently`,
  `apply_filter_matches_tags_and_gates`, `read_line_styles_headings_only`,
  and `picker_rows_carry_gate_markers_for_each_field` pin the new
  behavior (the marker *symbol*, its *palette role*, and the filter /
  heading logic) so a future `default.toml` edit cannot silently change
  what `◆`/`◇` mean to the user.
### Removed

- **The `mono` palette is dropped.** It had one audience (a true 2-color
  terminal or a printed TUI) that is essentially extinct — the only
  real-world 2-color terminals left are embedded devices over USART, which
  are not running this TUI. Dropping it also drops the only palette where
  issue #4 (the "dim locator flips to white on the selected row" blemish)
  actually shows up — in `default` and `colorblind` the selection is
  black-on-white / black-on-blue, which is the strongest possible contrast
  and the symbols already carry the gate meaning. If someone later ships
  a fixed-2-color build of the TUI it can be re-added as a user-supplied
  TOML (every role is required in a file, so there's no fallback to guess).

### Renamed

- **The `search_terms` frontmatter field is now `search_required_phrase`.**
  The old name read as "extra keywords/synonyms that *broaden* recall," which
  is the opposite of what the field does: a page *with* entries is **hidden**
  from general search and appears only once the query text contains **any one**
  entry as a contiguous span (a restriction, not a recall signal — a "secret
  unlock phrase"). The new name states that. **Singular** on purpose: the gate
  is an `OR`/`EXISTS` over the entries (any one phrase suffices), so the
  plural "phrases" would misread as an AND (satisfy-all) requirement. This is
  a pre-public rename with no compatibility shim — every occurrence was changed
  (frontmatter key, `NoteMeta` / `Frontmatter` fields, the `pages` DB column,
  the `--json` output key, the TUI field `SearchTerms` →
  `SearchRequiredPhrase`, all docs/fixtures/tests). The internal gate mechanism
  (`page_gates`, `GateClass`, "gated page") is unchanged. `SCHEMA_VERSION` is
  2 (marker; no on-disk DBs existed to migrate).

### Changed
- **`edit in place` (`-e`) / `v` failures now tell you *why*.** A failed
  *launch* (no editor found) and a non-zero *exit* are reported distinctly, and
  the launch case names the env var tried (`$VISUAL`/`$EDITOR`) and that the
  fallback `vi` isn't on `$PATH`, pointing at the fix instead of a bare
  `launch …: No such file`. The file is left untouched either way.
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

### Fixed
- **`kb tui` no longer breaks `kb-hardened --features tui` (F1).** The
  `kb-hardened` package declared a `tui` feature that *looked* like
  `kb-user`'s but was missing `serde` + `toml`, so `cargo build -p
  kb-hardened --features tui` failed with 10 unresolved-`serde`/`toml` errors
  (the `--palette` machinery in `kb/tui/palette.rs` derives them). The feature
  now mirrors `kb-user` exactly (`dep:serde` + `dep:toml`, both optional and
  never linked in the default headless build — the dep-tree gate still passes),
  so the two packages truly are "the same body, different default flags."
- **A bare search token containing `=` is no longer rejected (F4).**
  `kb E=mc2` (a legitimate single search term) hit the bare-dispatch filter
  `!a.contains('=')`, which dropped it from the bare-token set, so the shortcut
  was skipped and clap answered `unrecognized subcommand 'E=mc2'`. The filter is
  gone — a bare token is always reconstructed as `kb search "<token>"` (one
  argument, never a `--flag=val`), so it routes to `search` and only then
  surfaces the FTS-level "syntax error near `=`" (correct: the `=` is *inside*
  the query, not a flag). Real subcommands still route normally. Regression-tested.
- **`--json` "no rows" is one canonical `[]` across every verb (F6/C3).** The
  empty-result branches disagreed byte-for-byte: `search … tag:<missing>` emitted
  a phantom record `[{}]`, while `browse`/`tag`/`tag-alias` empty lists emitted
  `[ ]` (with a space) and the text path `[]`. An agent doing `len(json.loads(s))`
  or `jq 'map(.id)'` saw a spurious element in one shape and not the other. Every
  empty branch now prints the identical canonical `[]` (the populated `[{}]`
  item-rendering is untouched), and the integration suite exact-pins it (not just
  "is an array"). Regression-tested.
- **`append` on an absent field returns a clean `Err`, not a panic (B1).**
  `list_rewrite_upgrade` aborted via
  `.expect("scalar_value found a scalar…")` when the key's line wasn't found —
  a caller-contract panic on an agent *write* path (and a counter-example to the
   module's own invariant **I4**, which promises no `.unwrap()`/`.expect()` in the
   non-test zone). It now returns `Err(FmError::NotScalar)`.
   Regression-tested: `upgrade_scalar_missing_returns_err_not_panic`.
- **The `etag` FNV-1a hash now has a single owner (B6).** Three copies of the
   identical 16-hex FNV-1a-64 lived in `kb/main.rs`, `kb-db/src/index.rs`, and
   the CLI path — so a fix to the prime/offset in one would silently desync the
   `--if-unchanged` optimistic-concurrency check across the indexer and the CLI.
   The canonical implementation is now `kb_core::etag::of`, and both the CLI
   (`etag_of` wrapper) and `kb-db`'s indexer delegate to it. The
   empty-string golden (`cbf29ce484222325`) is pinned in `kb-core`.
- **The interactive editor read-back no longer swallows a real error (B5) and
   its temp file is now unique (F8).** `run_editor` read back the buffer with
   `std::fs::read(&tmp).unwrap_or_default()`, so a failed read after a
   *successful* editor exit (buffer vanished) was silently treated as "no
   changes" (`[]`). It now surfaces `read back edit buffer …: <err>` with
   `EXIT_ERR`. The buffer file name was also just `<pid>.md`, guessable by a
   competing `kb` or a `chmod` race on a shared `/tmp`; it is now
   `kb-edit-<pid>-<seq>-<nanos>.md` (atomic counter + nanos), matching the
   per-test isolation fix applied to the TUI metadata tests.
- **`confirm()` returns the abort as an error instead of a re-wrapped `bool`
   (C1).** Both call sites (`create` and `commit`) spelled the identical
   `if !confirm(…) { return Err(("aborted (not confirmed)".into(), EXIT_ERR)); }`
   wrap, pinning that exact message + `EXIT_ERR` in two places. `confirm` now
   returns `Result<(), (String, i32)>` and each caller writes
   `confirm(…)?;` — the abort outcome lives in exactly one place.
 - **The `cron` verb derives the default DB path from `Db::default_db_path()`
    (F9).** `kb cron` hardcoded `~/.local/share/knowledgebase/kb.db`, which is
    wrong on macOS (where `dirs::data_local_dir()` is
    `~/Library/Application Support`) — so the emitted crontab line could point
    at a DB the tool never opened. The path is now built in exactly one place
    (`kb_db::Db::default_db_path`), shared by `open_default` and the `cron`
    verb, and the hardcoded literal is the fallback only when `data_local_dir`
    is unavailable.
- **Whole-project `unsafe_code = "forbid"` via `[workspace.lints.rust]` (C2).**
    The tree is `unsafe`-free (verified: no `unsafe` in `kb-core`, `kb-db`, the
    shared CLI body, or any workspace member). That invariant is now a hard
    compile *error* across **all four** crates, declared once in workspace
    `Cargo.toml` and inherited by every member (each has `[lints] workspace =
    true`), so a future `unsafe` block — a TUI-backend FFI fix, a raw-pointer
    shortcut — fails to build anywhere unless someone deliberately opts out
    with `#[allow(unsafe_code)]` + a reason. Single source of truth; no
    per-file attributes to drift. Verified: injecting an `unsafe` block into
    `kb-core` is rejected; removing it builds again.
- **The frontmatter `append` path can no longer index out of bounds (B2).**
    `list_rewrite_add` did `locate_key_line(data, key).unwrap_or(0)` then
    `lines[idx]`; a desync (e.g. a BOM shifting line numbers) with an index
    `≥ lines.len()` used to panic. It now bounds-checks and returns
    `Err(FmError::NotScalar)` on both missing and out-of-range indices, and the
    function returns `Result<Vec<u8>, FmError>` end-to-end (its single caller
    already propagated `Result`, so the `append` verb surface is unchanged).
- **`cmd_info` link counts `?`-propagate instead of `unwrap_or(0)` (B7).**
    The two `SELECT COUNT(*)` `query_row` calls used `unwrap_or(0)`, hiding a
    real query failure as `0 out / 0 in` with exit 0. They now
    `map_err(…) → EXIT_ERR` (with the same `"<verb>: <e>"` shape as every other
    error), so a genuine failure surfaces instead of a silent zero.
- **The hand-built SQLite `IN (…)` lowercase-escape has one owner (C4,
    SECURITY_REPORT 3.4).** `cmd_search` and `cmd_tag` each inlined
    `format!("'{}'", t.to_lowercase().replace('\'', "''"))` (two copies, two
    variable names), and the same `''` doubling appeared a third time in the
    shell-quoting helper. The tag-IN-list sites now call one
    `sql_quote_lower(s)` helper, so the "did we escape?" and "did we
    case-fold?" questions have a single answer at every call site.
- **The `prepare search` error no longer leaks the full SQL text (F5).** On a
    prepare failure the whole query (SELECT list, column names, table names,
    gate subquery) was printed to stderr — a schema side-channel and louder than
    the failure warrants. The message is now the clean `prepare search: <err>`,
    with the full SQL appended only when `KB_DEBUG_SQL` is set.
- **A TUI metadata commit that only *removed* a list item never wrote to
  disk.** The commit's write-gate compared its result to the in-memory
  *working* copy, but a tail-removal was already baked into that copy — so a
  commit whose only effect was a `Backspace`-on-empty-buffer tail drop saw
  "no diff" and silently skipped the file write (and re-index), leaving the
  removal un-persisted. The gate now compares against the on-disk bytes
  (equal to the entry baseline at that point), which correctly writes for
  *any* op mix — appends, removals, and mixed — while still honoring the
  "open then commit → byte-identical" no-op guarantee. Regression-tested.
- **The TUI edit-metadata tests now each own an on-disk page (no more
  cross-test flake).** Every `seeded_app()` call keyed its temp dir on
  `process::id()` alone, but all tests in the binary share one pid — so
  parallel test threads read/wrote the *same* `page.md`, and one test's commit
  raced another test's re-seed, intermittently failing the "the committed file
  has the tail removed" assertion (it passed in isolation). `seeded_app` now
  mints a per-call unique dir (atomic sequence + pid) and removes any stale one
  first, fully isolating each call.

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
