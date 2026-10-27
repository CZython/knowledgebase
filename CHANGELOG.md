# Changelog

All notable changes to `kb` are recorded here. Dates are first-release dates.
This project follows [Semantic Versioning](https://semver.org/). Until `1.0`,
the API is still moving; breaking changes are called out explicitly.

## [Unreleased]

### Added
- **`D7` — the package identity is now a `const` in the shared body.**
  `kb/main.rs` defines `pub const VARIANT: &str = "user" | "hardened"` once,
  and the existing `Cli` derive (`name = …`, `version = …`) now reads
  `VARIANT` instead of re-evaluating `cfg!(feature = "editor")` inline. Any
  future call site that needs "am I the agent build?" can `VARIANT ==
  "hardened"` and not re-derive the answer from a feature flag.
- **Windows is now a first-class target (runtime, not just "it compiles").**
  The build machinery was already there (`dist/build-windows.sh` — native MSVC
  or `mingw-w64` cross), but the *behavior* was POSIX-baked. Every place that
  read "unix" or printed a unix-only command now branches on the build target:
  * **`kb cron`** emits a `schtasks /create …` line on Windows and the familiar
    `0 * * * * …` crontab line on POSIX, so the printed fragment is paste-ready
    for the OS the binary was built for (previously it always printed a
    crontab line and relied on the user to translate).
  * **`shell_quote`** uses the Windows cmd.exe scheme (double-quoted, backslash
    escapes) and the POSIX single-quote scheme elsewhere — so the emitted
    `cron` command is correct in both shells.
  * **`pick_editor`** falls back to `notepad` on Windows (a stock install has
    no `vi`) and `vi` elsewhere; `VISUAL`/`EDITOR` still take precedence.
  * **Editor error hints** stop printing `$VISUAL`/`$EDITOR`/`$PATH` shell
    syntax that means nothing in `cmd.exe`; they now name the env vars plainly.
  * **The integration tests that pin JSON** invoke `python3` on POSIX and
    `python` on Windows; the editor-saves-bytes test (which drives a
    `#!/bin/sh` + `sed -i` stand-in editor) is now gated `#[cfg(unix)]`; and
    the TUI roots-form test derives its path from `std::env::temp_dir()`
    instead of hardcoded `/tmp`. `kb-core` cross-checks clean for
    `x86_64-pc-windows-gnu`; `kb-db`/`kb` need the `mingw-w64` C cross-compiler
    (a toolchain prerequisite, documented in `dist/build-windows.sh`), not a
    code restriction.
- **Windows build scripts no longer require Git Bash.** Two new `dist/`
  entry points run a stock Windows shell — `build-windows.bat` (cmd.exe) and
  `build-windows.ps1` (PowerShell, with an `-Install` switch) — and
  `build-windows.sh` now surfaces the exact prerequisites when a cross tool
  is missing. Full details: "### Windows build scripts (added)" below.
- **The default DB path is now OS-correct at every call site.** The `kb
  cron` and `kb info` verbs each hard-coded a `~/.local/share/knowledgebase/
  kb.db` *fallback* string that would print a lie on Windows (the real path
  via `dirs::data_local_dir()` is `%LOCALAPPDATA%\knowledgebase\kb.db`). Both
  now call one shared `default_db_display()` (next to `shell_quote`) that
  resolves the per-user data dir and only falls back to a `~` / `%VAR%`
  placeholder a human can read when that is unset — and the placeholder is
  now the right one for the OS the binary was built for. The `--db` help text
  and the `kb-db` canonical-path docs now name both the POSIX and the
  Windows location instead of only the former.

### Windows build scripts (added)
- **`dist/build-windows.bat`** — cmd.exe build+stage+verify (runs everywhere,
  no shell prerequisites beyond `cargo` + the rust target).
- **`dist/build-windows.ps1`** — PowerShell build+stage+verify with an
  `-Install` switch (copy to `%LOCALAPPDATA%\knowledgebase\`, create the dir,
  `setx` the user PATH); `-Target` / `-Out` flags for control. Hardened: the
  build-output path is composed with `Join-Path` (not a hardcoded backslash
  string) so it works under pwsh on POSIX too; the verify step smoke-runs
  under `wine` on a cross host (native on Windows, size check if neither);
  and `-Install` persists the PATH via `[Environment]::SetEnvironmentVariable`
  (not `setx`), sidestepping `setx`'s ~1024-char truncation. Executed here
  under pwsh 7.5.2 (mingw gnu target): build → stage `dist/kb-x86_64-pc-
  windows.exe` → wine run → `kb 0.1.0`, exit 0.
- **`dist/build-windows.sh` (Linux, updated)** — now *gates* the cross
  prerequisites before failing deep in the build (points at the exact
  `apt-get install -y gcc-mingw-w64-x86-64 binutils-mingw-w64-x86-64` and
  `rustup target add x86_64-pc-windows-gnu` lines when a tool is missing), and
  now also **stages** the `.exe` to `dist/` (with a `.sha256`) and
  **smoke-runs** `kb --version` under `wine` (or a plain PE check when wine is
  absent). Verified end-to-end here: cross-built against mingw GCC 13-win32
  and ran `kb --version` → `kb 0.1.0` under wine-9.0.

### Fixed
- **`kb refresh` / `kb full-rebuild` no longer fail with
  `table pages has no column named search_required_phrase` on a DB created
  by a pre-rename version of `kb`.** The schema migration was
  `CREATE TABLE IF NOT EXISTS` — correct for a *fresh* database, wrong for a
  database that already had a `pages` table: `IF NOT EXISTS` is a no-op on an
  existing table's *columns*, so `migrate()` created the surrounding FTS5
  triggers, FTS5 indexes, and other tables but never reconciled the `pages`
  column set. A live DB whose `pages` still carried the pre-`d38340e` column
  `search_terms` would open, then fail on the very first upsert with the error
  above (the column `search_required_phrase` did not exist). `migrate()` now
  reconciles a pre-existing `pages` table:
   * any column in `PAGES_COLUMN_RENAMES` (currently just
     `search_terms` → `search_required_phrase`) is `ALTER TABLE …
     RENAME COLUMN`'d in place — **data preserved**, SQLite ≥ 3.25 rewrites
     trigger bodies that name it so the FTS5 sync stays in lockstep;
   * any column in `PAGES_COLUMNS` still absent after the renames is
     `ALTER TABLE … ADD COLUMN … TEXT` (nullable, matching the DDL's
     optional-column convention — a pre-existing row simply reads `NULL`).

  Verified end-to-end against a copy of a real user DB (3 registered roots,
  0 rows, old `search_terms` column): after `migrate`, `kb refresh` and
  `kb full-rebuild` both complete, the row count goes 0 → 3, and the failing
  note's `search_required_phrase` field is repopulated from its
  frontmatter. Three regression tests in `kb-db::migrate::tests`: one for the
  rename path with data preservation, one for the additive path, and one
  asserting a fresh DB is a no-op.

### Changed
- **`kb-hardened` (the agent build) no longer special-cases a bare `.md` file
  to `info`** — a bare positional token always routes to `search`.
  Previously the pre-parse entry point in `main.rs` probed
  `Path::new(<token>).exists()` *relative to the process CWD* and, for an
  existing `.md` file, rewrote the call to `kb info <file>`; otherwise it
  used `kb search <token>`. That made the dispatch state-dependent (two
  agents in different working directories running the same token got
  different verbs) — the one behavioral branch in the hardened surface that
  depended on ambient filesystem state (pentest F4, recommendation (b)).
  The hardened build now drops that shortcut: agents that want file details
  say `kb info <file>` explicitly, and `kb-hardened <anything>` is always a
  search. The *interactive* `kb-user` build keeps the convenience
  (`kb notes/foo.md` still opens that file's `info`) — the branch is gated
  on the existing `VARIANT` constant, the same "am I the agent build?"
  switch that already drives `--version` / `--help`. Pinned by
   `kb-hardened/tests/m9_dispatch.rs` (`hardened_bare_md_routes_to_search_
   not_info` + `both_bare_non_md_routes_to_search`) and
   `kb-user/tests/m9_dispatch_user.rs` (`user_bare_md_routes_to_info`).
   The latter used to live in `kb-hardened/tests/m9_dispatch.rs` behind an
   `if user_bin().is_none() { return; }` guard — but `kb-hardened` does not
   depend on `kb-user`, so `CARGO_BIN_EXE_kb` was unset on every run and
   the test *silently early-returned*, counting as "passing" while
   asserting zero. Moving it into `kb-user/tests/` (where the env var is
   always set) makes the user-side half of the divergence actually
   exercised on every gate invocation.
 - **Two further SECURITY_REPORT 3.1 invariants that were *silently
   skipped* by the gate are now exercised on every run.** Both were
   `#[cfg(not(feature = "editor"))]` tests in `kb-user/tests/`, but
   `kb-user` defaults `editor` ON, so the gate's `cargo test --workspace`
   never compiled them:
     - `kb-hardened section edit …` is rejected by clap as an
       unrecognized subcommand — now pinned by
       `kb-hardened/tests/m9_dispatch.rs::hardened_section_edit_is_rejected`
       (was `kb-user/tests/m9_agent_surface.rs::section_edit_removed`).
     - `kb-hardened create … -e` is rejected by clap as an unexpected
       argument with exit 2 — now pinned by
       `kb-hardened/tests/m9_dispatch.rs::hardened_create_e_flag_is_rejected`
       (was `kb-user/tests/create_cli.rs::
       create_e_flag_is_rejected_in_hardened_build`).
   `kb-hardened` compiles the identical `#[cfg(feature = "editor")]`
   gates as `kb-user --no-default-features`, so running these in
   `kb-hardened/tests/` exercises the exact code paths the originals
   intended.
- **Two related pre-parse footguns in the *same* entry point are fixed in both
  builds** (pentest F4): the non-flag filter no longer drops tokens containing
  `=`, so `kb E=mc2` is a legitimate single search term (not an "unrecognized
  subcommand"), and the unchecked `non_flag[0]` index is now
   `non_flag.first().expect("len()==1 guarantees non-empty")` — an explicit
   invariant instead of an `Index` out-of-bounds at an index we know is
   reachable.
- **`cmd_resolve` (`kb/cli/links.rs`) no longer uses bare `.unwrap()` on
  the `idx.get(abs)` lookups.** Every `abs_path` in the `resolve()` outcome
  is (by construction in `kb-core::resolve`) some `p.abs_path.as_str()`
  from the input `pages`, which is exactly the key `idx` is indexed on;
  the three `.unwrap()`s are now `.expect("resolve() returned an abs_path
  not present in the input pages")` with a short comment referencing
  `kb-core/spec/resolve_spec.rs`. No behavior change — the panic was
  already unreachable — but a future regression in `kb-core::resolve` will
  now name the broken invariant instead of surfacing as "called `Option::
  unwrap()` on a `None` value".
- **`default_db_display()` (new, shared body)** now owns the *display* form
  of the default DB path for every human-facing call site; the two call sites
  that previously hard-coded a POSIX string now branch on `cfg!(windows)`
  through it. No behavior change on POSIX — the path is byte-identical — but
  on a Windows build the `cron` / `info` / help output now shows the correct
  `%LOCALAPPDATA%` location instead of a `~/.local/share/...` the user would
  not find. `Db::default_db_path()` (the actual *open* path) was already
   cross-platform via `dirs` and is untouched; these were the *display-only*
   fallbacks.
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
- **`C3` — all `--json` output now flows through one mini-library.**
  Previously every verb hand-rolled its own JSON — `search` built compact single-line
  rows, `info`/`roots`/`stats`/`create`/`add`/`refresh`/`commit`/`etag-error`
  each `push_str`-ed a different `format!` template, and the *empty-array*
  case was decided per-emitter (hence the `F6` `[ ]` vs `[]` vs `[{}]`
  inconsistency). `kb/cli/json.rs` now owns the shape:
  * `JObject` — an ordered object with a fixed layout; string values always go
    through the one `jstr` escaper, numbers/booleans are inline literals, and
    nested objects/arrays are passed in pre-rendered. Two renders —
    `render()` (pretty, the `info`/`create` family) and `render_compact()`
    (the `search` row family, byte-pinned in `m7_multifile`).
  * `inline_array` / `block_array` / `top_array` — one owner of the
    *empty* list for each of the three shapes: `["a","b"]` (string values),
    one-object-per-line (result lists), and the top-level `[…]` stdout
    result (browse / tag / search). All collapse to the identical canonical
    `[]` when empty — `roots` no longer emits a stray `[ ]`.
  * `PageJson` — the single page-record shape shared by `browse`/`tag`/
    `info`/`search` — is now built *through* the same `JObject`/`JArray`
    primitives, so the page shape is one thing in exactly one place.
  No decoded value changed; the `jstr` / `jbool` / `PageJson::render` unit
  tests and the `m9_json_fidelity` / `m9_agent_surface` / `m7` byte pins are
  all green.
- **`A1` — the shared CLI body is split into a `kb/cli/` module tree.**
  `kb/main.rs` was a single ~3.4k-line file holding all the CLI verbs plus the
  shared core. It is now a thin crate root (constants, the clap schema,
  `main`/`run`, and the shared helpers) that declares `mod cli;` and pulls the
  verbs in with `use crate::cli::*;`. The verbs live in one file per
  responsibility — `search`, `browse`, `tag`, `links`, `info`, `section`,
  `show`, `roots`, `create`, `cmd_add`, `json`, `writeops`, `textsrc`,
  `editor` — wired up by `kb/cli/mod.rs`. Each verb file does `use crate::*;`
  so it reaches the shared helpers and sibling verbs through the one root
  namespace (the same single-file namespace the body always had — just with a
  file per concern). The `editor` module is `#[cfg(feature = "editor")]`-gated
  end-to-end (declaration, re-export, and inner import), so a `kb-hardened`
   build still compiles it away with zero unused-import warnings under
   `clippy -D warnings`. No behavior changed; the 448-test gate is byte-identical.
- **`A2` — the five oversized CLI verbs are split into named helpers.**
   Each `cmd_*` mixed SQL construction, row binding, and result rendering in
   one body: `cmd_search` was 235 lines, `cmd_create` 210, `cmd_tag_alias`
   184, `cmd_links` 173, `cmd_browse` 119. Each is now a thin orchestrator that
   delegates the self-contained concern to a purpose-named helper, leaving the
   verb a ~10-line (browse) to ~117-line (create) reader and making each
   extracted piece independently testable:
   * `search.rs` — `build_search_sql(parsed, …) -> (sql, params)` owns the
     WHERE-clause / FTS / NOT / gate logic; `render_search(rows|json)` owns the
     two output shapes.
   * `create.rs` — `resolve_target(…)` owns name/extension/title validation +
     path absolutisation; `index_created(…)` owns the best-effort index step.
   * `tag.rs` — `do_import(…)` owns the `--import` file/stdin read, line-by-line
     codec walk, and checked-transaction upsert.
   * `links.rs` — `resolve_target_path(…)` owns the three target shapes
     (abs path / display id / partial path); `render_links(…)` owns the
     shared JSON + human renderers.
   * `browse.rs` — `fetch_browse_pages(…)` owns the filtered vs. full query;
     `render_browse(…)` owns the JSON + human layout.
   Pure refactor: no SQL string, message, or JSON byte changed; all 448 tests
   and the `gate.sh` 7-stage pass are unchanged.
- **`A3` — the `run_editor` / `edit_in_place` pair is now one launch body.**
   The two ~50-line functions shared the same `$VISUAL → $EDITOR → vi`
   resolution, spawn, and temp-buffer cleanup, differing only in launch-vs-
   in-place semantics — and had drifted (the launch-hint logic lived only in
   `edit_in_place`, so a `run_editor` non-zero exit lost it). Both public
   entrypoints now delegate to a    single `run_editor_in(initial, in_place)` that
   keeps the *launch-failed* and *non-zero-exit* branches, and the temp-buffer
   cleanup, in one place. No behavior changed.
- **Test temp-file leak fixed — three test modules were creating files in
   `$TMPDIR` and not removing them.** `section_cli::temp_md` returned a bare
   `PathBuf` whose owner (the test) did not own cleanup → thousands of
   `kb_cli_<pid>_<name>.md` files accumulated per full-suite run. `kb/tui/mod.rs::
   seeded_app` returned a bare `PathBuf` for the seeded page, and every test
   that called `let (_app, page_dir) = seeded_app();` bound the dir but never
   removed it (1,151 `kb_tui_redesign_*` dirs accumulated). `kb/tui/input.rs::
   roots_form_types_jk_in_path` created a fixed `xjk-root` dir once per run.
   Each now owns its scratch via a small RAII guard
   (`TempFile(PathBuf)` and `struct Seeded { page, dir }` in `kb/tui/mod.rs`)
   that deletes on `Drop` via `Deref<Target name="PathBuf">` — so every existing
   call site that used `.to_str()`, `AsRef<Path>`, `Deref`-based `&path`
   derefs, or `Path::parent()` still compiles unchanged. After a full
   `cargo test --workspace` run the `kb*` entries left in `$TMPDIR` are the
   two shared fixed infra dirs (`kb_db_tests/` — one dir shared by all
   `kb-db` tests, per-test `.db` files inside removed by `Db::drop` — and
   `kbdb_schema_version_3_15/` — a schema-version marker dir), not a per-
   call accumulation. No new dependency; the guards are ~20 lines each.
- **`D5` / `D6` — `SECURITY_REPORT.md` and `PLAN.md` now each carry a
   table-of-contents / findings-index at the top, so the ~25
   `SECURITY_REPORT 3.x` and ~30 `PLAN §N` cross-refs in the source comments
   can be resolved against a single anchor table (previously a reader had to
   open the target file *and* scroll to the right numbered section).** The
   SECURITY_REPORT table maps every `3.x` finding to its one-line summary and
   stable `#…` anchor; the PLAN table maps `§1`–`§13o` to their headings
   (with `§9` "proven vs. assumed" and `§10` workspace layout flagged as the
   load-bearing ones). No code, tests, or gate behaviour changed.
- **`D10` — the README now enumerates every root-level document, with a
  one-line "what is it for" summary.** Previously a newcomer had to open each
  of `PLAN.md` / `CONTRIBUTING.md` / `HARDENED_BUILD.md` /
  `SECURITY_REPORT.md` / `PENTEST_HARDENED.md` / `CODEBASE_REVIEW.md` /
  `TODO-palette.md` to figure out whether it was "build", "verify", or "the bug
  catalogue." A *Further reading* table at the bottom of the README answers that
  question in one place.
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
- **`D2` — the `pages.search_required_phrase` SQL column now has one owner.**
  Every query site hand-typed the literal string `"search_required_phrase"`, so
  renaming the column would silently break any site the reader missed. The name
  now lives in a single `pub const COL_SEARCH_REQUIRED_PHRASE` in `kb-db`
  (the crate that owns the schema DDL), and the five shell query sites in the
  CLI (`search` FTS + non-FTS, `browse` LIKE + NONE, `info`) plus the `index`
  upsert `format!` it into the SQL. Rename the constant once and every
  statement follows; a mismatch between two of the six sites can no longer
  compile into a silent "no such column".
- **`B4` — the bare-token dispatch guard is now local to the site that uses it.**
  The old form read `non_flag[0]` inside `if non_flag.len() == 1 && …`; the
  guard was a *couple of lines* from the index, so a reader could not tell at a
  glance the index was safe. Now the first element is pinned (`non_flag.first()`)
  *under* the `len()==1` check, and every later use is the bound name — the
  "am I allowed to index?" question is answered right where the value is read.
- **`B3` — `diff.rs` no longer re-indexes a line it just pattern-matched.**
  The `Some(b' ') | Some(b'-') | Some(b'+')` arm read `l.as_bytes()[0]` *and*
  `l[1..]` from the same string. It now matches the first byte by name
  (`kind @ (b' ' | b'-' | b'+')`) and takes the rest with `split_at(1)` — a
  single pass over the line, and the one char boundary is provably valid
  because the matched arm is an ASCII prefix. A future refactor that loosens
  the arm to `Some(_)` can't silently reach the old second index.
- **`D1` — `search_required_phrase` now carries a cross-reference at both of
  its sites.** The identifier is used for a *corpus field* (the SQL column,
  `kb-core::frontmatter`) and for a *TUI palette role* (`gated` in
  `kb/tui/palette.rs`), which are different concepts that happened to share a
  name. Each site now points at the other, so "change the colour of the
  gate" and "change the SQL column" no longer send a reader to the wrong file.
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
