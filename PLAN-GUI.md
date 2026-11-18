# PLAN-GUI — `kb-gui`: an egui desktop client for `kb`

Status: **draft plan** (no code yet).

This document plans a native desktop GUI for `kb`, built with **egui/eframe**,
mimicking the three-pane dark layout of **Marknote** (workspace rail · note list ·
editor), with a **WYSIWYG Markdown editor** and a top-right **Formatted / Raw**
toggle. The app must **not** use an embedded browser/webview to render Markdown.

The existing TUI (`kb/tui/`) is the primary inspiration for behavior and state
machinery: Browse / Read / Edit / Search / Alias / Roots / Links. The GUI is a
second *assumed shell* over the same verified core and the same Turso index —
not a second implementation of the product.

---

## 1. Goals and non-goals

### Goals

- **Native, fast, offline desktop app** for Linux/macOS/Windows.
- **Marknote-like design**: dark theme, workspace rail, note list, large editor
  canvas, top-right view toggle.
- **WYSIWYG Markdown editing** with the Markdown file remaining the source of
  truth and byte-exact round-tripping when nothing changes.
- **Formatted ⇄ Raw toggle**, top-right, remembering scroll/selection per view.
- **Browse, search (full `search_grammar`), read, edit, create** notes.
- **All pages first**: one always-available, browse-parity view of the whole
  corpus, including gated pages.
- **Collections as scopes**: group pages by their `search_required_phrase`
  terms, and use the same terms to scope search to a field/topic.
- **Fast frontmatter editing**: all known fields visible, keyboard-first,
  `Tab`-completable tags and collections, no per-field modal.
- **Integrated settings**: roots, refresh/full-rebuild, tag aliases, collections,
  theme, and data tools live behind the app menu, not in the main chrome.
- **A proper launch surface**: no-argument launch shows search plus
  open/create actions; first run without roots shows a short welcome/onboarding.
- **Generic Markdown support**: opening a `.md` that is not a valid `kb` page
  works, with `kb`-specific features disabled and a non-blocking conversion
  banner.
- **Themes**: built-in dark/light and custom TOML themes, switchable live from
  Settings, covering both chrome and Markdown styles.
- **Fast cold start**: process launch → first painted empty/skeleton chrome in
  **<500 ms** on a reference-class machine (multicore CPU ≥4 cores, SSD); no
  DB, corpus, or page-content work on the first-frame path.
- **Manage corpus state**: roots, refresh/full-rebuild, tag aliases, tags,
  link navigation, section outline.
- **Reuse** `kb-core` (verified) and `kb-db` (index/write path). The GUI adds no
  new proven code and no new storage model.
- **No embedded browser**. Markdown is parsed with a Rust parser and rendered
  with egui widgets/galleys.
- **Hardened build unaffected**: `kb-hardened` must not pull egui/winit into its
  dependency tree.

### Non-goals (v1)

- Replacing the CLI or the TUI; both remain supported.
- Background filesystem watchers, cloud sync, collaboration, mobile.
- An Obsidian-scale plugin system.
- Rendering arbitrary HTML or running JavaScript.
- Full Word/Docs-style rich text (the document is always Markdown).

---

## 2. Reference design (Marknote) and mapping to `kb`

Target layout, adapted from the reference image:

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ☰    kb                    filesystem design            ⌕   [ A ]  [ </> ]    │  ← top bar
├────────┬─────────────────────────┬───────────────────────────────────────────┤
│  ▣ all │  +  filter · group ▾    │  # Energy in electronics        ⌕   ⋯      │  ← content header
│  128   │  ┌────────────────────┐ │                                           │
│ ◆ gated│  │ ▤ Energy ...       │ │  ## Energy in electronics                 │  ← editor canvas
│   38   │  │   18.11.26 9:20  ⋮ │ │                                           │
│ ◇ open │  └────────────────────┘ │  Power is the rate at which energy is     │
│   90   │  ▤ Nyquist ...         │  transferred...                           │
│ ────── │    ...                 │                                           │
│ Elec.. │                        │                                           │
│   12   │                        │                                           │
│ 128 notes                         │                                           │
└────────┴─────────────────────────┴───────────────────────────────────────────┘
```

| Marknote element | `kb` meaning |
|---|---|
| Workspace rail icons + badge | **Collections** derived from `search_required_phrase` (`page_gates`), badge = page count; pinned `All / Gated / Ungated` above |
| Note list rows | Current list context: `All pages` (browse parity), a collection scope, or an explicit filter |
| Note row timestamp | `pages.updated_at` / file `mtime` |
| Note row kebab menu | Rename/open externally/copy path/delete (v1: copy path + open in system editor) |
| Add note (`+`) | `kb create`-equivalent: pick root, type, title, and collection term(s) |
| Note title search | Filter within the current collection/All, case-insensitive over `id`/`title`/`rel_path`/tag/gate |
| Top-right search icon | Full `search_grammar` search overlay (`Cmd/Ctrl+K`), scoped to the current collection |
| Top-right view toggle | **Formatted** ⇄ **Raw** Markdown |
| Centered window title | Native titlebar only (portability) |
| Bottom-left “2 notes” | Count of visible pages |
| Large editor canvas | Read/Write surface with max content width (~820 px) |

### 2.1 All pages first, Collections are scopes

This is the load-bearing product decision. **Collections are not folders and
not roots.** A collection is the set of pages that share a
`search_required_phrase` gate term; the same term is a field/topic scope for
search. “Energy” in electronics, signal processing, and physics is the anchoring
example: the pages share vocabulary but belong to different fields, and the gate
term is exactly the field marker that already disambiguates them in search.

Rules:

1. **`All pages` is pinned and always shows every indexed page** — exact
   `kb browse` parity, gated pages included, no implicit filtering. Roots never
   hide pages and are not the rail grouping.
2. **A collection is a many-to-many membership** derived from `page_gates`.
   A page with two gate terms belongs to two collections. Normal lists show it
   once with collection chips; `Group by: Collection` intentionally duplicates
   it under every gate.
3. **Selecting a collection filters the list *and* scopes search.** The
   collection's gate term satisfies the gate predicate, so searching `energy`
   inside `Electronics` returns the electronics page without retyping
   `electronics`. FTS still matches only the user's query; the scope affects the
   gate predicate, not term ranking.
4. **Gateless pages live in `Ungated`** and remain in `All`. There is no
   requirement to gate every page.
5. **Gated pages are grouped, not hidden.** `Gated` and each collection are
   navigation aids; `All` remains the escape hatch.
6. **Gate terms are normalized case-insensitively for grouping** and displayed
   with a stable, title-cased label. Optional `gate_aliases` (mirroring
   `tag_aliases`) can merge synonyms later; none is required for v1.
7. **No schema change.** Collections are a read-side derivation from
   `page_gates`; roots remain corpus-registration state in Settings.

Rail model:

```text
▣ All pages              128     ← browse parity, always present
  Gated                   38
  Ungated                 90
  ── collections ──
  Electronics             12
  Signal processing        7
  Physics                 19
```

Counts are membership counts (a multi-collection page counts in each), so the
collection badges do not necessarily sum to `All pages`.

### 2.2 Launch states and document kinds

The app must behave well when launched empty, from a file manager, or on a
Markdown file that was never meant for `kb`.

| Launch input | Result |
|---|---|
| No path/ref, **no roots** registered | **Welcome** onboarding screen |
| No path/ref, roots exist | **Home**: search field + open/create buttons |
| File path (open-with / argv) | Open that file and classify it |
| Page ref (`type:slug`, uuid prefix, rel path) | Resolve via `kb-core::resolver`, then open |
| Directory | Offer to add it as a root (pre-filled) |
| Ambiguous ref | Candidate picker |

Document kinds (a property of the open document, not the file format):

```text
DocumentKind
├─ KbPage            valid type:, under a registered root, indexed in pages
├─ UnindexedMarkdown valid type:, but outside every root / not in pages
└─ GenericMarkdown   missing or unknown type:, or unparseable frontmatter
```

Rules:

1. `All pages`, collections, search, links, and backlinks only cover `KbPage`
   documents. Generic/unindexed files open in a **transient editor tab** and
   do not appear in the list.
2. `GenericMarkdown` disables `kb`-specific features (section 6.15) and shows
   the conversion banner. Raw/formatted editing, save, outline, and basic
   Markdown rendering still work.
3. `UnindexedMarkdown` gets a variant banner: “Not indexed — add the containing
   folder as a root to enable search, links, and collections.”
4. The editor is never blocked by missing roots. A file path always opens; only
   `kb` features are gated.

### Design tokens (approximate the reference)

| Token | Value | Use |
|---|---|---|
| `bg` | `#1e1f22` | window / editor background |
| `panel` | `#232428` | rail and note list |
| `panel_alt` | `#26282c` | header bars, hover |
| `border` | `#33363c` | dividers |
| `selection` | `#2e5a88` | selected note row |
| `accent` | `#b180d7` | headings, links, active toggle |
| `text` | `#d4d4d4` | body |
| `text_muted` | `#8a8f98` | timestamps, paths |
| `success` | `#57a773` | saved status |
| `error` | `#e06c75` | conflicts/errors |

The TUI's semantic palette roles (`kb/tui/palette.rs`) are the model; the GUI
gets its own `Theme` type with egui `Color32`s, but the **role names stay
aligned** (`label`, `default`, `dim`, `active`, `hint`, `gated`, `open`, `info`,
`success`, `error`, `selection`) so the two shells keep one visual vocabulary.

---

## 3. Architecture

### 3.1 Crate graph

```text
kb-core  (pure, Verus-verified)
   ▲
kb-db    (Turso / FTS / index)
   ▲                     ▲
kb-user (TUI + CLI)     kb-gui (eframe/egui)
```

- **`kb-gui`** is a new workspace member producing a `kb-gui` binary.
- **No `kb-shell` crate.** The repo already duplicates query/write behavior
  between the CLI and TUI; `kb-gui` follows that pattern for now rather than
  forcing an extraction. Its copies live in `kb-gui/src/data/` and are ported
  deliberately from `kb/cli/*`, `kb/main.rs`, and `kb/tui/mod.rs`:
  - `list_pages`, `load_page`, `load_body`, outline extraction
  - `search_rows` / `build_search_sql`, plus the optional `gate_scope`
  - collection facets from `page_gates`, plus alias/root list + add/drop
  - `atomic_write` (from `kb/main.rs:1738`)
  - single-page reindex (`build_page` + `upsert_page`, `kb-db/src/index.rs`)
  - link annotation/resolution helpers
- Duplication is accepted for speed and isolation. The mitigation is exact
  parity tests against the CLI/TUI for browse, search, save, roots, and aliases.
  If drift becomes painful later, extraction can be revisited separately.

`kb-gui` is **not** in `default-members`; `cargo build`/CI for the CLI stays
unchanged. Add to the root `Cargo.toml`:

```toml
default-members = ["kb-core", "kb-db", "kb-user", "kb-hardened"]
members = ["kb-core", "kb-db", "kb-user", "kb-hardened", "kb-gui"]
```

`kb-gui` inherits `unsafe_code = "forbid"`; egui/winit internals may use
`unsafe`, but no first-party code does.

### 3.2 Threading and DB

Turso's `Connection` is a sync façade and is not assumed `Send`. Design:

- A **DB worker thread owns the `Db` connection** for the whole session.
- UI sends `Cmd` values and receives `Evt` values over channels:
  ```text
  Cmd::LoadCorpus | Cmd::OpenPage(path) | Cmd::Search(query)
  Cmd::Save { path, expected_disk: Vec<u8>, new_bytes: Vec<u8> }
  Cmd::Refresh | Cmd::AddRoot(path) | Cmd::DropRoot(path)
  Cmd::ListAliases | Cmd::AddAlias | Cmd::RemoveAlias
  Cmd::ClassifyFile(path) | Cmd::ConvertToPage { path, opts }
  Cmd::SettingsSnapshot
  ```
  ```text
  Evt::Corpus(Vec<PageSummary>) | Evt::Page(OpenPage) | Evt::SearchResults(..)
  Evt::Classified(FileClass) | Evt::Settings(SettingsSnapshot)
  Evt::Progress { op, done, total } | Evt::Saved { .. } | Evt::Conflict { .. }
  Evt::Error { .. }
  ```
- The UI never blocks on disk/SQL. `ctx.request_repaint()` wakes egui when an
  event arrives.
- Long operations (`refresh`, `full-rebuild`) stream progress events and keep
  the UI responsive.
- egui itself is single-threaded; the worker is the only background thread.
- **The worker is spawned from the first `update` after laying out the empty
  shell**, then detached; it is never joined on the first-frame path. The DB
  open, migration, corpus load, and argv file classification all happen off the
  UI thread.

### 3.3 Source-of-truth contract

Same contract as the TUI and CLI:

1. **Disk is authoritative.** DB rows are a rebuildable cache.
2. Any write:
   - verifies the on-disk bytes still match what the editor opened (conflict
     guard, equivalent to `--if-unchanged`/TUI's `entry_bytes` check),
   - writes atomically (`atomic_write`, `kb/main.rs:1738`),
   - reindexes the single page with `build_page(..., include_language: true)`
     and `upsert_page` (same path as `kb/tui/mod.rs:2677`).
3. A save never rewrites bytes it did not edit.
4. Conflicts surface as a dialog: **Reload**, **Keep mine (write anyway)**,
   **Diff** — never silent clobbering.

### 3.4 Startup sequence and budget

Hard requirement: **from process spawn (Enter in the terminal) to the first
painted chrome with empty/skeleton content: <500 ms** on a reference-class
machine (defined in section 13).

Startup order:

1. `main` parses minimal argv, reads only a bounded preferences/theme file
   (≤64 KB), resolves a built-in or tiny custom theme, and constructs the
   eframe app with **empty state**.
2. **Frame 1 paints immediately**: window, top bar, collection rail skeleton,
   note-list skeleton, home/loading pane. No DB, corpus, page content, image,
   or font work is allowed before this frame.
3. Only then does the DB worker open/migrate the database, load roots/corpus/
   settings, classify any argv file, and emit events.
4. The UI repaints as data arrives. Until then it shows skeletons, not a blank
   window.

Budget targets (reference class, p95, cold start):

| Phase | Target |
|---|---|
| Process start + dynamic libs + window creation | < 250 ms |
| Theme resolution + egui first frame | < 100 ms |
| Slack (fonts, compositor, variance) | < 150 ms |
| **Total to empty GUI** | **< 500 ms** |

Rules:

- **Never** call `Db::open`, migrations, `load_corpus`, `refresh`, or read page
  content on the first-frame path. The only allowed pre-frame reads are the
  bounded preferences and selected theme files.
- Built-in themes are compiled in (`include_str!`); a custom theme TOML is tiny
  and may be parsed before frame 1. Fonts, icons, and images load lazily or
  after frame 1.
- No markdown parsing, page rendering, or corpus enumeration before frame 1.
- A path argument loads asynchronously; the shell appears first, then the file.
- Use the **glow** renderer in v1; do not pay for a wgpu adapter probe before
  the first frame. Benchmark other renderers only if the target is already met.
- System light/dark detection must not block frame 1: start from the cached
  last-known appearance and correct after the first frame if needed.
- Add a developer-only `--startup-report` flag / `KB_GUI_TRACE_STARTUP=1`
  env that prints the elapsed time at the first frame. Use it in the startup
  test and CI.
- In debug builds, a `FirstFrameGate` panics if `Db::open` or a corpus load is
  attempted before frame 1; release builds log it.
- “Empty GUI” means chrome plus empty/skeleton panes, not a blank native window.

---

## 4. Dependencies (proposed)

| Crate | Why |
|---|---|
| `eframe` + `egui` | window, event loop, widgets, text shaping; `glow` renderer in v1 for fast startup |
| `egui_extras` (optional) | local image loading, table helpers |
| `pulldown-cmark` | CommonMark/GFM parsing into events (no HTML rendering) |
| `unicode-width` (already in tree) | line/column geometry where needed |
| `serde` (already) | persisted UI preferences and theme schema |
| `toml` (already in workspace) | custom theme files |
| `directories`/`dirs` (already) | config/themes location |
| `syntect` (deferred, G6) | optional code-block syntax highlighting |

Explicitly **not** used: `webview`, `wry`, `servo`, `webkit2gtk`, `reqwest` for
rendering, or any HTML-to-image path.

The repo currently pins dependencies to what is cached offline; egui is **not**
cached on this machine. First build will need network, or the dependency set
must be vendored (`cargo vendor`) and the lockfile committed. Pin one exact egui
release at G0 and do not float versions.

---

## 5. Module layout (`kb-gui`)

```text
kb-gui/
  Cargo.toml
  src/
    main.rs              # eframe::run_native, CLI args (--db, --theme, --startup-report)
    app.rs               # top-level App, eframe::App impl, layout dispatch
    state.rs             # AppState, selection, view mode, dialogs, status
    data/
      mod.rs             # worker handle, Cmd/Evt
      corpus.rs          # page list + collection facets/counts
      page.rs            # open page, frontmatter, outline, links
      search.rs          # search_grammar -> results; gate-scoped search
      write.rs           # save/conflict/atomic write/reindex
      autocomplete.rs    # tag/collection/language suggestion sources
      convert.rs         # generic -> valid page classification + rewrite
      settings.rs        # roots/aliases/stats snapshots + mutations
      theme.rs           # find/parse/validate custom theme files
    md/
      mod.rs
      parse.rs           # pulldown-cmark -> Block/Inline IR
      ir.rs              # source-spanned document model
      render.rs          # IR -> egui widgets (read-only formatted view)
      style.rs           # inline LayoutJob styling
    edit/
      block.rs           # per-block editor widgets
      map.rs             # source span -> replacement splicing
      commit.rs          # debounce, conflict check, save orchestration
    ui/
      mod.rs
      chrome.rs          # top bar, content header, status bar
      rail.rs            # All / Gated / Ungated / collection rail
      list.rs            # note list, group-by, collection chips
      editor.rs          # raw/formatted editor host
      outline.rs         # section outline panel
      frontmatter.rs     # metadata inspector/editor + autocomplete popups
      search.rs          # search overlay
      home.rs            # landing: search + open/create actions
      welcome.rs         # first-run no-roots onboarding
      settings.rs        # app menu settings modal (tabs)
      banner.rs          # generic/unindexed conversion banners
      dialogs.rs         # new note, convert, conflict, roots, aliases
      theme.rs           # Theme tokens, built-in themes, egui Visuals
```

---

## 6. UI specification

### 6.1 Window

- Default `1200 × 800`, minimum `900 × 600`.
- **Marknote Dark** by default; built-in Light and High Contrast themes, plus
  System follow and custom TOML themes (section 11).
- Native titlebar only; no custom centered-title chrome (portability across
  X11/Wayland/macOS/Windows wins over visual parity here).
- High-DPI and font scaling respected.

### 6.2 Top bar (height ~40 px)

Left: menu button (`☰`) with App menu (New note, Add root, Refresh, Full rebuild,
Theme ▸, Aliases, Settings, Quit), then app name `kb`.
Center: current page title; when a collection is active, a scope chip
(`in: Electronics`) sits next to it and is also reflected in the search box.
Right: search icon, **Formatted/Raw segmented toggle**, optional outline toggle.

### 6.3 Collections rail (~72 px)

- Pinned tiles: **All pages**, **Gated**, **Ungated** (icon + label + count).
- Below: one tile per collection, derived from the distinct gate terms in
  `page_gates`, label truncated, page-count badge.
- Selecting a collection filters the list and sets the search scope; selecting
  `All pages` clears the scope.
- Selected tile uses `selection` background.
- Context menu on a collection: Open, Search in this collection, Copy gate
  phrase, Rename (edits the frontmatter field across pages, with confirmation).
- A collection with zero pages disappears from the rail.
- Roots are **not** in the rail; they live in Settings / Add root.
- Bottom of rail: “Add root” shortcut and “Manage collections” (opens
  Settings → Collections).

### 6.4 Note list (250–320 px, resizable)

- Header: `+` new note, inline filter field, **Group by**
  (`None · Collection · Visibility`), sort menu (title/date/path).
- Default `Group by: None` in `All pages` for browse parity.
- Rows: page-type icon, title (bold), timestamp/path (muted), gated marker (`◆`),
  collection chips for other memberships, and kebab menu on hover.
- In `Group by: Collection`, a page appears under **every** gate it has; rows
  repeat by design and there is no `page_gates`-order “primary”. Gateless pages
  remain under the `Ungated` section. Other groupings show each page once with
  membership chips.
- In `Group by: Visibility`, sections are `Open` / `Gated`.
- Selected row: `selection` background.
- Multi-select not in v1.
- Virtualized via `ScrollArea::show_rows` for large corpora.
- Footer: `N notes` (matching the reference); when in a collection, show
  `N in <collection>`. In `Group by: Collection`, show unique notes and row
  count separately (`N notes · M rows`) because memberships duplicate. If
  gate-aware search hides results, the footer/banner explains how to reveal
  them.

### 6.5 Editor host

- Content column centered, max width ~820 px, generous padding.
- Header inside content: page title/identity, collection chips, tag chips,
  kebab, **view toggle** (mirrored from top bar), save state.
- Two interchangeable bodies:
  - **Formatted**: WYSIWYG block editor (section 8).
  - **Raw**: full-source multiline text editor, monospace, line numbers,
    smart indentation, visible Markdown.
- The toggle is top-right and keyboard-driven (`Cmd/Ctrl+E`). Scroll position,
  cursor, and selection are preserved across toggles where possible.

### 6.6 Outline panel (optional right side, ~220 px)

- Extracted with the same heading logic as `kb toc` / TUI (`heading_of`).
- Click scrolls the editor to the heading.
- Toggle with a button; hidden by default on narrow windows.

### 6.7 Status bar (~24 px)

- Save state: `Saved`, `Saving…`, `Unsaved changes`, `Conflict`.
- Selected page path/rel path.
- Counts, refresh progress, errors.
- Color by severity (`info`/`success`/`error`).

### 6.8 Search overlay / panel

- `Cmd/Ctrl+K` opens a centered overlay.
- Full `search_grammar` query syntax (`type:`, `tag:`, `~`, `-`, `"phrase"`).
- Live results, keyboard navigation, `Enter` opens, `Esc` closes.
- Tag autocomplete for a trailing `tag:` token (alias-first, like the TUI).
- **Scope chip**: when a collection is active, the overlay shows
  `in: Electronics`; the search layer receives `gate_scope = [Electronics]` and
  treats the gate predicate as satisfied for pages in that scope. The chip is
  removable (`All fields`) and the scope is remembered per collection.
- `All pages` search uses the **strict** CLI/TUI gate rules. There is no
  `Include gated` toggle. If gated pages match the query but are hidden, show
  `N gated matches hidden — add the gate term to the query to include them`
  and link to the in-KB “Search gates” help page. The escape hatch is
  documentation plus the user adding the gate term, not a GUI override.
- Scoped search parity test: `energy` inside `Electronics` returns the same
  page as `energy electronics` globally, but FTS ranking is computed from
  `energy` alone.
- Gate-term autocomplete: while editing a collection scope or the frontmatter
  Collections field, suggest existing gate terms from `page_gates`.
- Search results keep their state when navigating into a page and back.

### 6.9 Frontmatter inspector (fast, keyboard-first)

Frontmatter editing is a first-class, low-friction surface — closer to a
property panel than a form wizard.

- **No per-field modal.** A compact property panel (right side or under the
  content header) shows **all known fields at once**. Toggle with
  `Cmd/Ctrl+Shift+F`, the Properties button in the header, or by clicking the
  title/collection chips. In Formatted view it renders as labeled rows and
  chips; in Raw view the YAML is visible and editable.
- **Fields** (tab order):
  1. `type` — combo: `note` / `wiki` / `label` (required)
  2. `title` — single-line text
  3. `description` — single-line text, ≤255 with live counter
  4. `language` — single-line text
  5. `tags` — chip/list editor
  6. `search_required_phrase` — presented as **Collections** (field/topic
     scopes; each term is also the search unlock phrase)
  - Read-only footer: display id, abs path, uuid short.
  - `type` and `language` are first-class fields. Changing `type` never deletes
    the file; it can deindex it, so show a warning and reindex after the write.
- **Fast keyboard model** (mirrors the TUI's zsh-style autocomplete):
  - `Tab` — when the suggestion popup is open, **accept the highlighted
    suggestion**; when it is closed, move focus to the next field.
  - `Shift+Tab` — move focus to the previous field (or up in the popup).
  - `↑` / `↓` — move the suggestion highlight while the popup is open.
  - `Enter` — commit the current field and advance to the next; with a popup
    open, accept the highlighted suggestion first.
  - `Esc` — close the popup; if already closed, revert the current field to its
    on-disk value.
  - `Cmd/Ctrl+Enter` — commit every dirty field in one atomic write.
  - `Cmd/Ctrl+Shift+F` — focus/toggle the inspector.
- **Autocomplete sources** (loaded with the corpus, refreshed after writes):
  - `type`: fixed enum.
  - `language`: `SELECT DISTINCT language FROM pages` (plus free text).
  - `tags`: `SELECT DISTINCT tag FROM page_tags` (case-insensitive filter,
    prefix matches first).
  - `search_required_phrase` / Collections: `SELECT DISTINCT term FROM
    page_gates`; free-form new terms are allowed.
  - Free-text fields still accept any value; suggestions are a speed aid, not a
    constraint.
- **Chip editors for tags/collections**:
  - Typing filters the corpus; `Tab`/`Enter`/`,` accepts the suggestion or the
    literal token; `Backspace` on an empty buffer removes the last chip; `×`
    removes a chip; double-click edits a chip in place.
  - Deduplicate case-insensitively; preserve the existing canonical spelling.
  - A new tag/collection is written as a list append; a removed one as a
    targeted list-item removal.
- **Validation and preview**:
  - `type` required; `description` ≤255; empty tokens rejected; gate terms
    stored unquoted (the existing `search_required_phrase` format).
  - The footer shows a one-line pending-change summary (for example
    `+1 tag, +1 collection, title changed`), matching the TUI's commit preview.
  - No-op commits (nothing changed) write nothing.
- **Commit path**: verified `kb_core::frontmatter` ops only
  (`set_scalar` / `append_list_item` / `remove_list_item` / `remove_field`),
  batched into one atomic write with the same disk guard as body edits.
  Autosave after the debounce; undo/redo covers frontmatter edits too.
- **Collections note**: editing this field is collection membership. Adding a
  term makes the page appear in that collection; removing it detaches the page;
  the rail counts refresh immediately after the commit.

### 6.10 Dialogs

- **New note**: type (note/wiki/label), title, root, collection term(s),
  optional initial body. With at least one collection term, the new page starts
  gated; leaving it empty starts in `Ungated`. Uses the same autocomplete field
  widgets as the frontmatter inspector.
- **Convert to page** (generic Markdown): choose `type`, prefill `title` from
  H1/filename, optional tags + collection term(s), preview the exact
  frontmatter diff, then write + index. If the file is outside every root,
  offer “Add containing folder as root & convert” as the primary action.
- **Collection rename**: edit a gate term across every page in the collection
  (frontmatter list rewrite with a dry-run summary), or detach pages instead.
- **Conflict**: file changed on disk → Reload / Keep mine / Show diff.
- **Roots**: add/drop root, walk options (extensions, hidden); registration
  only, not the rail grouping. Also reachable from Settings.
- **Aliases**: list, add (`alias → canonical`), remove.

### 6.11 Keyboard shortcuts

| Shortcut | Action |
|---|---|
| `Cmd/Ctrl+K` | Search |
| `Cmd/Ctrl+,` | Settings |
| `Cmd/Ctrl+S` | Save now |
| `Cmd/Ctrl+E` | Toggle Formatted/Raw |
| `Cmd/Ctrl+N` | New note |
| `Cmd/Ctrl+O` | Open file |
| `Tab` | Accept suggestion if popup open; otherwise next field |
| `Shift+Tab` | Previous field (or up in the popup) |
| `↑` / `↓` | Move autocomplete selection |
| `Enter` | Commit field / open result |
| `Cmd/Ctrl+Enter` | Commit all frontmatter fields |
| `Cmd/Ctrl+B` / `Cmd/Ctrl+I` | Bold / italic selection |
| `Cmd/Ctrl+Shift+C` | Inline code selection |
| `Cmd/Ctrl+Shift+K` | Insert link |
| `Cmd/Ctrl+Shift+F` | Frontmatter inspector |
| `Cmd/Ctrl+R` | Refresh corpus |
| `Alt+Left` | Back (link/search history) |
| `Esc` | Close overlay / leave field |
| `Ctrl+Q` / `Cmd+Q` | Quit (native) |

### 6.12 Settings surface

Administration lives in one place, behind the top-left `☰` menu → **Settings**
(and `Cmd/Ctrl+,`), so the main chrome stays quiet. It is an in-app modal/sheet
with a left tab list for now. A separate window is a later possibility if bulk
root/alias work needs more room; keep settings state in a shared service so that
change does not introduce async config races.

| Tab | Contents |
|---|---|
| **General** | Autosave delay, default editor view (Formatted/Raw), restore-last-page, privacy mode (mask gated titles) |
| **Appearance** | Theme picker with live preview (built-in + custom), follow-system toggle, density, open themes folder, reload |
| **Roots** | Registered roots table (path, page count, extra extensions, include-hidden, last refresh); Add folder (native picker); Remove (confirm; files untouched); Refresh this root; walk options; reveal folder |
| **Collections** | Derived gate terms with counts; rename a term across its pages; detach a term from pages; merge by rename; explains that collections are `search_required_phrase` scopes |
| **Tag aliases** | Table of `alias → canonical`; add row with tag autocomplete; delete; validation (alias ≠ canonical, canonical must exist or be explicitly allowed) |
| **Search** | Default scope (All / last collection), result limit, alias-aware tag matching; gate rules stay strict (no include-gated override) |
| **Data** | DB path and size, page/gate/tag counts, last refresh, **Refresh**, **Full rebuild** (typed confirmation + progress), copy `kb cron` line, open DB folder |
| **About** | Version, build variant (`kb`), license, repo links |

Rules:

- Every admin action calls `kb-gui/src/data` routines copied from the CLI/TUI
  (`add_root` + `refresh`, `drop_root`, full rebuild, alias upsert/delete).
- Roots are registration, not navigation; adding/removing a root never touches
  the files themselves. Removing a root is confirmed and explains that the DB
  rows disappear on the next refresh.
- Destructive actions (drop root, full rebuild, collection rename across many
  pages) show a dry-run summary first.
- Long operations show progress and keep the main window usable.
- Settings changes that affect the session (theme, scope default, privacy mode)
  apply immediately; persisted preferences go to the platform config directory.
- Anything not essential (cron line, DB size, walk options) lives here, not in
  the top bar.

Action map (all implemented in `kb-gui/src/data`, not the UI layer):

| UI action | Shared routine |
|---|---|
| Add folder as root | `add_root` + `refresh` |
| Remove root | `drop_root` + `refresh` |
| Refresh / Refresh this root | `refresh` (scoped by root when requested) |
| Full rebuild | `refresh --full` |
| Add/remove tag alias | same SQL as TUI `AliasState` / CLI `tag-alias` |
| Rename/detach collection term | frontmatter list ops across pages + reindex |
| Convert generic file | frontmatter ops + `save_page` + `build_page`/`upsert_page` |

### 6.13 Home (launch with no path/ref)

When `kb-gui` starts without a file path or page ref, the central pane shows
**Home** instead of an empty editor:

```text
                    Search all pages…
        [ type a query, page title, tag, or uuid…            ]

        [ Open existing page… ]   [ Create new page… ]
        [ Open Markdown file… ]

        Recent
        ▤ Energy in electronics        in Electronics
        ▤ Nyquist and sampling          in Signal processing
```

- The search field is focused on launch and supports the full
  `search_grammar` (with the same strict hidden-gate guidance as the search
  overlay; no include-gated toggle).
- `Enter` opens the top/selected result; `↓`/`↑` walk results.
- **Open existing page…** opens the in-app page picker/search over indexed pages
  (no file dialog). **Create new page…** opens the New note dialog.
  **Open Markdown file…** opens a native file picker for supplying an arbitrary
  path and supports generic files.
- Recent pages are derived from `pages.updated_at` (or a small session/persisted
  recent list); selecting one opens it.
- If roots exist but there are zero pages, show “No pages indexed yet.” with
  **Refresh** and **Add root** actions instead of a dead search field.
- If there are no roots at all, show the Welcome screen (6.14) instead.

### 6.14 Welcome (first run, no roots)

Shown for a no-path launch when the `roots` table is empty:

```text
                         Welcome!

        Add folder as first root to your pages.

        1. Pick a folder containing Markdown notes.
        2. Add it as a root — kb indexes the pages it finds.
        3. Remove it any time in Settings → Roots. Files are never deleted.

        Only Markdown with a valid `type:` becomes a page.
        Other .md files can still be opened and converted later.

        [ Add folder as root… ]      [ Open Settings ]
        [ Open a Markdown file… ]    [ Learn more ]
```

- **Add folder as root…** is the primary, fuss-free action: native directory
  picker → `add_root` → refresh with progress → land on Home/All pages with a
  success message. No Settings detour required.
- The short tutorial explains adding and removing roots; “Settings → Roots” is
  the removal path, and removing a root unregisters it without deleting files.
- The no-type rule is stated here so generic files do not surprise the user;
  the conversion flow (6.15) covers them.
- **Open Settings** goes straight to the Roots tab. **Open a Markdown file…**
  lets the user work on a file immediately even before adding a root.
- If the chosen folder has no indexable Markdown, show a non-blocking notice
  with “Choose another folder” and a link explaining `type:`.

### 6.15 Generic Markdown and conversion

A file is a valid page when `frontmatter::parse` yields a known `type:`
(`note` / `wiki` / `label`) and it is under a registered root. Otherwise:

| Case | Banner | Behavior |
|---|---|---|
| Missing/unknown `type:` | `Would you like to convert this markdown file into a valid page?  [Convert] [Dismiss]` | Generic editor; `kb` features disabled |
| Valid `type:` but outside roots / not indexed | `Not indexed — add the containing folder as a root?  [Add root] [Dismiss]` | Full editing; search/links/collections disabled until indexed |
| Unparseable frontmatter | Generic banner + `Frontmatter could not be parsed` | Conversion offers a minimal-frontmatter preview |

The banner is a non-blocking strip above the document, not a modal.

Disabled in `GenericMarkdown` (and until indexed):

- Collection rail/scoping, collection chips, and gate-aware search integration.
- Page id/uuid, labels/refs, backlinks, link resolution against the corpus,
  and `All pages` membership.
- DB-derived tag/collection autocomplete (free-form editing still allowed).
- Search/outline across the corpus (the local document outline still works).
- The structured frontmatter inspector is read-only; raw YAML editing still
  works, and the conversion dialog owns `type`/`title`/tags/collections.

Kept: raw and formatted editing, save, local outline, find, file path actions,
and the conversion entry point.

**Convert flow**

1. **Convert** opens the conversion dialog: `type` (required, default `note`),
   `title` prefilled from H1/filename, optional tags, optional collection
   terms; preview the exact frontmatter diff.
2. If the file is outside every root, the primary action is **Add containing
   folder as root & convert** (with the normal walk options). If no roots
   exist, the Welcome/Settings root flow is offered first.
3. Conversion preserves the body and all existing frontmatter fields; it only
   adds/sets the missing `type:`, title fallback, and any chosen list items. It
   never rewrites the whole file.
4. Write atomically, reindex with `build_page` + `upsert_page`, refresh the
   page list and collection facets, then reclassify the document as `KbPage`.
   The banner disappears and all `kb` features enable.
5. If indexing fails after the write, the file is still saved; show a warning
   and keep the unindexed banner with a **Retry indexing** action.
6. **Dismiss** hides the banner for the session. A **Convert to page** /
   **Retry indexing** action remains in the `⋯` menu and Settings, so dismissal
   is not permanent.
7. Conversion is undoable until the next save: undo restores the pre-conversion
   bytes and the `GenericMarkdown` state.

Opening a generic file from the file manager follows the same path; the
document opens in a transient editor tab, not in `All pages`.

---

## 7. Markdown rendering (no embedded browser)

Pipeline:

```text
file bytes ──► pulldown-cmark parser ──► md::ir::Document
              (CommonMark + GFM)         (spanned blocks/inlines)
                                            │
                                            ├─► read-only render: egui widgets
                                            └─► WYSIWYG editor: per-block widgets
```

- `pulldown-cmark` is used **only as a parser**; its HTML renderer is unused.
- The IR is ours: `Block` variants (`Heading`, `Paragraph`, `List`, `Quote`,
  `Code`, `Table`, `Rule`, `Image`, `HtmlFallback`) each carry a **source byte
  span** and their content.
- Inline formatting (`Strong`, `Emphasis`, `Code`, `Link`, `Strikethrough`)
  becomes styled ranges and is rendered with `egui::text::LayoutJob` / `Galley`.
- Images: local files under registered roots via `egui::Image`; remote URLs show
  a placeholder (no network fetch in v1).
- Tables: `egui::Grid`.
- Code blocks: monospace, background panel; optional syntax highlighting later.
- Raw HTML: shown as literal text in a muted block. **Never executed.**
- Internal links (`[[…]]`, resolvable paths) become clickable and navigate
  within the app. External links open only on explicit click via the OS opener
  (optional; not required for rendering).
- The renderer is deterministic and source-mapped; rendering must not mutate the
  document.

---

## 8. WYSIWYG editing model

### 8.1 Core idea

The document is always **Markdown source**. WYSIWYG is a *view* over that
source, not a separate rich-text document. This preserves compatibility with the
CLI/TUI, git, and the existing file format.

### 8.2 Block-span model

`Document` keeps an ordered list of blocks, each with:

```text
BlockSpan {
  kind: Heading(level) | Paragraph | ListItem(..) | Quote | Code(..) | ...
  range: Range<usize>,     // byte range in the original source
  content_range: Range<usize>, // range excluding block markers where applicable
  rendered: ...            // cached inline layout
}
```

On open:

1. Read the file bytes.
2. Parse into IR with exact source spans.
3. Build a display document and per-block editor states.

On edit of a block:

1. The widget edits the block’s *content* (not the whole file).
2. On commit (focus loss, `Enter` for new block, debounce idle, explicit save):
   replace **only that block’s source range** with the new Markdown text.
3. Reparse the changed region (or the whole document for v1; reparse is cheap
   for typical notes) and rebuild spans.
4. Update the single “dirty” buffer; autosave after a debounce.

This mirrors the repo’s section-splice philosophy: edit the smallest span,
leave every other byte untouched.

### 8.3 What is hidden in Formatted view

| Construct | Formatted view | Raw view |
|---|---|---|
| Heading `# ` markers | Hidden; rendered as large heading | Visible |
| List `- ` / `1. ` | Hidden; drawn as bullet/number | Visible |
| Quote `> ` | Left accent bar | Visible |
| Code fences | Background panel, no fence lines | Visible |
| Emphasis `*`/`**`/`_` | **Hidden**; rendered as styled text | Visible |
| Inline code backticks | **Hidden**; rendered with code background | Visible |
| Link URLs | Clickable text; URL hidden in v1 tooltip | Visible |

Inline markers are hidden in Formatted view, matching the clean reference look.
Because an egui text field maps cursor positions to the displayed string, hiding
delimiters requires a custom rich-text widget with grapheme-to-source mapping.
G4 owns that widget; the **Formatted/Raw toggle is the escape hatch**, and Raw
always shows the exact Markdown. There is no dimmed-marker intermediate state.

### 8.4 Structural edits

Typing at the end of a list item and pressing `Enter` creates a new list item
(with correct marker/indent). `Tab`/`Shift+Tab` adjusts list nesting. Backspace
at the start of a heading/list item can demote it to a paragraph (same semantics
as raw editing). These are block operations that rewrite the affected span and
reparse.

### 8.5 Inline formatting

Toolbar buttons and shortcuts operate on the current selection:
wrap/unwrap `**…**`, `*…*`, `` `…` ``, `[…](…)`, `~~…~~`. In Formatted view the
visual result updates immediately; the underlying source keeps the markers.

### 8.6 Guarantees / tests

- **Identity round-trip**: open a document and save with no edits → bytes
  identical, no write.
- **Span isolation**: editing one block never changes bytes outside its span.
- **Toggle stability**: Formatted ⇄ Raw ⇄ Formatted with no edits preserves the
  source exactly.
- **Inline hiding**: formatted text renders no `*`/`_`/backtick glyphs; cursor,
  selection, and copy map to the exact source ranges; Raw restores the markers.
- **Fuzz**: random Markdown corpora (reuse `kb-core/tests/fuzz_corpus.rs` style)
  must round-trip through parse → render → no-op edit → source.

---

## 9. Data layer and write path

### 9.1 Reads

- `list_pages` (same SQL as `kb/tui/mod.rs:4289`) plus tag/gate maps. `All pages`
  is this unfiltered list — exact `kb browse` parity, gated pages included.
- **Collection facets**: derive `Collection { label, terms, count }` from the
  distinct normalized `page_gates.term` values, preserving a stable display
  casing. Also derive `gated` / `ungated` counts. This is read-side only; no
  schema change.
- `load_page` reads DB metadata for the list; **content is re-read from disk**
  when opening a page, so the editor never edits stale DB body text.
- Outline via `kb_core::section::heading_of`.
- Search via `search_grammar::parse` + a port of `build_search_sql` from
  `kb/cli/search.rs`; extend the local builder with
  `gate_scope: Option<&[String]>`: when present, the gate predicate becomes
  “page has one of the scoped terms” instead of “the query text contains the
  gate phrase.” FTS ranking still uses only the user's query. `None` keeps
  CLI/TUI behavior byte-identical.
- Link annotation/resolution via `kb-core::links` + `resolver`, reusing the
  TUI's annotated-link shape (`FEATURES-links.md`).
- **Autocomplete snapshot**: distinct `page_tags.tag`,
  `page_gates.term`, `pages.language`, the fixed `NoteType` enum, and recent
  pages (`ORDER BY updated_at DESC LIMIT N`). Loaded with the corpus and
  refreshed after any successful write; free-form input is always allowed.
- **Classify file**: given an absolute path, read frontmatter and determine
  `KbPage` / `UnindexedMarkdown` / `GenericMarkdown` using
  `frontmatter::parse` + `is_indexable()`, root-prefix membership, and a
  `pages.abs_path` lookup. Also return the parse warning, if any.
- **Settings snapshot**: roots (with page counts and walk options), tag aliases,
  DB path/size, last refresh, and counts for the settings modal.
- Tags/aliases/roots reuse the same queries as the TUI.

### 9.2 Writes

All writes go through `kb-gui::data::write::save_page`, ported from the CLI/TUI
save path:

```text
save_page(abs_path, expected_disk_bytes, new_bytes):
  on_disk = read(abs_path)
  if on_disk != expected_disk_bytes: return Conflict
  if new_bytes != on_disk: atomic_write(abs_path, new_bytes)
  rec = build_page(abs_path, "", "", include_language=true)
  upsert_page(db, rec)
  return Saved
```

- Frontmatter edits use `fm::set_scalar` / `append_list_item` /
  `remove_field` / `remove_list_item`, batching every dirty field into one
  `save_page` call. No-op fields are skipped.
- Changing `type` is a normal frontmatter edit. It may make the page
  generic/unindexed, but it never deletes the file; warn when the new type is
  missing/unknown, reindex after the write, and recover by fixing the type or
  reopening the path (the file is still on disk under a root).
- Editing `search_required_phrase` is the collection-membership write: add /
  remove gate terms, then reindex the page and refresh collection facets. A
  collection rename is a multi-page frontmatter rewrite with a dry-run summary
  (never a new DB-side collection table).
- **Conversion**: `convert_generic(path, opts)` builds only the missing
  frontmatter (type, title fallback, optional tags/collections), writes through
  the same `save_page`, and indexes. It preserves the body and unrelated fields
  byte-for-byte. If the file is outside all roots, the containing folder is
  added first (explicit user action) or the conversion is declined.
- **Settings mutations**: add/drop root, refresh/full rebuild, alias
  upsert/delete. These call the duplicated `kb-gui/src/data` routines (mirroring
  the CLI/TUI), not ad-hoc SQL from UI code, and update the settings snapshot
  after completion.
- New note creation reuses the `create` logic semantics (frontmatter `type`,
  title, H1, optional collection/gate terms) and `upsert_page`.
- Refresh/full rebuild/root add/drop/alias add/remove reuse the CLI/TUI
  routines, not new SQL.
- Autosave: 700 ms debounce after last edit, on focus loss, and on explicit
  `Cmd/Ctrl+S`. A narrow status indicator always shows state.
- Conflict handling has no silent path: the editor keeps the user's text and
  shows Reload / Keep mine / Diff.

### 9.3 Performance targets

- **Cold start to empty chrome: < 500 ms** (section 3.4); DB/corpus loading is
  off the first-frame path and appears asynchronously.
- Open a page: < 100 ms for typical notes after the DB is ready.
- Keystroke latency: < 16 ms in Formatted view (local edit, no DB).
- Theme switch: < 16 ms, no restart, no re-layout beyond a repaint.
- Search: streamed/virtualized results; no full UI freeze.
- Large page (1 MB): lazy block rendering; only visible blocks are laid out.
- Large corpus: first shell still <500 ms; list load is progressive/virtualized,
  never a blocking full render before frame 1.
- Corpus list: `ScrollArea::show_rows` virtualization.

---

## 10. State machine

```text
Startup
  └─ FirstFrame (theme + empty chrome, no DB/file IO)
       └─ LoadingData (worker opens DB/loads corpus) ─► Routing
                  ├─ no path, no roots ─► Welcome
                  ├─ no path, roots ────► Home
                  ├─ file path ─────────► Classify ─► Reading (Kb/Unindexed/Generic)
                  └─ page ref ──────────► Resolve ──► Reading
Main overlays
  ├─ Settings modal (General/Appearance/Roots/Collections/Aliases/Search/Data/About)
  ├─ Search overlay (scoped or global)
  ├─ Convert dialog
  └─ Conflict / confirm dialogs
Document
  ├─ Reading (Formatted or Raw)
  ├─ Editing (dirty, autosave active)
  └─ Converting (banner + dialog; unindexed/generic only)
```

- Selection, view mode, and collection scope persist across navigation within a
  session.
- Back stack: `Read → Read (followed link) → Back` returns to the previous
  page/scroll.
- Search return address preserves the query and highlighted result.
- Home remains reachable via the app menu / `Esc` from an empty list.
- The app restores the last opened page only if a root/page still exists
  (optional, G6 preference).

---

## 11. Theming and preferences

### 11.1 Theme model

A theme is a set of **semantic tokens**, not a stylesheet. It covers both the
chrome and the Markdown content so a theme switch is a complete visual switch.

```text
Theme {
  name, appearance (dark|light),
  colors: {
    bg, bg_raised, bg_sunken, bg_hover, bg_active,
    text, text_muted, text_faint,
    border, border_strong,
    accent, accent_text, selection, focus_ring,
    rail_bg, list_bg, editor_bg, panel_bg,
    status_ok, status_warn, status_error,
    gated, ungated, dirty, conflict,
  },
  markdown: {
    h1..h6, body, bold, italic, code, code_bg, quote, quote_bar,
    link, link_hover, list_marker, table_header, table_grid, rule,
    syntax errors,
  },
  metrics: { density, corner_radius, row_height, content_max_width },
  fonts: { ui_family, mono_family, ui_size, mono_size,
           ui_file?, mono_file? },
}
```

- Built-ins: **Marknote Dark** (default), **Marknote Light**, and a
  **High Contrast** (colorblind-safe) theme, compiled in via `include_str!`.
- **System** follows the OS light/dark preference until the user explicitly
  picks a theme. Detection is cached/non-blocking: frame 1 uses the last-known
  appearance, then corrects if the OS disagrees.
- Every token maps to `egui::Visuals`/`Style` where possible; Markdown tokens
  feed `LayoutJob` styles in `md/style.rs`.
- Contrast is checked in tests against WCAG AA for body/UI text.

### 11.2 Custom themes

- TOML files at `<config>/kb-gui/themes/*.toml`, one theme per file.
- Schema mirrors `Theme` above; `deny_unknown_fields` with clear errors.
- Invalid/missing tokens fall back to the nearest built-in and show a
  non-blocking error; a bad theme must never prevent launch.
- The theme TOML is bounded and may be read before frame 1 if selected;
  otherwise it loads on selection. Font files are never read before frame 1.
- “Open themes folder” and “Reload” live in Settings → Appearance.
- CLI: `--theme <name|path>` selects a built-in or a TOML file for that run.
- **No TUI palette import.** The ratatui palette was designed for the TUI;
  the GUI keeps its own richer semantic token schema and does not consume it.

### 11.3 Theme switching and preferences

- Settings → Appearance shows theme swatches with a live preview and applies
  immediately, no restart.
- Theme, window size, pane sizes, last collection/page, autosave delay, default
  view, privacy mode, and restore-last-page persist in the platform config
  directory (using `dirs`).
- Preferences save debounced and off the first-frame path.
- Themes include fonts: custom `ui_file`/`mono_file` and family names are part
  of the theme. Font files load **asynchronously after frame 1**; frame 1 uses
  the built-in fonts, then swaps and repaints when ready. Missing/broken font
  files fall back without blocking launch. Font loading never counts against
  the 500 ms budget.
- Switching themes also reloads fonts asynchronously; UI state and scroll
  position are preserved.

---

## 12. Milestones

Each milestone has a demoable outcome and tests.

### G0 — Foundation
- Add the `kb-gui` crate, workspace `default-members` preserved; duplicate the
  data layer from CLI/TUI under `kb-gui/src/data` (no `kb-shell`).
- eframe window, built-in Dark/Light/System themes, three-pane shell with
  skeleton content; first frame has no DB/corpus/page-content work.
- DB worker thread + `Cmd`/`Evt` channels, spawned from the first `update`
  after the empty layout; open default/`--db` database asynchronously.
- Launch routing skeleton: argv path/ref, no-path detection, classify-file API.
- `--startup-report` / `KB_GUI_TRACE_STARTUP` first-frame timer.
- **Accept:** `cargo run -p kb-gui` paints empty chrome in **<500 ms p95** on
  the reference-class validation machine; launching with a path shows the shell
  first, then routes to the editor; `kb-hardened` dependency scan unchanged.

### G1 — All pages + collections + search parity
- `All pages` list with exact `kb browse` parity (gated included).
- Collection rail derived from `page_gates`, plus `Gated` / `Ungated`; counts.
- Collection selection filters the list and scopes search.
- **Home** (no path): search field + Open existing (in-app page search) /
  Create new / Open file (native picker) actions + recent pages.
- **Welcome** (no path, no roots): short tutorial, primary Add folder as root,
  Settings link, and fuss-free root addition.
- Read-only formatted renderer for all basic blocks (headings, paragraphs,
  lists, quotes, code, hr, links).
- Outline panel; full search overlay with scope chip and strict hidden-gate
  guidance (no include-gated toggle).
- Group-by control (`None · Collection · Visibility`); collection grouping
  duplicates multi-gate rows and keeps gateless pages under `Ungated`.
- **Accept:** TUI Browse/Read/Search behavior is reproduced (same rows, same
  search results, same outline); `All pages` shows every indexed page; scoped
  search returns the same page as the equivalent gate phrase in a global query;
  no-path/no-roots launch shows Welcome and adding a root reaches Home.

### G2 — Raw editor + save + frontmatter
- Raw Markdown editor with line numbers.
- Save/autosave, conflict guard, atomic write, single-page reindex.
- Fast frontmatter inspector: Type/Title/Description/Language/Tags/Collections
  visible together, inline validation, one-line pending-change summary; a type
  change warns when it deindexes but never deletes the file.
- `Tab` autocomplete for language/tags/collections, chip add/remove/rename,
  case-insensitive dedupe, free-form tokens.
- New note dialog with optional collection terms; uses the same widgets.
- **Accept:** create/edit/save round-trips; DB row updated; conflict detected
  when the file changes externally; `language` preserved after save; adding or
  removing a gate term updates the page's collection membership; `Tab` accepts
  a suggestion when the popup is open and advances focus when it is closed; a
  multi-field edit is one atomic write.

### G3 — Formatted read view + generic Markdown
- `pulldown-cmark` → IR with spans; full formatted renderer.
- Local images, tables, task lists; HTML shown as literal text.
- **Classify file** and disable `kb` features for
  `GenericMarkdown`/`UnindexedMarkdown`.
- Conversion banner with Convert/Dismiss; conversion dialog with type/title/
  tags/collections and frontmatter diff preview; add-root-and-convert path.
- **Accept:** reference screenshot can be reproduced with a fixture note; no
  webview dependency in `cargo tree -p kb-gui`; opening a plain `.md` shows the
  banner and disables links/collections/search membership; conversion produces
  a valid indexed page, preserves the body, and clears the banner.

### G4 — WYSIWYG block editing (MVP)
- Block editor for headings, paragraphs, list items, quotes, code.
- Top-right Formatted/Raw toggle with state preservation.
- Span-splice commits; debounced autosave; undo/redo per block session.
- Inline emphasis/code markers hidden via a custom rich-text widget; Raw view
  is the escape hatch. Toolbar/shortcuts for formatting.
- **Accept:** edit a note entirely in Formatted view, save, and verify byte-level
  round-trip when unchanged; editing one block leaves all other bytes identical;
  inline markers are invisible but Raw restores them and cursor/selection/copy
  map to the exact source.

### G5 — Settings and corpus management parity
- Settings modal (revisit a separate window later if needed) with General /
  Appearance / Roots / Collections / Tag aliases / Search / Data / About tabs.
- Add/drop roots, per-root refresh, refresh/full rebuild with progress, DB stats
  and cron line; roots are registration, not rail grouping.
- Collection management: rename a gate term across its pages, detach pages,
  bulk-add a collection term; dry-run summary before writing.
- Alias manager; tag browser; link picker and link navigation; back stack.
- Custom theme files, theme picker/live preview, async font loading.
- Command palette (`Cmd/Ctrl+P`) for app actions.
- **Accept:** every TUI manager operation has a GUI equivalent; collection
  operations leave `All pages` counts unchanged except for membership edits;
  root add/remove and alias changes are reflected without restarting; the main
  window is never blocked during refresh/full rebuild.

### G6 — Polish
- Polish hidden-delimiter editing: cursor entry/exit, selection across spans,
  copy/paste, and undo behavior in the custom inline widget.
- Syntax highlighting for code blocks (`syntect` optional), theme-aware.
- High Contrast/colorblind theme, contrast audit.
- Images drag/drop/copy into the note folder; link autocomplete.
- Accessibility pass, IME/CJK/bidi checks.
- Restore last page; preferences persistence.

### G7 — Packaging and release
- CI builds: Linux (glibc/musl? egui/winit need system libs; document them),
  macOS (ARM/Intel), Windows.
- Release artifacts alongside the CLI artefacts; checksum and install docs.
- `README` GUI section; `HARDENED_BUILD.md` verification updated.
- `cargo install --path kb-gui` instructions.

---

## 13. Testing

### Unit
- `md::parse`: spans and block classification over fixtures.
- `edit::map`: span replacement isolation; no-op identity.
- `md::style`: inline range styling (bold/italic/code/link).
- `data::write`: conflict detection, atomic write, reindex argument correctness.
- `data::autocomplete`: prefix/case-insensitive filtering, dedupe, free-form
  fallback, empty-corpus totality.
- `frontmatter`: chip add/remove/rename ops, multi-field batch ordering,
  no-op detection, description limit, type/language validation.
- `data::convert`: classification (Kb/Unindexed/Generic) over fixtures with
  missing/unknown/known `type:`, and body-preserving frontmatter construction.
- `ui::settings`: root/alias table state transitions and destructive-action
  guards (pure over snapshots).
- `data::theme`: built-in token completeness, TOML round-trip, unknown/missing
  field errors, fallback to a built-in, contrast ratios for body/UI text,
  font metadata parsing and missing-font fallback.
- Startup budget helper: `StartupReport` records each phase and flags a breach
  of the 500 ms first-frame target.

### Integration
- Temp file + temp DB (same pattern as TUI tests): open → edit → save →
  reopen; assert file bytes, DB row, tags, `language`.
- `All pages` count and rows match `kb browse` (including gated pages).
- Collection facets match the distinct `page_gates.term` values; multi-gate
  pages count in each collection and show membership chips. `Group by:
  Collection` emits one row per membership (duplicates by design), with
  gateless pages under `Ungated`; `All pages` still shows each page once.
- Scoped search parity: `energy` inside `Electronics` returns the same page as
  `energy electronics` globally; an unscoped search keeps the CLI gate rules.
- Gate edits (add/remove/rename) update collection membership and counts.
- Search results match CLI `kb search --json` for the same query corpus.
- Outline matches `kb toc`.
- Root add/drop and refresh match CLI behavior.
- Launch routing: no path + no roots → Welcome; no path + roots → Home; path →
  classified editor; ambiguous ref → picker; directory → add-root prompt.
- Welcome onboarding: adding a root indexes pages, reaches Home, and removal in
  Settings leaves files untouched.
- Frontmatter fast path: `Tab` accepts/advances correctly, chips dedupe
  case-insensitively, and a multi-field commit writes once.
- Type change: setting `type` to an unknown/missing value warns, deindexes the
  page, and leaves the file bytes on disk; setting a valid type again reindexes
  it without data loss.
- Generic Markdown: missing/unknown type opens with the banner, `kb` features
  disabled, body preserved; conversion adds only frontmatter, indexes, and
  clears the banner; dismiss is session-only and the menu action still works.
- Unindexed valid-type file: “add containing folder as root” indexes it without
  altering the body.
- Settings: add/remove root, alias upsert/delete, per-root refresh and full
  rebuild update the UI via worker events without restart.
- Theme: switch built-in themes live without losing editor/list state; load a
  custom TOML via Settings and via `--theme`; invalid custom file falls back
  and still launches; system preference is followed until overridden; custom
  fonts load after frame 1 and missing font files fall back cleanly.
- Startup: `--startup-report` on an empty DB/no roots prints first-frame time
  under the budget; launching with a large corpus still paints the shell first;
  a path argument does not delay frame 1.

### Golden / visual
- Fixture Markdown → IR snapshot.
- Headless egui tests (`egui_kittest` or `egui::Context` harness) for layout
  and interaction, including the Formatted/Raw toggle.
- Screenshot comparison against the Marknote-derived fixture (tolerant, not
  pixel-exact).

### Property / fuzz
- For random valid Markdown: parse → reconstruct source == input.
- For random block edit: bytes outside the edited span are unchanged.
- No panic on malformed Markdown, empty files, huge lines, invalid UTF-8
  (read as lossy, like `build_page`).

### Manual matrix
- Keyboard-only use; screen-reader basics; 100%/150%/200% scaling.
- CJK/IME input; RTL text; very long lines; large corpora (10k+ notes).
- First-run with an empty DB; add root from Welcome; remove root in Settings.
- Startup timing on cold/warm cache, empty DB, large corpus, and with a custom
  theme; verify <500 ms p95 on a reference-class machine.
- All built-in themes, system light/dark following, and an invalid custom theme
  (must still launch).
- Open-with a plain `.md`, a valid page `.md`, a directory, and an ambiguous
  page ref; convert a plain file and undo the conversion before the next save.
- Linux X11/Wayland, macOS, Windows.

### Startup measurement
- **Reference class** is the only pinned constraint: a multicore CPU with
  **≥4 cores** and an **SSD**. Exact CPU model, RAM, OS, and compositor are not
  pinned.
- Each run records the actual environment in `PERF.md` (CPU, core count, RAM,
  OS, compositor, build profile, cold/warm cache) so timings are falsifiable.
- Measure 10 cold launches (new process, empty DB/no roots) on at least one
  machine in the class used for release validation; report p95 and max. Also
  record a large-corpus launch and a custom-theme launch.
- `--startup-report` prints `window`, `theme`, `first_frame`, and `total`; the
  first frame is timestamped at the end of the first `App::update` with a
  one-shot flag. Compositor presentation is typically one vsync later and is
  covered by the 150 ms slack.
- Release gate: p95 <500 ms on the validation machine, which must meet the
  reference class. Shared CI runners use a documented 2× allowance and log
  trends; they are not the release gate.

### CI gates
- `cargo fmt --check`, `cargo clippy -p kb-gui -- -D warnings`.
- `cargo test -p kb-core -p kb-db -p kb-user -p kb-gui` (headless).
- `cargo build -p kb-hardened` + dependency-tree assertion: no egui/winit.
- `cargo tree -p kb-gui` assertion: no `wry`/`webview`/`webkit`.
- Startup smoke: `--startup-report` on shared CI with a documented 2×
  allowance, plus a trend log. The strict p95 <500 ms gate runs on the
  reference-class validation machine before releases.

---

## 14. Risks and mitigations

| Risk | Mitigation |
|---|---|
| True WYSIWYG cursor/source mapping is hard | Source-spanned blocks + span splicing; custom inline widget for hidden delimiters; raw mode always available; cursor/selection/IME tests early; property tests |
| egui text input limitations (IME, bidi, a11y) | Use stock `TextEdit` per block; test IME/CJK early in G4; document known gaps; contribute fixes upstream if needed |
| Heavy dependency/offline builds | Pin exact versions; vendor or cache egui; keep `kb-gui` out of default members and hardened builds |
| Turso `Connection` not `Send` | DB worker thread owns it; channels only across the boundary |
| Big files / big corpora freeze the UI | Virtualize lists, lazy block layout, debounce parsing/autosave, worker thread for DB |
| 500 ms cold-start target is missed | First-frame rule (no DB/corpus/page-content/font/image work; only bounded prefs/theme reads), worker loads data after paint, skeleton UI, `--startup-report` gate, pin/lazy egui assets, measure every milestone |
| Startup regressions creep in | Startup probe in CI/reference runs; treat first-frame path as a reviewed boundary; no eager DB/corpus calls in `main`/`App::new` |
| Themes undermine readability | Semantic token schema, contrast tests, High Contrast built-in, preview before apply, validated fallbacks |
| Custom theme files break launch or crash | Strict TOML validation, unknown-field errors, fallback to built-in, test malformed/missing files, never panic on bad input |
| Theme/startup budgets depend on hardware | Only pin the class (≥4 cores + SSD); record actual hardware/OS per run in `PERF.md`; use p95 + max, cold/warm, and large-corpus numbers; treat >500 ms p95 on the validation machine as a release blocker |
| Conflict/clobber bugs | Mandatory on-disk comparison before write; never silent overwrite; explicit conflict dialog; TUI-parity tests |
| Feature creep toward Obsidian | The non-goals list is enforced per milestone; raw mode is the escape hatch |
| Divergence between TUI/GUI query behavior | Duplication accepted; port exact queries, keep copies close to upstream, and run parity tests for browse/search/save/roots/aliases; review diffs when CLI/TUI behavior changes |
| Collections couple navigation to search gates | Make the scope chip explicit; keep `All pages` as browse parity; show strict hidden-matches guidance with the gate term to add; keep a “Search gates” help page; document that gate edits change both membership and search visibility |
| Gate terms are sparse or inconsistently spelled | Provide an `Ungated` bucket; normalize case for grouping; add collection management (rename/detach) and optional `gate_aliases`; never require every page to be gated |
| Generic-file conversion corrupts or clobbers content | Frontmatter-only rewrite with a diff preview; preserve body and unrelated fields; atomic write + disk guard; undo until next save; conversion tests over malformed/minimal fixtures |
| `Tab` autocomplete conflicts with field navigation | Context-sensitive rule: popup open → accept suggestion, popup closed → advance focus; show the rule in the panel footer; test both paths; `Shift+Tab` always navigates |
| Settings becomes a second overwhelming app | One Settings modal with eight tabs (separate window revisited later if needed); destructive actions behind dry-run/confirmation; no admin controls in the main chrome; keyboard-reachable |
| First-run onboarding dead-ends (empty/permission-denied folder) | Non-blocking notice with “Choose another folder”; root addition is cancellable; opening a file is always available without a root |
| Generic files silently missing from `All pages` | Persistent transient-tab badge (`Generic` / `Not indexed`); banner explains why; conversion or add-root action always reachable from the `⋯` menu |

---

## 15. Decisions

### Adopted

1. **Collections are `search_required_phrase` gate terms.** They are field/topic
   scopes, not roots or folders. “Energy” in electronics vs signal processing vs
   physics is the anchoring example.
2. **`All pages` is pinned and shows every indexed page.** Grouping is
   presentation; `All` is always browse parity.
3. **Selecting a collection scopes search.** The scope satisfies the gate
   predicate, so searching inside a collection does not silently hide its
   pages. Unscoped search retains the CLI/TUI gate rules.
4. **Membership is many-to-many.** A multi-gate page belongs to each collection;
   normal lists show it once with membership chips. `Group by: Collection`
   duplicates it under every gate; there is no `page_gates`-order “primary”.
   Gateless pages stay under `Ungated`.
5. **No new collection schema.** Collections are derived read-side from
   `page_gates`; editing membership is a frontmatter edit.
6. **Frontmatter is a fast, visible property panel.** All known fields at once;
   `Tab` accepts an open suggestion or advances focus; chips for tags and
   collections; multi-field commits are one atomic write.
7. **Admin lives in one Settings modal behind the `☰` menu**, with General,
   Appearance, roots, collections, aliases, search, data, and about tabs. The
   main chrome stays quiet. A separate window is deferred to “revisit later”;
   settings state stays in a shared service so that change is safe.
8. **No-path launch shows Home**, and a first run with no roots shows Welcome
   with a one-click “Add folder as root”. A file path always opens immediately.
9. **Generic Markdown is first-class.** It opens with a non-blocking conversion
   banner; `kb`-specific features stay disabled until converted or indexed.
10. **Themes are semantic token sets**, covering chrome and Markdown styles.
    Built-ins are compiled in; custom themes are TOML files; switching is live
    from Settings → Appearance.
11. **First frame is the performance contract.** Process launch → painted empty
    chrome is <500 ms p95 on a reference-class machine (≥4 cores + SSD); DB,
    corpus, page content, font, and image work happen after frame 1 behind
    skeletons. Only bounded preferences/theme reads are allowed before frame 1.
    The strict p95 gate runs on a reference-class release-validation machine;
    shared CI uses a 2× smoke allowance.
12. **Standalone `kb-gui` binary.** The CLI/TUI stay unaware of terminal vs
    desktop-environment detection; no `kb gui` subcommand and no decision
    logic added to `kb`. Leave the TUI alone.
13. **Opening by intent.** Supplying an arbitrary path uses the native file
    dialog; selecting an indexed page uses the in-app page search/picker.
    They are separate actions, not one ambiguous “open”.
14. **Native titlebar only.** No custom centered-title chrome; portability
    across X11/Wayland/macOS/Windows is worth more than visual parity.
15. **Local-only images** in v1; no remote image fetching.
16. **Strict gate search; no `Include gated` toggle.** GUI unscoped search
    matches CLI/TUI gate semantics. Hidden gated matches show guidance to add
    the gate term; the escape hatch is an in-KB “Search gates” help page, not a
    GUI override.
17. **Conversion requires a root.** If the user chooses Convert for a file
    outside every root, the flow adds the containing folder as a root first,
    then converts and indexes. Opening a file never forces a root.
18. **Session-only banner dismissal.** Convert/Retry stays in the `⋯` menu;
    dismissal does not persist across launches.
19. **No TUI palette import.** The ratatui palette was designed for the TUI;
    the GUI keeps an independent semantic token schema.
20. **Themes include fonts.** Custom themes may specify UI/mono families and
    font files; font files load asynchronously after frame 1 so they never
    threaten the startup budget.
21. **No primary collection; duplicate by gate.** `Group by: Collection` shows a
    page under every gate it has, with repeated rows by design. There is no
    first-term primary; gateless pages stay under `Ungated`.
22. **Hide inline markers.** Formatted view hides emphasis/code delimiters
    behind a custom rich-text widget; Raw view remains the escape hatch. No
    dimmed-marker intermediate state.
23. **Type and language in the inspector.** Both are first-class fields. Type
    changes are ordinary frontmatter edits: they can deindex, but never delete
    the file, so they are recoverable by fixing the type or finding the file
    under a root (grep/root scan).
24. **Duplicate the data layer; do not extract `kb-shell`.** The repo already
    duplicates CLI/TUI queries; `kb-gui/src/data` follows that pattern and is
    guarded by parity tests against the CLI/TUI.

### Open before G0

None — the decisions above unblock G0.

### Revisit later

1. **Settings as a separate window.** The modal ships first. Revisit if bulk
   root/alias/collection work feels cramped; keep settings reads/mutations and
   theme state in a shared service so a second window cannot race config writes.
2. **Visible inline markers on cursor entry.** Pure hiding ships in G4; if
   editing feels disorienting, evaluate cursor-reveal as a polish option, not a
   replacement for the Raw escape hatch.

---

## 16. Relationship to existing docs

- `PLAN.md` §2 (disk authoritative, DB cache), §9 (proven vs assumed),
  §10 (workspace layout) still govern. `kb-gui` is another *assumed shell*.
- `PLAN.md` §3/§4 and “The `search_required_phrase` gate” define the gate
  semantics. The GUI reuses them as collection scopes; it does not change the
  field, the file format, or the CLI/TUI default behavior.
- Roots remain corpus registration (one `pages.root`/`rel_path` assignment,
  innermost/most-recent wins, overlapping roots allowed). The GUI does not use
  roots for grouping because they are not a partition.
- `PLAN.md` §3 requires a known `type:` for a page to be indexed, and `kb
  create` already handles the no-roots case as a warning. The GUI mirrors that:
  generic files are openable but unindexed until converted, and conversion
  reuses the same `build_page`/`upsert_page` path.
- The TUI's frontmatter edit fields (`title`, `description`, `tags`,
  `search_required_phrase`) and Tab-autocomplete corpus
  (`corpus_for`/`suggestions_for`) are the behavior baseline; the GUI adds
  `type`/`language` and chip editors but keeps the same verified mutations.
- `HARDENED_BUILD.md` must be extended to assert `kb-gui` is not reachable from
  `kb-hardened`.
- `FEATURES-links.md` link semantics are reused unchanged.
- `QUICK_SEARCH.md` section addressing/Hints are consumed by the outline and
  link navigation rather than reimplemented.
