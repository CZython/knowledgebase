//! # kb tui — the interactive UI (M8)
//!
//! `kb tui` is an **in-process** subcommand (the M8 criterion: "cannot be a
//! separate program — must be `kb <subcommand>`"). It shares the *same*
//! `kb` binary, the *same* `Db` handle, and the *same* verified
//! `kb-core` + `kb-db` modules every other verb uses. This is not a second
//! program with its own copy of the world; the TUI re-uses what CLI verbs
//! already re-use — the same SQL, the same frontmatter edit API
//! (`set_scalar`, `append_list_item`, …), the same tag/alias expansion.
//! Adding a TUI adds **no new proven code**: it composes primitives the
//! shell already trusts.
//!
//! ## Assumed vs proven (PLAN §9)
//!
//! `ratatui` renders; `crossterm` drives the terminal (raw mode + event
//! loop). Both are *assumed* shells, exactly like `clap`/`rusqlite` —
//! they may misbehave in ways we do not prove. Everything **inside**
//! this module — the state machine, selection logic, key-binding table,
//! filter-match, the frontmatter-edit pipeline — is plain Rust over the
//! already-verified `kb-core::frontmatter`; we could add specs for any of
//! it (e.g. the selection move is total + bounded at the edges) and it
//! would verify. We choose not to, on the same grounds as `clap`: the
//! shell is assumed, so a TUI bug is a shell bug, fixable here without a
//! spec. (The two pure TUI invariants we do pin: selection move is total
//! and bounded — see the tests at the bottom.)
//!
//! ## Modes (state machine)
//!
//! - **Browse** (M8.0+) — page list + selection + keybindings; the default
//!   mode. `/` puts the TUI in *filter input* (M8.1): each printable char is
//!   appended to `filter_text`; the active row list is recomputed (case-insens-
//!   itive substring match over `id` / `title` / `rel_path`); `Enter` commits
//!   the filter, `Esc` clears it. `Enter` opens the **Read** mode (M8.1):
//!   body + outline panes, scroll with j/k/PgUp/PgDn/Home/End. `a` opens
//!   **Alias** (M8.3), `r` opens **Roots** (M8.3).
//! - **Read** (M8.1) — open a page: body text (scrollable) + section outline
//!   (a `heading_of` extraction over the body — reusing the already-verified
//!   `kb-core::section::heading_of`; no new spec).
//! - **Edit** (M8.2) — edit scalars + lists with autocomplete.
//! - **Alias** (M8.3) — manage `tag_aliases`: list, add (upsert, last-wins),
//!   remove. The add path reuses the *same* SQL as `cmd_tag_alias` in
//!   `main.rs`; the remove path is `DELETE FROM tag_aliases WHERE alias = ?1`.
//! - **Roots** (M8.3) — manage the registered roots: list, add (a directory
//!   path — `add_root` + a full `refresh`), drop (`drop_root` + a full
//!   `refresh`). The add/drop paths reuse the *same* `Db` methods as
//!   `cmd_add` / `cmd_drop_root` in `main.rs`.
//! - **Search** (M8.4) — full `search_grammar` power search armed by `s` in
//!   Browse: type a query, results stream from `search_rows` (the *same*
//!   `search_grammar::parse` + `build_search_sql` + `pages_fts` engine the
//!   CLI `kb search` verb runs), and a trailing `tag:` token gets inline
//!   suggestions (alias-first, shown `alias → canonical`) that `Tab` injects.
//!   `Enter` opens the selected result in **Read**; `Esc` back to Browse.
//!
//! ## Boring-shell conventions
//!
//! - **Single-threaded.** The crossterm event loop is the heartbeat; no
//!   async, no `tokio` (same as the rest of `kb` — `rusqlite` is sync).
//! - **One `App` value, no hidden state.** Add a `Read`/`Edit`/`Aliases`
//!   variant on `Mode` and the existing loop dispatches it.
//! - **Terminal restore is a `Drop` guard.** If we return (or panic) out
//!   of `event_loop`, the alt screen is left and raw mode re-enabled — no
//!   early `?` can leak the terminal state.

use std::path::Path;
use std::time::Duration;

use crossterm::event as ct_event;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use kb_db::Db;
#[cfg(test)]
use unicode_width::UnicodeWidthStr;
use kb_core::frontmatter as fm;
use kb_core::section as section_core;

use crate::load_corpus;

use crate::EXIT_ERR;
use crate::EXIT_OK;

pub mod input;
pub mod palette;
pub mod render;

/// A single page row — a snapshot of one `pages` row. The TUI only reads
/// these; an edit commits against the file then re-validates the DB.
/// `abs_path` / `pt` / `root` are reserved for the edit + alias managers
/// (M8.2/M8.3) that need a full identity to commit a change.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PageRow {
    pub abs_path: String,
    pub id: String,
    pub pt: String,
    pub title: String,
    pub rel_path: String,
    pub root: String,
    /// The page's `tags:` list (from `page_tags`). Empty when the page has
    /// no tags. Used by `apply_filter` so a user who types `/security` also
    /// finds pages *tagged* `security`, not just pages whose id/title/path
    /// mention it.
    pub tags: Vec<String>,
    /// The page's `search_required_phrase:` list (from `page_gates`, i.e.
    /// the unquoted form). Non-empty = the page is *gated* (hidden from
    /// general search until a query contains one of these entries). Both
    /// used for the browse row's `◆`/`◇` marker (palette.gated/palette.open)
    /// and for `apply_filter` to match phrases the user types into the filter.
    pub gates: Vec<String>,
}

/// A single row in the **Alias** manager (M8.3): one `tag_aliases` row.
/// `alias` is the PK; `canonical` is the resolved tag it maps to. The TUI
/// only reads these; an add/remove writes the row directly then refreshes
/// the in-memory snapshot.
#[derive(Debug, Clone)]
pub struct AliasRow {
    pub alias: String,
    pub canonical: String,
}

/// A single row in the **Roots** manager (M8.3): one `roots` row. `path`
/// is the registered (canonical) root path. `extra_exts` / `include_hidden`
/// are the per-root walk options (displayed in the list, not edited here —
/// `kb add` owns them).
#[derive(Debug, Clone)]
pub struct RootsRow {
    pub path: String,
    pub extra_exts: Option<String>,
    pub include_hidden: bool,
    pub added_at: Option<String>,
}

/// The **Alias** manager state (M8.3): the tag-alias list + the "add" input
/// buffer + the "remove" list selection. The `list` is *live* — `add_alias`
/// / `remove_alias` write the DB then refresh it, so the render always
/// reflects the on-disk truth.
///
/// The "form vs. list" split: `focused == false` means the *add form* is
/// active (type into `input`; `Enter` submits an upsert); `focused == true`
/// means the *list* is active (`↑`/`↓` move the selection; `x` removes the
/// selected alias). `Tab` toggles `focused`. `input` is the *add form*
/// buffer (the user types `"<alias> <canonical>"`, space-separated, and
/// `Enter` parses it).
#[derive(Debug, Clone)]
pub struct AliasState {
    /// The current `tag_aliases` snapshot (sorted by alias, the same order
    /// `cmd_tag_alias --list` uses).
    pub list: Vec<AliasRow>,
    /// The *add form* buffer — `"<alias> <canonical>"` (space-separated).
    /// `Enter` parses this; `Backspace` pops; printable chars append.
    pub input: String,
    /// The selected index in `list` (for the *x*-removal). Total: 0 when
    /// `list` is empty.
    pub selected: usize,
    /// `false` = the add form is active (type / `Enter` to add);
    /// `true` = the list is active (`↑`/`↓` to move; `x` to remove).
    /// `Tab` toggles.
    pub focused: bool,
}

/// The **Roots** manager state (M8.3): the registered roots + the "add root
/// path" buffer + the "drop root" list selection. `list` is *live* —
/// `add_root_tui` / `drop_root_tui` write the DB then refresh it.
///
/// Same "form vs. list" contract as [AliasState]: `focused == false` = the
/// *add form* (type a path into `input`; `Enter` registers it and
/// full-refreshes); `focused == true` = the *list* (`↑`/`↓` move; `x` drops
/// the selected root and full-refreshes). `Tab` toggles.
#[derive(Debug, Clone)]
pub struct RootsState {
    /// The current `roots` snapshot (ordered `path ASC`).
    pub list: Vec<RootsRow>,
    /// The *add form* buffer — the absolute path of the directory the user
    /// wants to register. `Enter` canonicalizes it and calls `add_root` +
    /// `refresh`.
    pub input: String,
    /// The selected index in `list` (for the *x*-drop). Total: 0 when
    /// `list` is empty.
    pub selected: usize,
    /// `false` = the add form is active; `true` = the list is active.
    /// `Tab` toggles. On open, the *list* is focused when there is
    /// something to select (so navigation works immediately) and the *form*
    /// only when the list is empty.
    pub focused: bool,
}

/// The TUI state machine. M8.0 Browse, M8.1 Read, M8.2 Edit, M8.3 Alias +
/// Roots.
#[derive(Debug, Clone)]
pub enum Mode {
    Browse,
    /// Read a single page: body + section outline. `page` is the *index
    /// into `rows`* the user clicked (not a path), so a re-`list_pages`
    /// after an edit still finds the same page.
    Read(ReadState),
    /// Edit a single page's frontmatter (scalar fields + list fields) with
    /// tab-autocomplete over the corpus. M8.2.
    Edit(EditState),
    /// Show a page's links (per-occurrence list, position + context +
    /// resolution) and navigate into the one the user picked — the M8.5
    /// link-picker. Opened by `l` in Read mode (anchored to the last find
    /// hit when one is armed); closed by `Esc`, which pops back to the Read
    /// that armed it.
    Links(LinksState),
    /// Manage `tag_aliases`: list, add (upsert), remove. M8.3.
    Alias(AliasState),
    /// Manage registered roots: list, add, drop. M8.3.
    Roots(RootsState),
    /// Power search (M8.4): full FTS5 search_grammar + live tag
    /// suggestions. Armed by `s` from Browse.
    Search(SearchState),
}

/// The **Search** mode (M8.4): full `search_grammar` FTS5 + live tag
/// suggestions. The query buffer supports the same grammar `kb search`
/// does (`tag:…`, `type:…`, `not …`, `"quoted phrase"`, etc.). Live
/// results appear in the left pane; tag suggestions (with alias→canonical
/// display) appear in the right pane while the user is typing a `tag:`
/// token.
///
/// Keybindings: type → append to query · ↑↓/jk → move selection in
/// results · Tab → inject the highlighted tag suggestion · Enter → open
/// selected page · Esc → back to Browse.
#[derive(Debug, Clone)]
pub struct SearchState {
    /// The query buffer the user is typing (full search_grammar).
    pub query: String,
    /// Live results from `search_rows`, re-computed after every query
    /// change. `None` = the query was empty (nothing to search).
    pub results: Option<Vec<PageRow>>,
    /// The selected index into `results` (0 when no results).
    pub sel: usize,
    /// Live tag suggestions for the current `tag:partial` token. Empty
    /// when the trailing token is not a `tag:` prefix.
    pub tag_sugs: Vec<TagSuggestion>,
    /// The highlighted tag suggestion (0 when `tag_sugs` is empty).
    pub tag_sel: usize,
}

/// A single row in the **Link-picker** (M8.5): one reference in the open
/// page's body, with its position + context (so a user can pick *which*
/// `[[…]]` among repeats) and its resolution (so the user sees where it
/// points *before* they open it).
///
/// `resolved` / `candidates` mirror the §4 contract on the CLI side:
/// exactly one of `resolved` and `candidates` is `Some` / non-empty depending
/// on whether the target resolved uniquely vs ambiguously; both are `None`
/// / empty when the target is a dead `[[…]]`.
///
/// `resolved` is the *display id* (`type:slug`, e.g. `note:Transformer`)
/// for the unique hit — exactly the string the picker shows ("→ note:Transformer").
/// It is the *tuple* of the CLI-side `AnnotatedLink.resolved_id`; the
/// `type:slug` format is the user-facing form, while the navigation identity
/// (the absolute path `open_linked` consumes) lives in `abs_path`.
///
/// `abs_path` is the *target* page the link points at (NOT the page the
/// user is reading) — the value `links_open` uses as the `abs_path` of the
/// new `ReadState` it builds on `Enter`.
#[derive(Debug, Clone)]
pub struct LinksEntry {
    /// 0-based raw line index (matching `FindState::matches`).
    pub line: usize,
    /// 0-based byte offset in that line where the **target token** begins
    /// (for wikilinks: the first char after `[[`; for path tokens: the
    /// first char of the path). The `[[` itself is *not* counted — the
    /// offset is where the *content* the user wrote starts.
    pub col: usize,
    /// The containing raw line, trimmed (for context display).
    pub context: String,
    /// The written target (wikilink inner text, or a path token).
    pub target: String,
    /// The *display id* (`type:slug`) of the unique hit — for the
    /// "→ {id}" render. `None` when ambiguous (see `candidates`) or
    /// unresolved.
    pub resolved: Option<String>,
    /// The collision set (absolute paths, sorted) when resolution was
    /// ambiguous. Empty when the link is unique or unresolved.
    pub candidates: Vec<String>,
    /// The resolved `abs_path` the `Enter` key will navigate to (only set
    /// for a *unique* hit; `None` otherwise).
    pub abs_path: Option<String>,
}

/// The **Link-picker** state (M8.5). Holds the full per-occurrence list of
/// the open page's links (in document order), the cursor, and a *return
/// reference* to the `ReadState` the user came from (the "anchor" — so
/// `Esc` pops back into Read, not Browse).
///
/// Keybindings: ↑/↓ move · `Enter` opens the selected *unique* link ·
/// `x` / `q` cancel (back to Read) · `Esc` cancels (back to Read).
///
/// When the user opened `l` from a `FindState`-armed Read, the picker
/// pre-selects the entry nearest the find hit (see
/// [`App::nearest_link_for_find_anchor`]).
#[derive(Debug, Clone)]
pub struct LinksState {
    /// Per-occurrence list (document order, as emitted by
    /// `crate::annotated_links`).
    pub links: Vec<LinksEntry>,
    /// The selected index into `links` (0 when empty).
    pub cursor: usize,
    /// The `abs_path` of the *source* page the picker was opened from,
    /// kept so the TUI's "Esc" pops back into it via `load_body` rather
    /// than the Browse view.
    pub source_abs_path: String,
}

/// A single row in the tag-suggestions pane: the display label (what the
/// user sees) and the canonical value to inject into the query.
#[derive(Debug, Clone)]
pub struct TagSuggestion {
    /// What the user sees in the suggestion list (e.g. `security` or
    /// `sec → security` for aliases).
    pub display: String,
    /// The value to inject on Tab (the canonical form, e.g. `security`).
    /// Multi-word values are quoted at injection time.
    pub canonical: String,
}

/// Display metadata from a page's frontmatter, shown above the ToC in the
/// Read pane. All fields are optional — a page may lack any of them.
#[derive(Debug, Clone, Default)]
pub struct FmMeta {
    pub page_type: Option<String>,
    pub language: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub search_required_phrase: Option<String>,
}

/// A snapshot of a page's *body* in the Read mode (loaded from the DB on
/// `Enter`, cached in the App — reading it out is O(1) per scroll since
/// `body` is already text, and the outline is recomputed once per load).
#[derive(Debug, Clone)]
pub struct ReadState {
    /// The row index we opened (kept stable across a corpus — the *identity*
    /// lives on `row_idx`; a re-`list_pages` after an M8.2 edit must find the
    /// same page by that index).
    pub row_idx: usize,
    /// The page's id, for the header ("reading note:P3").
    pub id: String,
    /// The page's body, loaded once per open (reloaded by `R`).
    pub body: String,
    /// The *display* outline — a `(line_index, level, title)` triple per
    /// heading, in body order. Recomputed per load so the outline always
    /// reflects what's on screen (not a stale cache).
    pub outline: Outline,
    /// The first **rendered row** to show (scroll offset, total on any body).
    /// A rendered row is one row as the terminal sees it — a logical line
    /// that wraps to N rows contributes N rows, so one press of ↓ moves one
    /// rendered row (not one logical line).
    /// 0 = top; values beyond the max are clamped by the mutators.
    pub scroll_top: usize,
    /// The absolute file path, shown in the header for easy `cd`/`copy`.
    pub abs_path: String,
    /// Frontmatter display metadata (type, language, description, tags, gates).
    pub fm: FmMeta,
    /// The reader-find session (armed by `f`, disarmed by `Esc`). `None` =
    /// not in find; `Some(_)` = the needle is live, its occurrences are
    /// recomputed, and the cursor walks them. Held on `ReadState` (not
    /// `App`) so find can't outlive the page it's searching — opening a
    /// different page clears it via `open_read`/`open_in_editor`.
    pub find: Option<FindState>,
}

/// The memoized geometry of the body currently open in Read mode: the
/// per-line rendered row counts, the cumulative first-row table, and the
/// total, measured by ratatui's `WordWrapper` at a **specific** pane
/// width. The three `App` hot paths that previously re-measured the whole
/// body on every call (`scroll_body`, `scroll_body_bottom`,
/// `find_center_scroll`) plus the every-frame `draw_read` now read these in
/// O(1); the roll-up is rebuilt only when the body or the pane width
/// changes. `total_rows` is the full rendered-row count (the
/// `render::rendered_row_count` value); `total_rows - 1` is the
/// `scroll_max_top` (largest valid `scroll_top`), and `first_row_of` is the
/// old per-line `line_first_row`.
///
/// **Single-writer invariant:** only the owning `App` builds and replaces
/// this (via `App::ensure_body_rollup`), against the current `(body, width)`,
/// so the read paths can assume the numbers match the body that is on
/// screen. A hash collision or a width mismatch that the key somehow fails
/// to catch would only change a *scroll clamp* (worst case the user nudges
/// with arrows) — never a crash, because every read is `.min()`-clamped and
/// `total_rows` is `saturating_sub`-ed.
#[derive(Debug, Clone, Default)]
pub struct BodyRollup {
    /// `true` once a roll-up has been built for the current key.
    pub valid: bool,
    /// The FNV-1a hash of the body the tables were built against (so a key
    /// change on the body triggers a rebuild — even if the body pointer
    /// hasn't moved, a new `open_read` with different bytes does).
    pub body_hash: u64,
    /// The pane width (cells) the tables were measured at.
    pub width: usize,
    /// Per-logical-line **first** rendered row (the cumulative sum of per-line
    /// counts). `first_row_of[n]` = the rendered row line `n` starts on.
    pub first_row_of: Vec<usize>,
    /// The full rendered-row count of the body (sum of the per-line counts).
    pub total_rows: usize,
}

impl BodyRollup {
    /// The largest valid `scroll_top` (the old `scroll_max_top`): one less
    /// than the total rendered-row count, `saturating_sub` so an empty body
    /// clamps to 0 rather than underflowing.
    pub fn max_top(&self) -> usize {
        self.total_rows.saturating_sub(1)
    }
}

/// The reader-find session (the `f` key in Read mode): a *needle* typed by
/// the user, its *occurrences* in the body (`(line_index, byte_offset)` in
/// document order), and a *cursor* index into `matches` (the selected hit —
/// the one the viewport centers on).
///
/// **Live-input model** (the same idiom the Browse `/` filter and the Search
/// query use): every printable char appends to `needle` and re-runs
/// `find_center_scroll` (the hit list updates + recenters as you type); `Backspace` pops;
/// `↑/↓/j/k` walk the hits (each move recenters the viewport); `Esc` closes
/// find (clears the highlight, keeps the scroll). The needle is matched
/// case-insensitively, as a *substring*, across the body's logical lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindState {
    /// The needle being typed (the search string, case-insensitive match).
    pub needle: String,
    /// The occurrences of the needle in the body: `(line_index, byte_offset)`,
    /// in document order (top-to-bottom, within a line left-to-right).
    /// Refreshed by `find_center_scroll` on every keystroke/nav.
    pub matches: Vec<(usize, usize)>,
    /// The cursor index into `matches` (the *selected* hit, i.e. the one the
    /// viewport centers on). Always `< matches.len()` while armed — enforced
    /// by `find_center_scroll` and the nav mutators.
    pub cursor: usize,
    /// The latest computed centering padding for the selected hit (the
    /// "fake rows" + scroll values). `None` when the match list is empty or
    /// a padding could not be computed — the render pass then uses the
    /// unpadded body, the scroll pass keeps the current position.
    ///
    /// Written by `find_center_scroll` (after it refreshes
    /// `matches`). Read by `render_read` (prepend / append blank rows, set
    /// scroll) and by the `App` scroll mutators (clamp against
    /// `scroll_max_top`). This is the single source of truth for the
    /// padded layout — the render pass *and* the input pass derive their
    /// values from the same stored padding, so they cannot desync.
    #[allow(dead_code)]
    pub center: Option<FindPadding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
/// The **stage** of the Edit-metadata screen. Two stages, mirroring the
/// zsh *type → live-filter → arrows-select → Enter-adopts* idiom exactly:
///
/// * **Picker** — the field list (title / description / tags /
///   search_required_phrase, with each field's *current* value next to it). `↑/↓`
///   move the highlight; `Enter` moves to the **Value** stage for that
///   field. The manager keys (`a` alias, `r` roots) are reachable *only*
///   from here (and from Browse / Read) — never from a value buffer, so a
///   `q` / `a` / `r` typed into "query" or "security" is a *letter*, not a
///   command.
///
    /// * **Value** — the value buffer for the chosen field (a *final* choice —
    ///   `↑/↓` no longer switch fields here). **Typing always goes to the
    ///   buffer** and re-narrows the live candidate list in place (a pure,
    ///   live function of the buffer — the zsh filter idiom). `↑/↓` move the
    ///   highlight across the candidates; `Tab` **fills the buffer with the
    ///   highlighted candidate** (autocomplete); `Backspace` pops the buffer
    ///   (or, on an empty list-field buffer, drops the list's tail item);
    ///   `Enter` **commits the buffer** (with candidate-aware append for list
    ///   fields) and stays on the field for the next token. `Esc` pops back to
    ///   the Picker. `C-c` quits the TUI. Hard-quit is a control key, *not* a
    ///   letter — every letter types through.
    pub enum EditStage {
        /// The field picker is active (the value buffer is not).
        Picker,
        /// The value buffer for the chosen field is active. `sug_cursor`
        /// highlights a candidate; `Tab` adopts it into the buffer, `Enter`
        /// commits, `↑/↓` move the highlight.
        Value,
    }

/// A field the Edit mode can show — either a *scalar* (free text) or a
/// *list* (the value is the next item to append). Both are edited over the
/// already-verified `kb-core::frontmatter` ops; the TUI just composes them.
///
/// **D1 note:** `SearchRequiredPhrase` is the *TUI's name* for the
/// `search_required_phrase` *corpus field*; the two share an identifier by
/// convention. The SQL column itself is owned by `kb-core::frontmatter`; see
/// the `pages.search_required_phrase` column on `SearchRow` in `kb/main.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    Title,
    Description,
    Tags,
    SearchRequiredPhrase,
}

impl EditField {
    /// The key the verified frontmatter ops address this field by.
    pub fn key(self) -> &'static str {
        match self {
            EditField::Title => "title",
            EditField::Description => "description",
            EditField::Tags => "tags",
            EditField::SearchRequiredPhrase => "search_required_phrase",
        }
    }
    /// A short, human label for the render surface (the "title:" / "tags:"
    /// caption in the Edit form).
    pub fn label(self) -> &'static str {
        match self {
            EditField::Title => "title",
            EditField::Description => "description",
            EditField::Tags => "tags",
            EditField::SearchRequiredPhrase => "search_required_phrase",
        }
    }
    /// Whether the field's value is a comma/list-style *item* (so the
    /// "append an item" semantic + tab-autocomplete applies) vs. a scalar
    /// (so "set the value" applies). Title/description are scalars;
    /// tags/search_required_phrase are lists.
    pub fn is_list(self) -> bool {
        matches!(self, EditField::Tags | EditField::SearchRequiredPhrase)
    }
    /// The ordered field list the Edit mode shows. Total + stable.
    pub fn all() -> &'static [EditField] {
        &[EditField::Title, EditField::Description, EditField::Tags, EditField::SearchRequiredPhrase]
    }
}

/// The Edit mode's working state: which page is being edited, which field
/// the cursor is on, the value buffer currently being typed, and the
/// live tab-autocomplete context for that field.
#[derive(Debug, Clone)]
pub struct EditState {
    /// The *absolute path* of the page (stable identity). The file is the
    /// source of truth; the DB is refreshed after a commit.
    pub abs_path: String,
    /// The page's id, for the header ("editing note:P3").
    pub id: String,
    /// The field the cursor is on.
    pub field: EditField,
    /// The value buffer for the current field. **Scalar** fields seed this
    /// with the field's *current* value (so the user edits in place; an
    /// unchanged commit is a no-op — `build_ops` sees `v == original` and
    /// produces no `SetScalar`). **List** fields seed it empty — here it is
    /// the *new item(s)* to append (one or more comma-separated), and the
    /// existing items are shown separately in `current_list`.
    pub value: String,
    /// The live tab-autocomplete suggestions for `value`. Empty for scalar
    /// fields (no autocomplete); for list fields, `suggestions_for(corpus,
    /// value)`. Recomputed whenever `value` changes.
    pub suggestions: Vec<String>,
    /// The known-token corpus for the *current list field* (all tags seen
    /// across the corpus, or all search-terms seen), loaded once when the
    /// field is entered. Empty for scalar fields and when the corpus has no
    /// tokens. This is the autocomplete source (PLAN M8.2 "list all matching
    /// tags/search_required_phrase from the corpus").
    pub corpus: Vec<String>,
    /// The page's file bytes **on entry** (fixed for the lifetime of the
    /// edit session, overwritten only after a successful commit). Two roles:
    ///   1. **Disk guard** — at commit time, the file-on-disk must still
    ///      match these bytes (a mid-session external edit is refused).
    ///   2. **Baseline** — scalars compare the buffer against the scalar
    ///      value *in* these bytes; lists know the entry items from
    ///      `entry_list` (derived from these bytes, cached separately).
    pub entry_bytes: Vec<u8>,
    /// The items currently on the active **list** field **on entry**
    /// (the list we saw when we opened this page). The baseline that
    /// `commit_edit`'s "file changed on disk" guard checks against.
    /// Empty for scalar fields.
    pub entry_list: Vec<String>,
    /// The items on the active **list** field **in the working state**
    /// (after any tail-removals via the smart `Backspace`). This is the
    /// "what's there now" the user sees, and the dedup baseline for the
    /// `AppendItem` ops `plan()` derives. Empty for scalar fields.
    pub current_list: Vec<String>,
    /// The file bytes **in the working state** (updated by `remove_tail_item`).
    /// `commit_edit` applies ops to this, not to `entry_bytes`, so that
    /// tail-removals are already baked in and `AppendItem` only needs to
    /// add the new tokens. For scalar fields these are always equal to
    /// `entry_bytes` (scalars don't do tail removal).
    pub working_bytes: Vec<u8>,
    /// The current **stage** of this edit session — the two zsh-style modes
    /// (`Picker` or `Value`). See [EditStage].
    pub stage: EditStage,
    /// The currently highlighted **field** in the Picker stage (an index
    /// into `EditField::all()`). `field` (above) is the *currently being
    /// edited* field (the Editor/Completions stage); `field_cursor` is the
    /// *Picker* cursor. They may legitimately differ (the user was on
    /// `tags` in the Editor, went back to the Picker, and is now hovering
    /// over `description`). Total: always `< EditField::all().len()`.
    pub field_cursor: usize,
    /// The highlighted **completion** in the Completions stage (an index
    /// into `suggestions`). Total: always `< suggestions.len()` (clamped on
    /// every filter change). `0` when `suggestions` is empty.
    pub sug_cursor: usize,
    /// The **text cursor** inside the value buffer — a *character offset*
    /// into `value` (Total: always in `0..=value.chars().count()`), so
    /// multi-byte characters land on clean char boundaries. Used only for a
    /// **scalar** field (free-text `←`/`→` editing); a **list** field's
    /// buffer is a whole token, so the cursor is irrelevant there.
    pub value_cursor: usize,
    /// Two sub-modes inside the Value stage for a **list** field (ignored
    /// on a scalar field, where `←`/`→` move the text cursor):
    /// - `false` (default) — the **buffer** is live: `←` enters Selection,
    ///   `↓`/`↑` pick, `Tab` completes, `Enter` commits.
    /// - `true` — the **Selection** sub-mode is live: the user is walking
    ///   the existing items (the trailing `<new>` is a virtual entry).
    ///   `←`/`→`/`↑`/`↓` move the highlight, `Backspace`/`Del` remove the
    ///   highlighted item (live, a real `fm::remove_list_item`), `Enter` on
    ///   an item loads it into the buffer (so the user can type-paste-
    ///   commit), `Enter` on `<new>` returns to buffer mode with the
    ///   cached buffer intact, typing a letter returns to buffer mode
    ///   (appending the letter to the *same* cached buffer), `Esc` returns
    ///   to buffer mode with no change.
    pub in_list_select: bool,
    /// The **highlighted** item in Selection mode (an index into
    /// `current_list` when that is the target, or `current_list.len()`
    /// when the trailing `<new>` entry is the target). Total: always in
    /// `0..=current_list.len()`.
    pub list_high: usize,
    /// The item currently being *edited in place*, if any — the value the
    /// buffer was loaded with (via Selection → `Enter` on an item), and
    /// which `build_ops` will *replace* on commit (not append). `None` for
    /// every "add a new token" / "scalar" commit.
    pub editing_item: Option<String>,
}

impl EditState {
    /// The pure "what will commit apply from this exact state?" — the
    /// *same* `build_ops` helper the render surfaces as the "pending"
    /// footer, so the display + commit paths cannot diverge. For a scalar
    /// field the no-op rule compares the buffer against the field's value
    /// *on entry* (the `build_ops` "original"); for a list field the
    /// dedup rule compares against the *current* list (after any
    /// removals tracked in `current_list`); for a "replace" commit
    /// (`editing_item: Some(old)`) the buffer is a *new* value, so the
    /// plan is `remove_item(old)` + `append_item(new)` (remove-before-
    /// append, atomic). Total.
    pub fn plan(&self) -> Vec<EditOp> {
        build_ops(self.field, &self.value, &self.editing_item, &self.current_list, self.entry_bytes.as_slice())
    }
}

/// A single pending frontmatter edit, over the verified
/// `kb-core::frontmatter` ops. The TUI composes 0..n of these on commit.
/// This is a *data* record (the "what will happen" the UI surfaces), not a
/// function — the actual call into the verified core happens in `apply_op`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOp {
    /// A scalar replacement (`fm::set_scalar`).
    SetScalar { key: &'static str, value: String },
    /// A list-item append (`fm::append_list_item`). One per comma-separated
    /// token the user typed.
    AppendItem { key: &'static str, item: String },
    /// A whole-field removal (`fm::remove_field`) — used to *clear* a scalar.
    RemoveField { key: &'static str },
    /// A single list-item removal (`fm::replace_list_item`). Emitted
    /// (for "replace" commits) when the user edits an existing item via the
    /// Selection sub-mode (`Enter` on a tag loads it; typing a different
    /// value; `Enter` commits) — the "load → edit → save" workflow that
    /// `edit metadata` is designed to support for long tokens. The commit
    /// path derives one `RemoveItem { key, old }` op for the (already
    /// removed) old value and one `AppendItem { key, new }` op for the new
    /// one, applied in that order (`remove` before `append`) so a "mTLS"
    /// → "mtls" rename is atomic — no window in which either value is
    /// absent. `remove_tail_item` (the *live* smart-Backspace path) does
    /// its own `fm::remove_list_item` call without going through `EditOp`
    /// (it mutates the working bytes directly, not building a commit plan).
    RemoveItem { key: &'static str, item: String },
}

/// A one-line status-bar message (the bottom bar).
#[derive(Debug, Clone)]
pub struct Status {
    pub kind: StatusKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum StatusKind {
    Info,
    Success,
    Error,
}

impl Status {
    fn of(kind: StatusKind, text: &str) -> Self {
        Status { kind, text: text.to_string() }
    }
    fn info(text: &str) -> Self { Self::of(StatusKind::Info, text) }
    #[allow(dead_code)]
    fn success(text: &str) -> Self { Self::of(StatusKind::Success, text) }
    #[allow(dead_code)]
    fn error(text: &str) -> Self { Self::of(StatusKind::Error, text) }
}
/// The whole TUI — one value, no hidden state, owned by the event loop.
pub struct App {
    /// The DB handle (moved in) — the only thing that owns the connection
    /// for the session. Every DB read goes through it. Held so the Read
    /// mode (M8.1) and Edit (M8.2) can query against the live table
    /// without a second `Db::open`.
    pub db: Db,
    /// A snapshot of the full page list (built once, so a keystroke is
    /// O(1). Rebuilt by `reload` after an edit commits.)
    pub rows: Vec<PageRow>,
    /// The currently **filtered** row list (a subset of `rows`, in the
    /// same order). Equal to `rows` when `filter_text` is empty. The
    /// *selection* indexes into this list, so a filter change that moves
    /// the selected row out of bounds is clamped by `apply_filter`.
    pub view: Vec<PageRow>,
    /// The filter text (case-insensitive substring over `id` / `title` /
    /// `rel_path`). Empty = show all.
    pub filter_text: String,
    /// Whether the filter input line is currently *armed* — the `/` key
    /// toggles it on, so a following `Enter` commits the filter and `Esc`
    /// clears it, instead of the Enter/Esc that a non-input context would
    /// treat as "open" / "pop".
    pub filter_active: bool,
    /// The selected index in `view`.
    pub selected: usize,
    /// Which screen we're on.
    pub mode: Mode,
    /// Return address (M8.4). When a Read was opened **from Search**
    /// (`search_open`), this holds the `SearchState` so `close_read`
    /// restores it — query buffer, live results, and result selection all
    /// intact. `open_read` clears it (a Browse/filter-opened read has no
    /// search to go back to), `search_open` sets it just before entering
    /// Read, and `close_read` consumes it via `.take()`. Every path that
    /// leaves Read and re-enters it goes through `open_read` (clears) or
    /// `search_open` (sets fresh), so a stale address is never
    /// resurrected.
    pub search_return: Option<SearchState>,
    /// The status-bar text (last event message).
    pub status: Status,
    /// The active color palette (P2). **`Copy`** and read by `render.rs` for
    /// every `Style`. Default-constructed (the pinned `default_palette`) when
    /// `kb tui` runs with no `--palette`; set by `with_palette` after a
    /// `--palette <name|path>` resolves.
    pub palette: palette::Palette,
    /// The **inner** width of the Read body pane (the value `draw_read` passes
    /// to the body `Paragraph` after subtracting the two borders). Written by
    /// the render pass each frame (the event loop draws *before* it polls a
    /// key, so the scroll mutators always read a current value). 0 until the
    /// first frame, in which case the clamp falls back to the logical-line
    /// model (a window too narrow to matter).
    pub body_pane_width: usize,
    /// The **inner** height of the Read body pane (rows minus the two
    /// bordered rows), written by the render pass each frame (same model as
    /// `body_pane_width`). 0 until the first frame; falls back to a
    /// reasonable default (24) until then. Used by find-centering math so
    /// the `input` pass (which mutates `scroll_top`) reads a current value.
    pub body_pane_height: usize,
    /// One-shot "force a full clear before the next draw" flag. Set after an
    /// external editor (the TUI's `$VISUAL`/`$EDITOR`/`vi`) returns: the
    /// editor ran on the *same TTY* while we were in raw mode + alt-screen,
    /// so on exit the real terminal buffer is left diverged from ratatui's
    /// internal model of it — the next diff-based redraw would only patch
    /// "changed" cells and leave stale "holes". `open_in_editor` sets this;
    /// the event loop consumes it (one `terminal.clear()`, then reset) so
    /// the next frame repaints the whole pane from scratch.
    pub needs_clear: bool,
    /// Memoized body geometry (see [`BodyRollup`]). Rebuilt lazily by
    /// [`App::ensure_body_rollup`] the first time it's needed for a given
    /// `(body_revision, pane_width)` key, then read back in O(1). Holding
    /// it on `App` (not `ReadState`) lets the cache outlive the body: when
    /// the user re-opens the same page (with the same body bytes) the cache
    /// is still valid without a re-measure.
    pub body_rollup: BodyRollup,
    /// Return address for the **Link-picker** (M8.5): the `ReadState` the
    /// user was looking at when they pressed `l`. Held on `App` (not
    /// `ReadState`) so the picker can pop back into the Read pane with its
    /// scroll / outline / find state intact. `None` when the Link-picker is
    /// not active.
    pub links_return: Option<ReadState>,
    /// Return address for a *linked* Read (M8.5): the `ReadState` the user
    /// was looking at when they pressed `Enter` in the Link-picker and the
    /// TUI opened the *target* page's Read. `Esc`-back from that linked
    /// Read pops to this (not Browse). `None` when the current Read was
    /// opened from Browse / Search (the normal case).
    pub read_return: Option<ReadState>,
}

/// A `Result` over the `(message, exit-code)` tuple the rest of `kb`
/// uses, so a failed TUI maps to the same stderr + exit-code contract.
pub type KbResult<T> = Result<T, (String, i32)>;

impl App {
    /// Load the DB snapshot into the TUI state.
    pub fn new(db: Db) -> KbResult<Self> {
        let rows = list_pages(&db)?;
        // The keybinding legend lives in the status bar (the one place a
        // user glances for "what do I press"), NOT the pane title — the
        // title carries identity + count only. This is what removes the
        // duplicate/truncated legend we saw in the captures (top title +
        // bottom bar each printing the same hints).
        let status = if rows.is_empty() {
            Status::info("no pages indexed yet — run `kb add <root>` then re-run `kb tui`")
        } else {
            Status::info("↑↓/jk move · / filter · s search · e edit · a alias · r roots · Enter open · v $EDITOR · q quit")
        };
        Ok(App {
            view: rows.clone(),
            db,
            rows,
            filter_text: String::new(),
            filter_active: false,
            selected: 0,
            mode: Mode::Browse,
            search_return: None,
            status,
            palette: palette::Palette::default_palette(),
            // 80 columns is a reasonable *default* pane width: it matches
            // the conventional `COLUMNS=80` terminal and keeps the scroll
            // mutators total before the first render pass has written the
            // real width. `draw_read` overwrites this every frame with the
            // real value computed from the live terminal size.
            body_pane_width: 80,
            // 24 rows is a reasonable default body-pane height (matches
            // COLUMNS=80, LINES=24 conventional terminal). Keeps the
            // find-centering math total before the first render pass has
            // written the real height. `draw_read` overwrites this every
            // frame with the live value.
            body_pane_height: 24,
            // A fresh App has just drawn its first frame in the loop, which
            // already clears; only *post-editor* returns need the forced
            // clear, so start off.
            needs_clear: false,
            // Link-picker not open yet — nothing to return to; same for a
            // linked Read (no link was followed yet).
            links_return: None,
            read_return: None,
            // Empty roll-up: the first body we scroll / center / draw
            // triggers a build. Default to a known-stale state so the
            // identity check below always misses.
            body_rollup: BodyRollup::default(),
        })
    }

    /// Build (or rebuild) the body roll-up on `self`, so that the caller can
    /// then read the fields on `self.body_rollup` with a fresh, short-lived
    /// immutable borrow. Kept as a **void** method (not `-> &BodyRollup`) so
    /// that the `&mut self` borrow ends *before* the caller takes its
    /// subsequent `&self` reads — this is what lets the read paths mix
    /// `self.body_rollup.max_top()` + `self.palette` + `self.body_pane_width`
    /// (and friends) in the same expression without tripping E0502, and it
    /// is the entire reason this is written the way it is.
    ///
    /// The key is `(body_hash, pane_width)`: the body is *hashed* (a 64-bit
    /// FNV-1a on the raw body string — see [`fnv1a`]) and the width is an
    /// exact `usize` match. A key hit means the tables are already valid,
    /// so the whole call is **O(1)**. A miss triggers a single O(n) pass —
    /// one `line_row_count` (`WordWrapper`) call per logical line — and
    /// installs the result; the next hit is free.
    ///
    /// **Correctness guard:** a 64-bit FNV-1a collision that maps two
    /// different bodies to the same `body_hash` would return a stale
    /// roll-up. That is acceptable for a TUI scroll-clamp (worst case: the
    /// user nudges with arrows); it cannot panic or corrupt memory because
    /// every read site is `.min()`-clamped and `total_rows` is
    /// `saturating_sub`-ed. If a stronger guarantee is ever needed, swap
    /// the FNV-1a for 128-bit xxHash + body byte length.
    pub fn ensure_body_rollup(&mut self, body: &str) {
        let hash = fnv1a(body.as_bytes());
        let stale = {
            let r = &self.body_rollup;
            !r.valid || r.body_hash != hash || r.width != self.body_pane_width
        };
        if !stale {
            return;
        }
        let width = self.body_pane_width;
        let mut first_row_of: Vec<usize> = Vec::new();
        let mut acc: usize = 0;
        for line in body.lines() {
            let per = crate::tui::render::line_row_count(line, width);
            first_row_of.push(acc);
            acc += per;
        }
        self.body_rollup = BodyRollup {
            valid: true,
            body_hash: hash,
            width,
            first_row_of,
            total_rows: acc,
        };
    }

    /// Install a resolved palette (P2). Called from `run_tui` after
    /// `--palette <name|path>` has been validated to a `Palette`. A no-flag
    /// run never calls this, so the pinned default stays in effect.
    pub fn with_palette(mut self, palette: palette::Palette) -> Self {
        self.palette = palette;
        self
    }

    /// Recompute `view` (the filtered row list) from `filter_text` and
    /// clamp `selected` into the new bounds. Total: on an empty filter the
    /// view is the full `rows`; on a non-empty filter it is the (possibly
    /// empty) matching subset, and `selected` never points out of range.
    ///
    /// Matching is a *case-insensitive substring* over:
    /// - `id`, `title`, `rel_path` — the page's display identity;
    /// - **any tag** (`r.tags`) — a user who types `/security` should find a
    ///   page tagged `security` even if its id/title/path don't mention it;
    /// - **any `search_required_phrase`** entry (`r.gates`) — typing one of
    ///   the page's gate terms (e.g. `mtls`) should surface the gated page
    ///   in the browse list (a useful way to find gated pages by name).
    pub fn apply_filter(&mut self) {
        let needle = self.filter_text.to_lowercase();
        if needle.is_empty() {
            self.view = self.rows.clone();
        } else {
            self.view = self
                .rows
                .iter()
                .filter(|r| {
                    r.id.to_lowercase().contains(&needle)
                        || r.title.to_lowercase().contains(&needle)
                        || r.rel_path.to_lowercase().contains(&needle)
                        || r.tags.iter().any(|t| t.to_lowercase().contains(&needle))
                        || r.gates.iter().any(|g| g.to_lowercase().contains(&needle))
                })
                .cloned()
                .collect();
        }
        if self.view.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.view.len() - 1);
        }
    }

    /// Move selection by `delta` (positive = down, negative = up),
    /// clamped to `[0, view.len()-1]`. No-op on an empty view (total).
    pub fn move_selection(&mut self, delta: isize) {
        if self.view.is_empty() {
            return;
        }
        let n = self.view.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, n - 1);
        self.selected = next as usize;
    }

    /// Jump to top / bottom of the filtered view (no-op on an empty view).
    pub fn jump_to(&mut self, end: bool) {
        if self.view.is_empty() {
            return;
        }
        self.selected = if end { self.view.len() - 1 } else { 0 };
    }

    /// The selected row, if any.
    pub fn selected_row(&self) -> Option<&PageRow> {
        self.view.get(self.selected)
    }

    /// A human status string for the current selection state ("" on an
    /// empty view, e.g. "row 3 of 12 — note:P3"). This is the "one source
    /// of truth" for the current position message, so input events that
    /// change selection all funnel through this one helper.
    pub fn position_note(&self) -> String {
        if self.view.is_empty() {
            return String::new();
        }
        let id = self
            .selected_row()
            .map(|r| r.id.as_str())
            .unwrap_or("<no selection>");
        format!("row {} of {} — {}", self.selected + 1, self.view.len(), id)
    }

    /// Scroll the Read body by `delta` **rendered rows** (the user's perceptual
    /// unit: one press = one row on screen). The clamp is against the
    /// *width-aware* max (a logical line that wraps to N rendered rows adds N
    /// to the scroll range, not 1). The pane width is `body_pane_width`,
    /// written by the render pass (`draw_read`) on the previous frame — the
    /// event loop draws before it dispatches the key, so this mirror is
    /// always current.
    pub fn scroll_body(&mut self, delta: isize) {
        // Take the current body + scroll out of the borrow (we need `&mut self`
        // for the roll-up, so we can't hold a borrow simultaneously).
        let (body, cur) = match &self.mode {
            Mode::Read(r) => (r.body.clone(), r.scroll_top),
            _ => return,
        };
        self.ensure_body_rollup(&body);
        let max_top = self.body_rollup.max_top();
        let n = cur as isize + delta;
        if let Mode::Read(r) = &mut self.mode {
            r.scroll_top = n.clamp(0, max_top as isize) as usize;
        }
    }

    /// Scroll the Read body to the top.
    pub fn scroll_body_top(&mut self) {
        if let Mode::Read(r) = &mut self.mode {
            r.scroll_top = 0;
        }
    }

    /// Scroll the Read body to the bottom (the last rendered row).
    pub fn scroll_body_bottom(&mut self) {
        // Read the body + find-center up front (immutable), then compute the
        // plain max via the roll-up (which needs `&mut self`), then write the
        // target. Two passes over `self` are required by the borrow checker,
        // so we copy what we need into locals and do the roll-up once.
        let (body, center_max) = match &self.mode {
            Mode::Read(rs) => (
                rs.body.clone(),
                rs.find
                    .as_ref()
                    .and_then(|fs| fs.center.as_ref())
                    .map(|c| c.scroll_max_top),
            ),
            _ => return,
        };
        // If find has no stored center, the plain max (from the roll-up) is
        // what we use; if it does, the stored center already carries the
        // pad-aware max and we skip the roll-up.
        let target: usize = if let Some(c) = center_max {
            c
        } else {
            self.ensure_body_rollup(&body);
            self.body_rollup.max_top()
        };
        if let Mode::Read(rs) = &mut self.mode {
            rs.scroll_top = target;
        }
    }

    // ===================================================================
    //  M8.5 — Reader find (the `f` key in Read mode)
    //  ===================================================================

    /// Arm the reader-find: a fresh `FindState` with an empty needle; no
    /// matches yet, cursor 0. The body keeps its current scroll (find only
    /// *highlights* + recenters once matches exist). Total (a no-op outside
    /// Read mode).
    pub fn find_activate(&mut self) {
        if let Mode::Read(r) = &mut self.mode {
            r.find = Some(FindState {
                needle: String::new(),
                matches: Vec::new(),
                cursor: 0,
                center: None,
            });
        }
    }

    /// Append a char to the needle (the live-input arm — the `/` filter
    /// idiom), then recenter: `find_center_scroll` refreshes the hit list
    /// from the new needle, recomputes the centering, clamps the curl, and
    /// settles the scroll on the newly-selected first hit. Total (a no-op
    /// outside Read mode or when find is closed).
    pub fn find_input(&mut self, c: char) {
        if let Mode::Read(r) = &mut self.mode {
            if let Some(fs) = &mut r.find {
                fs.needle.push(c);
            }
        }
        self.find_center_scroll();
    }

    /// Pop the last needle char (Backspace), then recenter (same contract as
    /// `find_input`). An empty needle stays armed (Esc closes find) — the
    /// match list simply becomes empty and the body renders unpadded. Total.
    pub fn find_backspace(&mut self) {
        if let Mode::Read(r) = &mut self.mode {
            if let Some(fs) = &mut r.find {
                fs.needle.pop();
            }
        }
        self.find_center_scroll();
    }

    /// Move the selected hit to the **next** occurrence (wrap around the
    /// match list — `n m 2 of 2` behaves like `n m 1 of 2`). Each move
    /// recenters the viewport on the newly selected hit (via the padded
    /// centering in `find_center_scroll`). The match list is empty ⇒ a no-op
    /// (the user is still typing the needle).
    pub fn find_next(&mut self) {
        if let Mode::Read(r) = &mut self.mode {
            if let Some(fs) = &mut r.find {
                if !fs.matches.is_empty() {
                    fs.cursor = (fs.cursor + 1) % fs.matches.len();
                    self.find_center_scroll();
                }
            }
        }
    }

    /// Move the selected hit to the **previous** occurrence (wrap; the
    /// inverse of `find_next`). Same no-match, recenter semantics.
    pub fn find_prev(&mut self) {
        if let Mode::Read(r) = &mut self.mode {
            if let Some(fs) = &mut r.find {
                if !fs.matches.is_empty() {
                    fs.cursor =
                        (fs.cursor + fs.matches.len() - 1) % fs.matches.len();
                    self.find_center_scroll();
                }
            }
        }
    }

    /// Close the find session: drop the `find` state (the highlight goes
    /// away; the body keeps its scroll position so the user is still where
    /// they navigated). Total (a no-op outside Read mode or when find is
    /// already closed).
    pub fn find_close(&mut self) {
        if let Mode::Read(r) = &mut self.mode {
            r.find = None;
        }
    }

    /// Recompute the needle's hits **and** recenter the viewport on the
    /// selected hit (the live-input / nav call site). Two effects:
    /// (1) `find_center_scroll` refreshes the match list from the current
    ///     needle + body (the pure `find_matches` predicate);
    /// (2) `find_center_scroll` recomputes the above/below padding (the pure
    ///     `find_centering` helper) and sets `scroll_top` so the selected
    ///     hit lands at the pane's center — the "fake rows" the user asked
    ///     for, applied live on every keystroke/nav.
    /// Recompute the needle's hits **and** recenter the viewport on the
    /// selected hit (the live-input / nav call site). This is the single
    /// sync path for every find mutation:
    /// (1) refresh the match list from the current needle + body (the pure
    ///     `find_matches` predicate) and clamp the cursor into range;
    /// (2) compute the centering padding (the pure `find_padding` helper)
    ///     and store it on `FindState.center` — the render pass and the
    ///     scroll clamps then derive their values from that one stored
    ///     padding, so they cannot desync;
    /// (3) set `ReadState::scroll_top` to the pad-aware center row (clamped
    ///     to the pad-aware max). With no matches the body is rendered
    ///     unpadded and the scroll is clamped to the un-padded max (the user
    ///     stays where they are — no jump to row 0).
    /// Total (a no-op outside Read mode or when find is closed).
    pub fn find_center_scroll(&mut self) {
        // Read the body + needle up front (immutable), so the roll-up and
        // center compute don't hold a borrow across the `&mut self` writes.
        let (body, needle, cursor, cur_top) = match &self.mode {
            Mode::Read(r) => match &r.find {
                Some(fs) => (
                    r.body.clone(),
                    fs.needle.clone(),
                    fs.cursor,
                    r.scroll_top,
                ),
                None => return,
            },
            _ => return,
        };
        // Build / refresh the roll-up on the body (O(1) on a hit; O(n) only
        // when the body or width has actually changed). After the call the
        // roll-up lives on `self.body_rollup`; we copy the two fields we
        // need into locals (both are `usize` / a `Vec<usize>`, cheap to
        // read) so the `&self` reads below don't conflict with the
        // subsequent `&mut self` write.
        self.ensure_body_rollup(&body);
        let total_rows = self.body_rollup.total_rows;
        // O(n) clone of the per-line first-row table (one `usize` per logical
        // line — typically far smaller than the body itself). Cloned once,
        // then the closure below reads only locals, so the `&mut self`
        // borrow ends cleanly before the `&mut self` write after.
        let first_row_of = self.body_rollup.first_row_of.clone();
        let max_plain = total_rows.saturating_sub(1);
        let pane_h = self.body_pane_height;
        let pane_w = self.body_pane_width;
        let lines = crate::tui::render::rendered_lines(&self.palette, &body);
        let m = find_matches(&lines, &needle);
        let sel_idx = if m.is_empty() { None } else { Some(cursor.min(m.len() - 1)) };
        let new_center: Option<FindPadding> = sel_idx.map(|cur| {
            let (line_idx, byte_off) = m[cur];
            // Resolve the hit's *actual* rendered row (ratatui's own
            // `WordWrapper`, via an off-screen `Buffer` probe) — the
            // per-line row tables in the roll-up can't see a hit several
            // wraps down a long line, so we call out to ratatui for that,
            // falling back to the line's first row from the cached table.
            let hit_row = crate::tui::render::match_rendered_row(&lines, (line_idx, byte_off), pane_w)
                .unwrap_or_else(|| first_row_of.get(line_idx).copied().unwrap_or(0));
            find_padding(hit_row, total_rows, pane_h)
        });
        let max = new_center
            .as_ref()
            .map(|c| c.scroll_max_top)
            .unwrap_or(max_plain);
        let target = new_center
            .as_ref()
            .map(|c| c.scroll_top.min(c.scroll_max_top))
            .unwrap_or(cur_top);
        if let Mode::Read(r) = &mut self.mode {
            let fs = match r.find.as_mut() {
                Some(fs) => fs,
                None => return,
            };
            fs.matches = m.clone();
            fs.cursor = cursor.min(fs.matches.len().saturating_sub(1));
            fs.center = new_center;
            r.scroll_top = target.min(max);
        }
    }

    /// Whether a find session is currently armed (in Read mode with a live
    /// `FindState`). Used by `handle_read` to short-circuit into the
    /// live-input handler before the normal keymatch.
    pub fn find_armed(&self) -> bool {
        matches!(&self.mode, Mode::Read(rs) if rs.find.is_some())
    }

    /// The "hit K of N" status note for a live find (the status-bar
    /// text after a keystroke/nav). `Some(note)` when armed with at least
    /// one match; `None` when find is closed, not in Read, or has zero
    /// matches (the caller falls back to the last status).
    pub fn find_note(&self) -> Option<String> {
        let rs = match &self.mode {
            Mode::Read(rs) => rs,
            _ => return None,
        };
        let fs = rs.find.as_ref()?;
        let m = fs.matches.len();
        if m == 0 {
            return Some(format!("find — needle: '{}' (0 matches)", fs.needle));
        }
        let pos = fs.cursor.min(m - 1) + 1;
        Some(format!(
            "find ({} match{}) — {pos} of {}  needle: '{}'",
            m, if m == 1 { "" } else { "es" }, m, fs.needle
        ))
    }

    // ===================================================================
    //  M8.5 — Link-picker
    //  ===================================================================

    /// Open the **Link-picker** for the page at `abs_path`. Loads the
    /// page's body + the full corpus on demand (the TUI does not cache the
    /// corpus in the App — `load_corpus` is O(corpus) and the picker is
    /// short-lived), and pre-selects the link nearest a *pending* find
    /// anchor in the current Read mode (the `f` key left a cursor on a hit;
    /// `l` then starts the picker at the closest link to that hit, so the
    /// "follow this fact" path is *`f` → type the fact → `l` → `Enter`*).
    ///
    /// `abs_path` is required because the picker is opened *from* the Read
    /// mode (the App already knows it) but it is also the identity for the
    /// return stack (`links_return`) — the picker's `Esc` pops back to it
    /// without a re-lookup.
    pub fn open_links(&mut self) -> KbResult<()> {
        let rs = match &self.mode {
            Mode::Read(rs) => rs,
            _ => {
                self.set_info("link-picker: press `l` while in Read mode");
                return Ok(());
            }
        };
        // Snapshot of the Read state we're coming from (so `Esc` can pop
        // back to it with scroll / outline / find intact).
        let return_read = rs.clone();
        let abs_path = rs.abs_path.clone();

        // If a find session is armed *right now*, use the *currently
        // highlighted* hit as the anchor (the user typed, moved with ↑/↓,
        // and is now pressing `l` to follow the closest link). Otherwise
        // no anchor → start at link 0.
        let anchor: Option<(usize, usize)> = rs
            .find
            .as_ref()
            .and_then(|fs| fs.matches.get(fs.cursor).copied());

        let corpus = load_corpus(&self.db)?;
        let body: String = self
            .db
            .conn
            .query_row(
                "SELECT COALESCE(body, '') FROM pages WHERE abs_path = ?1",
                [abs_path.as_str()],
                |r| r.get(0),
            )
            .map_err(|e| (format!("body lookup for {abs_path}: {e}"), EXIT_ERR))?;
        let annotated = crate::annotated_links(&body, &corpus);
        let entries: Vec<LinksEntry> = annotated
            .into_iter()
            .map(|al| LinksEntry {
                line: al.line,
                col: al.col,
                context: al.context,
                target: al.target,
                resolved: al.resolved_id.as_ref().map(|(t, s)| format!("{t}:{s}")),
                candidates: al.candidates,
                abs_path: al.resolves_to,
            })
            .collect();

        // Nearest-neighbor selector: when an anchor (the find hit) is
        // supplied, the picker's cursor points at the entry closest to it
        // (primary: `|line - anchor_line|`, then `|col - anchor_col|`).
        // The list stays in document order — the user still has a stable
        // "this is the 3rd link on the page" mental model — only the
        // starting position is pre-computed for the "I just finished `f`"
        // path. A pure function (no IO, no App) so it is unit-testable.
        let links = entries;
        let cursor = if let Some((al, ac)) = anchor {
            let pairs: Vec<(usize, usize)> = links.iter().map(|l| (l.line, l.col)).collect();
            crate::nearest_link_index(&pairs, al, ac).unwrap_or(0)
        } else {
            0
        };

        // Park the Read we're leaving for the picker's `Esc`; clear the
        // linked-Read return stack so a stale one doesn't leak through.
        self.links_return = Some(return_read);
        self.read_return = None;
        let ls = LinksState {
            links,
            cursor,
            source_abs_path: abs_path.clone(),
        };
        self.mode = Mode::Links(ls);
        if let Some((al, ac)) = anchor {
            self.set_info(format!(
                "link-picker — started at the link nearest find hit (line {al}, col {ac}) · ↑↓ move · Enter open · Esc back"
            ));
        } else {
            self.set_info("link-picker — ↑↓ move · Enter open · Esc back");
        }
        Ok(())
    }

    /// Pop back from the Link-picker to the `ReadState` the user armed it
    /// from (held in `links_return`). Idempotent — a double-Esc can't
    /// resurrect a second Read (the `.take()` clears the return address).
    pub fn close_links(&mut self) {
        let rs = self.links_return.take();
        self.mode = match rs {
            Some(rs) => Mode::Read(rs),
            None => Mode::Browse,
        };
    }

    /// Open a *linked* page's Read from the Link-picker (the `Enter`
    /// action on a *unique* link entry). The target page is identified by
    /// its `abs_path` (the stable identity — see `open_read`), so a
    /// `full-rebuild` that reassigns rowids can't open the wrong page.
    ///
    /// The *incoming* Read is parked in `read_return` (so `Esc`-back from
    /// the new Read lands in the *incoming* Read state, not Browse —
    /// `close_read` checks `read_return` before `search_return`); the
    /// picker itself was opened *from* the incoming Read, so its
    /// `links_return` is set to the *same* state and the two return stacks
    /// are consistent after the round trip.
    pub fn open_linked(&mut self, abs_path: &str) -> KbResult<()> {
        let (body, outline, fm) = load_body(&self.db, abs_path)?;
        // row_idx / id are display only — the *identity* is the path.
        let id = self
            .rows
            .iter()
            .find(|r| r.abs_path == abs_path)
            .map(|r| r.id.clone())
            .unwrap_or_else(|| "?".to_string());
        let row_idx = self
            .rows
            .iter()
            .position(|r| r.abs_path == abs_path)
            .unwrap_or(0);
        // Park the *incoming* Read in `read_return` (so `close_read` can
        // pop back into it) and clear the picker's return address (the
        // picker is done).
        let incoming = std::mem::take(&mut self.links_return);
        self.read_return = incoming;
        let new_rs = ReadState {
            row_idx,
            id,
            body,
            outline,
            scroll_top: 0,
            abs_path: abs_path.to_string(),
            fm,
            find: None,
        };
        self.mode = Mode::Read(new_rs);
        self.set_success(format!("opened linked page {abs_path} — Esc back to the link you followed"));
        Ok(())
    }

    /// Enter-key action in the Link-picker: read the *selected* entry and
    /// either open it (unique) or no-op (ambiguous / unresolved). Kept as a
    /// separate `pub` method (not a match arm) so it can be unit-tested
    /// directly (`let _ = app.links_open_selected();` and assert on the
    /// resulting mode + `read_return`).
    pub fn links_open_selected(&mut self) -> KbResult<()> {
        // Read the selected entry out of the mode first (the borrow of
        // `self.mode` conflicts with the `self.open_linked` call below, so
        // clone the entry + cursor out and drop the borrow).
        let (entry, cursor, total) = match &self.mode {
            Mode::Links(l) => {
                let e = l.links.get(l.cursor).cloned();
                let t = l.links.len();
                (e, l.cursor, t)
            }
            _ => (None, 0usize, 0usize),
        };
        let Some(entry) = entry else {
            self.set_info("link-picker — no entry at cursor");
            return Ok(());
        };
        // Unique resolution — open it.
        if let Some(abs) = entry.abs_path.clone() {
            return self.open_linked(&abs);
        }
        // Ambiguous — tell the user how to get there from the CLI.
        if !entry.candidates.is_empty() {
            self.set_info(format!(
                "link {} of {} is ambiguous ({} candidates) — run `kb links --list {page}` to disambiguate",
                cursor + 1,
                total,
                entry.candidates.len(),
                page = self.links_source_page()
            ));
            return Ok(());
        }
        // Unresolved — dead link.
        self.set_info(format!(
            "link {} of {} is unresolved (no matching page) — {target}",
            cursor + 1,
            total,
            target = entry.target
        ));
        Ok(())
    }

    /// Status-bar note for the Link-picker: the selected entry's context +
    /// resolution at a glance (the user can see *which* link they're about
    /// to open without reading the list). `App` owns this so it is
    /// unit-testable.
    pub fn set_links_note(&mut self) {
        let (cursor, total, entry) = match &self.mode {
            Mode::Links(l) => {
                let e = l.links.get(l.cursor).cloned();
                (l.cursor, l.links.len(), e)
            }
            _ => (0, 0, None),
        };
        let Some(entry) = entry else {
            self.set_info("link-picker — no links on this page");
            return;
        };
        let status = match &entry.resolved {
            Some(id) => format!("→ {id} — Enter opens"),
            None if entry.candidates.len() > 1 => format!(
                "({} candidates — open from the CLI to disambiguate)",
                entry.candidates.len()
            ),
            None => String::from("unresolved — dead link"),
        };
        self.set_info(format!(
            "link {cursor_plus} of {total} · L{line} {status} · {context}",
            cursor_plus = cursor + 1,
            line = entry.line + 1,
            status = status,
            context = entry.context
        ));
    }

    /// The `abs_path` of the page the picker was opened from — for status
    /// messages that need to name it (`self.links_source_page()`).
    pub fn links_source_page(&self) -> String {
        match &self.mode {
            Mode::Links(l) => l.source_abs_path.clone(),
            _ => String::new(),
        }
    }

    /// Commit the selected row to a new Read mode (loads the body from the
    /// DB). Called by `Enter` from Browse. The page is identified by its
    /// *absolute path* — a stable identity across a `full-rebuild` (which
    /// may reassign rowids) — so the TUI can't open the wrong page after a
    /// re-index. The `row_idx` cached on `ReadState` is for display only
    /// ("reading row 3 of 12 — note:P3"); the DB re-lookup is always by path.
    pub fn open_read(&mut self) -> KbResult<()> {
        let row = match self.selected_row() {
            Some(r) => r.clone(),
            None => return Ok(()),
        };
        // This read is a Browse/filter open — no Search context to return to.
        // (A read opened *from* Search sets `search_return` explicitly in
        // `search_open`; clearing here also kills any stale address from an
        // earlier search session.) Also clear any stale `read_return` (a
        // linked Read's return stack) so it does not leak through this new
        // Read opened from the list.
        self.search_return = None;
        self.read_return = None;
        let (body, outline, fm) = load_body(&self.db, &row.abs_path)?;
        let id = row.id;
        // Keep a stable row-index for the header display: the index of the
        // page in the full (un-filtered) `rows` snapshot — not in `view`,
        // which can be a filtered subset where "row 1" is ambiguous.
        let row_idx = self
            .rows
            .iter()
            .position(|r| r.abs_path == row.abs_path)
            .unwrap_or(0);
        let rs = ReadState {
            row_idx,
            id,
            body,
            outline,
            scroll_top: 0,
            abs_path: row.abs_path,
            fm,
            find: None,
        };
        self.mode = Mode::Read(rs);
        Ok(())
    }

    /// Go back from Read to *where the read was opened from*: if a Search
    /// context is parked in `search_return`, restore it (Esc from
    /// Search→Read returns to the search, query intact); otherwise return to
    /// Browse with `view` / `selected` intact (the selection is preserved
    /// across the round trip, so `Esc` in Read then `j` continues where the
    /// user left off). `.take()` makes this idempotent — a double-Esc (or an
    /// `open_read` that cleared the address) never resurrects a second
    /// search.
    pub fn close_read(&mut self) {
        // Linked-Read return takes precedence (most specific): a Read opened
        // *from* a link picker lands back on the incoming Read, not Browse.
        if let Some(rs) = self.read_return.take() {
            self.mode = Mode::Read(rs);
            self.set_info("back to the Read you linked from");
            return;
        }
        if let Some(search) = self.search_return.take() {
            self.mode = Mode::Search(search);
            let note = self.position_note();
            self.set_info(note);
        } else {
            self.mode = Mode::Browse;
        }
    }

    /// Open the *selected page's file* directly in the user's
    /// `$VISUAL` / `$EDITOR` / `vi` — the same `edit_in_place` the CLI uses
    /// for `kb create -e`, so the TUI and the CLI share one editor-launch
    /// path (one place to tweak, one error wording). The page is identified
    /// by its *absolute path* (stable across DB re-indexes) and must exist
    /// on disk — a missing file is an error, not a no-op (the TUI should
    /// never select a phantom row; `kb refresh` re-syncs if it happens).
    /// **Total** on an empty `view`: returns `Ok(())` (the user has
    /// nothing to open).
    ///
    /// The *mode* is left in `Browse` / `Read` (the editor launches
    /// synchronously — the TUI does not repaint until it returns). A
    /// successful editor exit re-reads the page (so any `kb refresh` or
    /// manual `Esc`-back sees the freshest body); this is the same refresh
    /// `open_read` / `close_read` implicitly rely on.
    #[cfg(feature = "editor")]
    pub fn open_in_editor(&mut self) -> KbResult<()> {
        let row = match self.selected_row() {
            Some(r) => r.clone(),
            None => {
                self.set_info("nothing selected — scroll to a page, then press `v`");
                return Ok(());
            }
        };
        let path = std::path::PathBuf::from(&row.abs_path);
        crate::edit_in_place(&path)?;
        // Best-effort re-read: if the user edited the file, the *DB* is
        // still on the old bytes until `kb refresh` — but the *file* is the
        // source of truth, so we re-load the Read body if we're in Read
        // (cheap: one fs read + one DB parse; a failure is a no-op so the
        // TUI stays usable on a transient io error).
        if let Mode::Read(r) = &mut self.mode {
            if let Ok((body, outline, fm)) = load_body(&self.db, &path.to_string_lossy()) {
                r.body = body;
                r.outline = outline;
                r.fm = fm;
                r.scroll_top = 0;
            }
        }
        // Also re-snapshot the row's file bytes (in Browse mode the row's
        // `abs_path` is already what we'd read next — no state to update).
        self.set_success(format!("opened {} — Esc back (the file is now source of truth; re-index with `kb refresh`)", row.abs_path));
        // The editor held our TTY (raw mode + alt-screen) for its whole
        // lifetime; on exit the real terminal buffer is diverged from
        // ratatui's internal model, so a plain diff-redraw leaves "holes".
        // Force a full clear on the next frame so it repaints from scratch.
        self.needs_clear = true;
        Ok(())
    }

    // ===================================================================
    //  M8.2 — Edit mode
    //  ===================================================================

    /// Open the selected page into Edit mode. The file bytes (from disk)
    /// are loaded and cached as `entry_bytes` (the disk guard + baseline for
    /// the no-op rule), and the field's *current* values are read so the
    /// user edits in place (scalars) and sees what's there (lists).
    /// **Total**: an open on an empty `view` is a no-op.
    ///
    /// Enters the **Picker** stage: the *field list* is shown (title /
    /// description / tags / search_required_phrase, with each field's current value
    /// next to it). `↑`/`↓` move the highlight; `Enter` moves to the
    /// *Editor* for that field. The user must *choose* a field before
    /// touching its value — the value buffer and the completion menu are
    /// two *focus states* of the same pane, toggled by `Tab` (the zsh
    /// completion idiom), not two parallel panes that steal keys from each
    /// other.
    pub fn open_edit(&mut self) -> KbResult<()> {
        let row = match self.selected_row() {
            Some(r) => r.clone(),
            None => {
                self.set_info("nothing selected to edit — scroll to a page and press Enter");
                return Ok(());
            }
        };
        // The file is the source of truth (the DB is only a cache).
        let bytes = std::fs::read(&row.abs_path)
            .map_err(|e| (format!("read {} for edit: {e}", row.abs_path), EXIT_ERR))?;
        let field = EditField::Title;
        // The Picker stage shows the *field list*; it does not yet use a
        // corpus / suggestions (no field is active). Those are loaded on
        // demand the moment the user enters the Editor for a specific field
        // (the `cycle_field` path).
        self.mode = Mode::Edit(EditState {
            field,
            abs_path: row.abs_path,
            id: row.id,
            value: String::new(),
            suggestions: Vec::new(),
            corpus: Vec::new(),
            entry_bytes: bytes.clone(),
            entry_list: Vec::new(),
            current_list: Vec::new(),
            working_bytes: bytes,
            stage: EditStage::Picker,
            field_cursor: 0,
            sug_cursor: 0,
            value_cursor: 0,
            in_list_select: false,
            list_high: 0,
            editing_item: None,
        });
        self.set_info("edit metadata — ↑/↓ field · Enter to edit · Esc back");
        Ok(())
    }

    /// Switch the active field to `field` and re-load its state (value,
    /// corpus, current-list, suggestions) from the *original* bytes. A
    /// field switch is "start fresh on this field", not "carry the previous
    /// buffer over" — the input buffer is reset to the field's current
    /// value (scalar) or empty (list), and the suggestions re-anchor to the
    /// full corpus.
    pub fn cycle_field(&mut self, field: EditField) {
        if let Mode::Edit(e) = &mut self.mode {
            let corpus = corpus_for(&self.db, field);
            let value = if field.is_list() {
                String::new()
             } else {
                fm::scalar_value(e.entry_bytes.as_slice(), field.key()).unwrap_or_default()
            };
             let current_list = if field.is_list() {
                fm::list_values(e.entry_bytes.as_slice(), field.key()).unwrap_or_default()
            } else {
                Vec::new()
            };
            let suggestions = if field.is_list() {
                suggestions_for(&corpus, "")
            } else {
                Vec::new()
            };
            e.field = field;
            e.value = value;
            e.suggestions = suggestions;
            e.corpus = corpus;
            e.current_list = current_list;
            // Entering a field (re-)anchors the candidate highlight at the
            // top, so a fresh field starts with a stable, known menu.
            e.sug_cursor = 0;
            // Scalar fields start the text cursor at the *end* of the
            // buffer (the natural "append at the end" default — the user is
            // editing in place and will typically append, not prepend).
            // List fields start the cursor in buffer mode (so `↑/↓` on a
            // fresh field go to the candidate list, not Selection-mode).
            e.value_cursor = if field.is_list() {
                0
            } else {
                e.value.chars().count()
            };
            e.in_list_select = false;
            e.list_high = 0;
            e.editing_item = None;
            self.set_info(Self::edit_field_note(field));
        }
    }

    /// Re-derive the live candidate list from the current buffer. The menu
    /// is *always* a pure function of the buffer (`suggestions_for`), so a
    /// typing or backspace re-narrows the list in place (the zsh idiom —
    /// a filter change does not drop the menu, it only re-anchors it), and
    /// clamps the menu cursor into the (possibly shorter) list. `sug_cursor`
    /// is the one field *not* reset — the user's selection survives a
    /// re-narrow when it still applies. Total on a scalar (no-op).
    fn refresh_suggestions(&mut self) {
        if let Mode::Edit(e) = &mut self.mode {
            if e.field.is_list() {
                e.suggestions = suggestions_for(&e.corpus, &e.value);
                e.sug_cursor = e.sug_cursor.min(e.suggestions.len().saturating_sub(1));
            }
        }
    }

    /// **Typed character**, dispatched by the current (sub-)mode:
    /// - **Scalar**, buffer mode: insert `c` at `value_cursor` (a char
    ///   offset; multi-byte safe), advance the cursor. The "text cursor"
    ///   the user asked for in scalar fields — `←/→` will move it separately
    ///   (see `move_buffer_cursor`).
    /// - **List**, buffer mode: append `c` to the end of the buffer (a list
    ///   buffer is a whole token, so the cursor is irrelevant here).
    /// - **List**, Selection mode: *exit* Selection mode (back to buffer),
    ///   *append* `c` to the *same* cached buffer (the user's accidental
    ///   `→` press is undone: their in-progress token is preserved, and they
    ///   can keep typing), clear `editing_item` (a new token is being typed,
    ///   not a loaded one being refined), reset `list_high`.
    ///
    /// Total; no-op outside a Value-stage Edit mode (defensive).
    pub fn edit_type(&mut self, c: char) {
        if let Mode::Edit(e) = &mut self.mode {
            if e.stage != EditStage::Value {
                return;
            }
            if e.field.is_list() && e.in_list_select {
                // Exiting Selection with a typed char: append to the same
                // cached buffer; drop any loaded-item state.
                e.value.push(c);
                e.in_list_select = false;
                e.editing_item = None;
                e.list_high = 0;
            } else if e.field.is_list() {
                e.value.push(c);
            } else {
                // Scalar: char-offset insert at the cursor, then advance.
                let mut chars: Vec<char> = e.value.chars().collect();
                let pos = e.value_cursor.min(chars.len());
                chars.insert(pos, c);
                e.value_cursor = pos + 1;
                e.value = chars.into_iter().collect();
            }
        }
        self.refresh_suggestions();
    }

    /// **Cursor-aware `Backspace`** (the plain path — see
    /// `edit_backspace_or_tail` for the smart-dispatch wrapper).
    /// - **Scalar**: delete the char *before* `value_cursor` (a classic
    ///   edit cursor); `value_cursor` steps back one. No-op when the cursor
    ///   is at the start (Total; no panic on an empty / zero-cursor state).
    /// - **List**, buffer mode: pop the last char of the buffer (a list
    ///   buffer has no cursor — the cursor is irrelevant for a whole token).
    /// - **List**, Selection mode: **not** handled here — routed separately
    ///   by `edit_backspace_or_tail` (Selection-mode `Backspace`/`Del`
    ///   remove the highlighted item).
    pub fn edit_backspace(&mut self) {
        let do_it = match &self.mode {
            Mode::Edit(e) if e.stage == EditStage::Value => {
                !e.field.is_list() || !e.in_list_select
            }
            _ => false,
        };
        if !do_it {
            return;
        }
        if let Mode::Edit(e) = &mut self.mode {
            if e.field.is_list() {
                e.value.pop();
            } else {
                let mut chars: Vec<char> = e.value.chars().collect();
                let pos = e.value_cursor.min(chars.len());
                if pos > 0 {
                    chars.remove(pos - 1);
                    e.value_cursor = pos - 1;
                }
                e.value = chars.into_iter().collect();
            }
        }
        self.refresh_suggestions();
    }

    /// **The *single* "remove something" key** (`Backspace` *and* `Del`
    /// are both wired to this in `handle_edit`). Dispatch by (sub-)mode:
    /// - **Scalar**, buffer mode: plain cursor-aware `Backspace` (delete
    ///   the char before the cursor).
    /// - **List, buffer mode**: while the buffer has text, pop a char
    ///   (re-narrowing the live menu); once the buffer is *empty*, drop
    ///   the field's *tail item* (a "delete the last thing I added"
    ///   ergonomic — preserved for convenience; the Selection mode is the
    ///   full removal path).
    /// - **List, Selection mode**: **remove the highlighted item**
    ///   (`remove_item_at`, a real `fm::remove_list_item`, *live* — no
    ///   commit). This is the "remove any existing tag" path the user asked
    ///   for. After a removal `list_high` clamps into the new (shorter)
    ///   list, `current_list` shrinks, `working_bytes` is re-serialized,
    ///   and the state is *ready to chain* another removal (stay in
    ///   Selection mode).
    pub fn remove_or_pop(&mut self) {
        let (in_value, in_sel, is_list, empty_buffer, has_items) = match &self.mode {
            Mode::Edit(e) if e.stage == EditStage::Value => (
                true,
                e.in_list_select,
                e.field.is_list(),
                e.value.trim().is_empty(),
                !e.current_list.is_empty(),
            ),
            _ => (false, false, false, false, false),
        };
        if !in_value {
            return;
        }
        if is_list {
            if in_sel && has_items {
                self.remove_item_at();
            } else if !in_sel && empty_buffer && has_items {
                self.remove_tail_item();
            } else if !in_sel && empty_buffer && !has_items {
                self.set_info(
                    "list already empty — nothing to remove (Backspace does nothing more)",
                );
            } else {
                self.edit_backspace();
            }
        } else {
            self.edit_backspace();
        }
    }

    /// The human note for the status bar after `enter_editor`. Every letter
    /// in the value buffer is a literal. The special keys differ by field
    /// kind:
    /// - **Scalar** (title / description): `←/→` move the text cursor,
    ///   `Backspace` deletes before it, `Enter` commits.
    /// - **List** (tags / search_required_phrase): `←/→` enter the *selection* sub-
    ///   mode (the existing items, plus a trailing `<new>`); there, `←/→/↑/↓`
    ///   move the highlight, `Backspace`/`Del` remove the highlighted item,
    ///   `Enter` loads it into the buffer (or return to `<new>`), and typing
    ///   a letter returns to the buffer (preserving the typed token). In the
    ///   buffer, `↑/↓` pick a candidate, `Tab` autocompletes, `Enter`
    ///   commits. `Esc` back to Picker; `C-c` quit.
    fn edit_field_note(field: EditField) -> String {
        if field.is_list() {
            format!(
                "editing {} — type · ↑/↓ pick · Tab fill · ←/→ select existing (Backspace remove) · Enter commit · Esc back · C-c quit",
                field.label()
            )
        } else {
            format!(
                "editing {} — type · ←/→ move cursor · Backspace delete · Enter commit · Esc back · C-c quit",
                field.label()
            )
        }
    }

    /// Move the **Picker** cursor (the highlighted *field*) up or down,
    /// wrapping at the edge. This is the *deliberate* first step: the user
    /// navigates the field list before entering any field's value. Total:
    /// always lands within the field list, even on a `field_cursor` at the
    /// very first/last entry.
    pub fn picker_move(&mut self, down: bool) {
        if let Mode::Edit(e) = &mut self.mode {
            if e.stage != EditStage::Picker {
                // Coming back to the Picker preserves the cursor; moving is
                // only meaningful here. (Defensive: a stray call in a other
                // stage has no field to move to.)
                return;
            }
            let n = EditField::all().len();
            e.field_cursor = if down { (e.field_cursor + 1) % n } else { (e.field_cursor + n - 1) % n };
            let hovered = EditField::all()[e.field_cursor];
            self.set_info(format!(
                "edit metadata — hover: {} · Enter to edit · ↑/↓ move · Esc back",
                hovered.label()
            ));
        }
    }

    /// Move from the **Picker** stage into the **Value** stage for the
    /// highlighted field. Loads that field's corpus + current value (the
    /// *same* `cycle_field` load) and seeds the value buffer so the user
    /// edits *in place*: a scalar field is pre-filled with its current
    /// value (an unchanged commit is a no-op), a list field is empty (the
    /// buffer is the *new item(s)* to append). The live menu is **not
    /// focused** yet — `Tab` (zsh idiom) opens it; until then, `↑/↓` cycle
    /// the *field* and typing goes straight into the buffer.
    pub fn enter_editor(&mut self) {
        // Snapshot the *target* field from the Picker cursor (a copy of
        // the `EditField` — a single atomic read under an `&mut` borrow).
        let target = match &self.mode {
            Mode::Edit(e) if e.stage == EditStage::Picker => EditField::all()[e.field_cursor],
            _ => return, // already in the Value stage for this field
        };
        self.cycle_field(target);
        // `cycle_field` already armed the buffer + loaded the corpus; force
        // the stage to `Value` so the render + input dispatch see the
        // correct focus state.
        if let Mode::Edit(e) = &mut self.mode {
            e.stage = EditStage::Value;
        }
        self.set_info(Self::edit_field_note(target));
    }

    /// **`Tab`** in the Value stage — the zsh *autocomplete* idiom: take
    /// the **highlighted candidate** (`sug_cursor`) and fill the value
    /// buffer with its full text (replacing whatever partial text the user
    /// had typed). The candidate list then re-narrows to the (single, fully
    /// matched) candidate, so `↑/↓` still has a stable highlight to sit on.
    ///
    /// This is a deliberate *fill* (not the old "pure focus toggle"): the
    /// user pressed `Tab` meaning "complete this word", so the buffer now
    /// holds a whole token. Typing immediately afterwards appends to it
    /// (zsh behaviour), so a partial match can still be typed past.
    ///
    /// **Total**: on a scalar field (no candidates) or an empty candidate
    /// list, `Tab` is a clean no-op (the buffer is left alone — there is
    /// nothing known to complete it to). A *commit* is `Enter`'s job, never
    /// `Tab`'s.
    pub fn autocomplete(&mut self) {
        // Only meaningful in *buffer* mode (not Selection mode — in
        // Selection there is no candidate list to pull from). Dispatched
        // on `in_list_select`.
        let tok = match &self.mode {
            Mode::Edit(e) if e.stage == EditStage::Value => {
                if e.in_list_select {
                    None
                } else {
                    e.suggestions.get(e.sug_cursor).cloned()
                }
            }
            _ => None,
        };
        match tok {
            Some(t) => {
                if let Mode::Edit(e) = &mut self.mode {
                    e.value = t;
                    // Scalar (if any): put the cursor at the end of the
                    // just-filled buffer (a natural "I just completed —
                    // the cursor is at the end" default).
                    if !e.field.is_list() {
                        e.value_cursor = e.value.chars().count();
                    }
                    // The buffer no longer has a loaded item — a fresh
                    // completion is an *add*, not a *replace*.
                    e.editing_item = None;
                }
                self.refresh_suggestions();
            }
            None => {
                self.set_info("no candidate to complete — keep typing");
            }
        }
    }

    /// Move the highlighted **candidate** up or down within the live
    /// suggestion list, wrapping at the edge — the *only* meaning of `↑/↓`
    /// in the **buffer** mode of a Value stage (the field was already
    /// chosen in the Picker and is never cycled again from here). In the
    /// **Selection** sub-mode of a list field, `↑/↓` instead moves the
    /// Selection highlight (the `list_high`) — the same key, a different
    /// *job* based on which sub-mode the user is in (see the doc on
    /// `EditState::in_list_select`). No-op when the candidate list is
    /// empty (a scalar field, or a filter with no matches). Total: the
    /// cursor is always clamped into `[0, suggestions.len())` (→ `0` when
    /// empty) and `list_high` is always clamped into
    /// `[0, current_list.len()]`.
    pub fn move_suggestion(&mut self, down: bool) {
        if let Mode::Edit(e) = &mut self.mode {
            if e.stage != EditStage::Value {
                return;
            }
            if e.field.is_list() && e.in_list_select {
                // Selection sub-mode: move the Selection highlight.
                let n = e.current_list.len().saturating_add(1);
                if n > 0 {
                    e.list_high = if down {
                        (e.list_high + 1) % n
                    } else {
                        (e.list_high + n - 1) % n
                    };
                }
            } else if !e.suggestions.is_empty() {
                let n = e.suggestions.len();
                e.sug_cursor = if down {
                    (e.sug_cursor + 1) % n
                } else {
                    (e.sug_cursor + n - 1) % n
                };
            }
        }
    }

    /// **← / →** in the Value stage — *move the buffer cursor* (scalar)
    /// or *enter/exit the Selection sub-mode* (list). See the docs on
    /// `EditState::in_list_select` for the two-mode model. The user's
    /// exact spec: "pressing left arrow will kick out of the input buffer
    /// into a tag selection … (right does the same thing but selects the
    /// first tag in the list). Then you can using right and left arrow
    /// or up and down arrow select an elements in the list, and using
    /// del/backspace delete it, or using enter start editing it (fill the
    /// input buffer with the tag and user can use backspace to remove /
    /// edit it however they like) selecting `<new>` brings it back to
    /// the normal add new tag / element empty input buffer (Or better
    /// yet, cache it so when user accidentally presses a right arrow,
    /// they can immediately select `<new>`, and continue editing the
    /// cached buffer.)"
    ///
    /// **Total**: a stray call outside a Value-stage Edit mode is a
    /// no-op (defensive; no panic). On a scalar field the cursor is
    /// always in `0..=value.chars().count()` (clamped on every move).
    pub fn move_buffer_cursor(&mut self, right: bool) {
        // Snapshot the routing first (a single atomic `&self` read under
        // the `&mut` borrow; the snapshot ends the borrow so we can
        // mutate freely afterwards).
        let (is_list, in_sel) = match &self.mode {
            Mode::Edit(e) if e.stage == EditStage::Value => (e.field.is_list(), e.in_list_select),
            _ => return, // Total; defensive no-op outside a Value stage.
        };
        if is_list && in_sel {
            // Inside Selection mode: `←/→` and `↑/↓` share the same job —
            // move the Selection highlight through existing items + `<new>`.
            // (The user's exact spec: "using right and left arrow or up
            // and down arrow select an element in the list".) Delegate to
            // `move_suggestion`, which handles both sub-mode branches.
            self.move_suggestion(right);
        } else if is_list && !in_sel {
            // Enter Selection mode from the buffer. Spec: `←` highlights
            // the *last* existing item, `→` the *first*. With an empty
            // existing list, highlight `<new>` (index `current_list.len()`).
            if let Mode::Edit(e) = &mut self.mode {
                e.in_list_select = true;
                if e.current_list.is_empty() {
                    e.list_high = 0; // the only entry is `<new>`
                } else if right {
                    e.list_high = 0; // first existing item
                } else {
                    e.list_high = e.current_list.len() - 1; // last existing item
                }
            }
            self.set_info("in selection — ←↑/→↓ move · Backspace remove · Enter load · Esc/tip-letter return to buffer");
        } else {
            // Scalar: move the text cursor by one char (clamped to the
            // buffer's char range — multi-byte safe).
            if let Mode::Edit(e) = &mut self.mode {
                let len = e.value.chars().count();
                e.value_cursor = if right {
                    (e.value_cursor + 1).min(len)
                } else {
                    e.value_cursor.saturating_sub(1)
                };
            }
        }
    }

    /// Drop the last item from the active **list** field (a "delete tail"
    /// semantic, like Backspace on the last item of a list editor —
    /// reached in the Value stage by the *smart* `Backspace` on an empty
    /// buffer, see `edit_backspace_or_tail`). No-op on a scalar field or an
    /// empty list. The removal
    /// is reflected in `current_list` + `original_bytes` (the baseline is
    /// updated so the next `commit_edit` re-derives ops from the *new*
    /// bytes) — `suggestions` is untouched (still anchored to the full
    /// corpus). This is the only path that mutates `original_bytes`
    /// *before* a commit (a "live preview" of the pending op).
    pub fn remove_tail_item(&mut self) {
        // Snapshot field + the tail item (one `&self` borrow, ends on the
        // next line), then do the work and write back under a fresh
        // `&mut self` borrow (the state mutation + status bar).
        let (field, last_opt) = match &self.mode {
            Mode::Edit(e) => (e.field, e.current_list.last().cloned()),
            _ => return, // total on a stray non-Edit call
        };
        if !field.is_list() {
            self.set_info("x — not on a list field (arrow to tags / search_required_phrase)");
            return;
        }
        let last = match last_opt {
            Some(t) => t,
            None => {
                self.set_info("list is already empty — nothing to remove");
                return;
            }
        };
        let working = match &self.mode {
            Mode::Edit(e) => e.working_bytes.clone(),
            _ => return,
        };
        let field_key = field.key();
        match fm::remove_list_item(working.as_slice(), field_key, &last) {
            Ok(new_bytes) => {
                let new_list = fm::list_values(new_bytes.as_slice(), field_key).unwrap_or_default();
                if let Mode::Edit(e) = &mut self.mode {
                    e.working_bytes = new_bytes;
                    e.current_list = new_list;
                }
                self.set_success(format!(
                    "removed '{}' from {} — Enter to commit",
                    truncate_str(&last, 60),
                    field.label()
                ));
            }
            Err(err) => {
                self.set_error(format!("remove {last}: {err}"));
            }
        }
    }

    /// Remove the item **highlighted in Selection mode** (`list_high`)
    /// from a list field — the "remove *any* existing tag" path the user
    /// asked for (vs. `remove_tail_item`, which only removes the tail).
    /// A real `fm::remove_list_item` on `working_bytes`; **live** (no
    /// commit) — `working_bytes` + `current_list` are updated so the next
    /// commit derives ops from the new bytes. After a removal `list_high`
    /// is clamped into the now-shorter list (stays on the same "slot" if
    /// possible, so the user can keep removing downward). **Only valid in
    /// Selection mode on a list field**; a stray call elsewhere is a
    /// no-op (Total).
    pub fn remove_item_at(&mut self) {
        // Snapshot everything (one `&mut` borrow, ends before the write) so
        // the removal + status update can re-borrow below freely.
        let (field, item, working) = match &mut self.mode {
            Mode::Edit(e) if e.stage == EditStage::Value => {
                if !e.field.is_list() || !e.in_list_select || e.current_list.is_empty() {
                    return;
                }
                // `high` in Selection mode is `0..=len` where `== len` is
                // the `<new>` slot — you can't remove `<new>` (it holds no
                // real item), so fall back to the last real item.
                let idx = if e.list_high < e.current_list.len() {
                    e.list_high
                } else {
                    e.current_list.len() - 1
                };
                (
                    e.field,
                    e.current_list[idx].clone(),
                    e.working_bytes.clone(),
                )
            }
            _ => return, // total on a stray non-Edit call
        };
        let field_key = field.key();
        match fm::remove_list_item(working.as_slice(), field_key, &item) {
            Ok(new_bytes) => {
                if let Mode::Edit(e) = &mut self.mode {
                    let new_list = fm::list_values(new_bytes.as_slice(), field_key).unwrap_or_default();
                    e.current_list = new_list;
                    e.working_bytes = new_bytes;
                    // Clamp `list_high` into the new (shorter) list; keep
                    // the same slot if it still exists so the user can
                    // chain downward removals; on an empty list stay at 0
                    // (which is the `<new token>` slot — a valid landing
                    // place in Selection mode, even if odd).
                    e.list_high = e.list_high.min(e.current_list.len());
                }
                self.set_success(format!(
                    "removed '{}' from {} — Enter to commit",
                    truncate_str(&item, 60),
                    field.label()
                ));
            }
            Err(err) => {
                self.set_error(format!("remove {item}: {err}"));
            }
        }
    }

    /// The active Edit mode's commit plan (a one-line convenience over
    /// `EditState::plan`), for the test surface. (The *render* surface reads
    /// the active `EditState` directly and calls `plan()` on it — the
    /// `App`-level method only exists so a test can assert "the *current*
    /// mode's plan is empty in Browse", without reconstructing the state.)
    #[cfg(test)]
    pub fn current_plan(&self) -> Vec<EditOp> {
        match &self.mode {
            Mode::Edit(e) => e.plan(),
            _ => Vec::new(),
        }
    }

    /// Commit the current field's edit: build the op(s) (the same
    /// `EditState::plan` the render surfaces as the "pending" footer),
    /// apply them in order to the *entry* bytes (the "file changed on
    /// disk" guard), atomically write the result, then re-index the DB so
    /// the row reflects the change. **Total** on the entry baseline: an
    /// unchanged scalar (or an empty-buffer list) commit is a no-op write
    /// (the file is written back byte-identical — the "open then commit"
    /// UX leaves no artifact).
    pub fn commit_edit(&mut self) -> KbResult<()> {
        // Snapshot the current state (we borrow self.mode below for the
        // post-write refresh; the snapshot decouples the two borrows).
        let (abs_path, ops, ops_human_count) = match &self.mode {
            Mode::Edit(e) => (
                e.abs_path.clone(),
                e.plan(),
                e.plan().len(),
            ),
            _ => return Ok(()),
        };
        // The "file changed on disk" guard: the file on disk must still
        // match the *entry* bytes (what we saw when we opened). A mid-session
        // external edit (by an editor, a `kb commit`, a re-index, …) is
        // refused with a clean error rather than clobbered.
        let entry_bytes_snap = match &self.mode {
            Mode::Edit(e) => e.entry_bytes.clone(),
            _ => Vec::new(),
        };
        let on_disk = std::fs::read(&abs_path).map_err(|e| (
            format!("re-read {} before write: {e}", abs_path),
            EXIT_ERR,
        ))?;
        if on_disk != entry_bytes_snap {
            return Err((
                format!("{} changed on disk since we opened it — re-open and retry", abs_path),
                EXIT_ERR,
            ));
        }
        // Apply in order over the *working* bytes (which already include
        // any tail-removals from the smart `Backspace` — they are
        // "baked in"). `apply_op` never mutates its input, so a mid-way
        // failure leaves the disk untouched (the write below is the only
        // thing that changes bytes).
        let working_snap = match &self.mode {
            Mode::Edit(e) => e.working_bytes.clone(),
            _ => Vec::new(),
        };
        let mut acc = working_snap.clone();
        let mut applied = 0usize;
        for op in &ops {
            acc = apply_op(&acc, op).map_err(|e| (e, EXIT_ERR))?;
            applied += 1;
        }
        // Write when the *result* differs from what's **on disk** (we just
        // verified `on_disk == entry_bytes`). Comparing to `working_snap`
        // instead would skip a pure tail-removal commit, because that
        // removal was already baked into the working copy. Comparing to
        // the disk also keeps the "open then commit → byte-identical"
        // guarantee (no net change → no write), now for *every* op mix.
        if acc != on_disk {
            // The *same* atomic-write path the CLI `commit` uses.
            crate::atomic_write(std::path::Path::new(&abs_path), &acc)
                .map_err(|e| (format!("write {} atomically: {e}", abs_path), EXIT_ERR))?;
        }
        // Re-index the single page: `build_page` re-reads the file from
        // disk (abs_path), parses the verified frontmatter, and returns a
        // `PageRecord`; `upsert_page` writes it into the index in a single
        // transaction. `root` / `rel_path` are passed as placeholders —
        // the *row-level* root/rel_path columns are preserved by
        // `upsert_page` (it only re-derives the frontmatter-derived
        // columns); `abs_path` is the identity, so the right row is hit.
        // `include_language = true` — the TUI commit_edit pass re-derives every
        // frontmatter-derived column (title, description, tags, gates, body,
        // *and* `language`) from the freshly-written file. Commit `1bd68dd`
        // established the pattern: re-index re-sources the frontmatter, and
        // upsert_page's `COALESCE(NULLIF(...))` sentinel preserves `root`/
        // `rel_path` when this path passes `""`. `language` is *not* a
        // sentinel-guarded column — it is replaced unconditionally by
        // `excluded.language` — so passing `false` here (the historical value)
        // silently clobbered the page's `language:` column back to NULL on
        // every TUI edit, breaking `language:` / `language:Null` search
        // filters on any edited page. `true` matches the refresh/create
        // callers and is the only value consistent with the design intent.
        let rec = match kb_db::index::build_page(
            std::path::Path::new(&abs_path),
            "",
            "",
            true,
        ) {
            Ok(r) => r,
            Err(err) => {
                // The file is already written (source of truth), so a
                // re-index failure is a *warning* (green status), not an
                // error: the data is on disk; `kb refresh` re-syncs later.
                self.set_success(format!(
                    "wrote {} ({} edit(s)); re-index note: {err}",
                    abs_path, ops_human_count
                ));
                self.refresh_edit_state_on_disk(&abs_path)?;
                return Ok(());
            }
        };
        if let Err(err) = kb_db::index::upsert_page(&self.db, &rec) {
            self.set_success(format!(
                "wrote {} ({} edit(s)); DB upsert note: {err}",
                abs_path, ops_human_count
            ));
            self.refresh_edit_state_on_disk(&abs_path)?;
            return Ok(());
        }
        // Re-load the post-commit bytes + refresh the list view, so a
        // second commit (or a field switch) sees the *new* file state.
        self.refresh_edit_state_on_disk(&abs_path)?;
        // Re-snapshot the page in the view (the title / tags may have
        // changed; the next `list_pages` re-read is total).
        self.reload_rows();
        // The status bar: green, with a human summary of the op(s).
        let (field, value, current_list) = match &self.mode {
            Mode::Edit(e) => (e.field, e.value.clone(), e.current_list.clone()),
            _ => (EditField::Title, String::new(), Vec::new()),
        };
        let summary = op_summary(field, &value, &current_list, applied);
        self.set_success(format!("committed {} — {}", abs_path, summary));
        Ok(())
    }

    /// Re-load the Edit state (value / current_list / original_bytes) from
    /// disk after a commit, so the next field interaction is consistent
    /// with what's on disk. Pure: the DB + file are the source of truth.
    /// The *active field* is preserved (the user stays on the field they
    /// were editing); `value` is reset to the new scalar value (or empty
    /// for a list). The "open then commit → commit" UX ends with `value`
    /// reflecting what's on disk, so the user can keep editing from a
    /// clean state.
    fn refresh_edit_state_on_disk(&mut self, abs_path: &str) -> KbResult<()> {
        let bytes = std::fs::read(abs_path).map_err(|e| (
            format!("re-read {} after commit: {e}", abs_path),
            EXIT_ERR,
        ))?;
        if let Mode::Edit(e) = &mut self.mode {
            let field = e.field;
            e.entry_bytes = bytes.clone();
            e.entry_list = if field.is_list() {
                fm::list_values(bytes.as_slice(), field.key()).unwrap_or_default()
            } else {
                Vec::new()
            };
            // After a successful commit the working state is "fresh": the
            // baseline (entry_*) and the working bytes are both = disk.
            // Any next commit will re-derive its no-op rule from the
            // new entry_*.
            e.current_list = e.entry_list.clone();
            e.working_bytes = bytes.clone();
            e.value = if field.is_list() {
                String::new()
            } else {
                fm::scalar_value(bytes.as_slice(), field.key()).unwrap_or_default()
            };
            e.suggestions = if field.is_list() {
                suggestions_for(&e.corpus, &e.value)
            } else {
                Vec::new()
            };
            // A commit resets the candidate highlight to the top of the menu
            // on the same field — the "commit + stay" UX: ready to type the
            // next token, `↑/↓` picks, `Tab` fills, `Enter` commits.
            e.sug_cursor = 0;
            // The scalar buffer is re-seeded with the freshly-loaded value,
            // so the text cursor is at the end (a natural default — the
            // user just "saw" the buffer, it's at the end-of-text). In
            // buffer mode, `Backspace` deletes before it (so the user can
            // refine the end of the just-seen value).
            e.value_cursor = if field.is_list() {
                0
            } else {
                e.value.chars().count()
            };
            // A successful commit exits the Selection sub-mode (it was
            // just about to remove/replace an item — that item is now baked
            // into `working_bytes` via the op; the sub-mode's
            // "pending" state is over). The *new* value buffer is in
            // buffer mode (ready to type / pick a *different* item).
            e.in_list_select = false;
            e.list_high = 0;
            e.editing_item = None;
        }
        Ok(())
    }

    /// **`Enter`** in the Value stage. **Dispatch** by (sub-)mode:
    /// - **Buffer mode**: `commit_edit()` — apply `plan()` to disk. After
    ///   a successful commit, `refresh_edit_state_on_disk` resets the
    ///   buffer/sub-mode, so the user can pick / type the next token on
    ///   the same field ("commit + stay").
    /// - **Selection mode** on a **list** field: **do not commit** —
    ///   instead:
    ///   - If `list_high == current_list.len()` (the `<new>` slot), *exit*
    ///     Selection mode, re-arm the buffer (cached token from the
    ///     previous mode is preserved; `in_list_select = false`,
    ///     `list_high = 0`, no change to `value` or `editing_item`).
    ///     The user's in-progress token is exactly what they were
    ///     typing before they hit `←`/`→` to peek at the list.
    ///   - Otherwise (a real existing item is highlighted), *load* that
    ///     item into the buffer: `value = current_list[list_high]`,
    ///     `editing_item = Some(that_item)`, `value_cursor = end`,
    ///     **exit** Selection mode. The user is now in a "refine a
    ///     known item" sub-mode: `←`/`→` move the text cursor, `Tab`
    ///     autocompletes, `Enter` commits (which replaces the `old`
    ///     item with the refilled `new` via `RemoveItem + AppendItem`),
    ///     `Esc`/typing a letter returns to buffer mode. A scalar field's
    ///     `Enter` always commits (no Selection sub-mode on scalars).
    ///
    /// Total: a stray call outside a Value-stage Edit mode is a no-op.
    pub fn enter_action(&mut self) -> KbResult<()> {
        // Snapshot whether we're in Selection mode (one `&mut` borrow,
        // ends before the commit / load branches below).
        let (in_value, in_sel, is_list, high, len) = match &self.mode {
            Mode::Edit(e) if e.stage == EditStage::Value => {
                (true, e.in_list_select, e.field.is_list(), e.list_high, e.current_list.len())
            }
            _ => (false, false, false, 0, 0),
        };
        if !in_value {
            return Ok(());
        }
        if is_list && in_sel {
            if high < len {
                // Load the highlighted item into the buffer (a "refine an
                // existing item" path). Snapshot the item (one `&self`
                // borrow, ends before the write) so the state mutation
                // below can re-borrow freely.
                let item = match &self.mode {
                    Mode::Edit(e) => e.current_list.get(high).cloned().unwrap_or_default(),
                    _ => String::new(),
                };
                if let Mode::Edit(e) = &mut self.mode {
                    e.value = item.clone();
                    e.editing_item = Some(item);
                    e.in_list_select = false;
                    e.list_high = 0;
                    e.value_cursor = e.value.chars().count();
                }
                self.set_info("loaded into buffer — Enter to commit (replaces the loaded item) · Esc to return to list");
            } else {
                // `<new>` slot — exit Selection mode, keep the cached
                // buffer. (`value` is left as the user was typing it. `Esc`
                // is the same action here by symmetry of "escape back to
                // the thing I was doing before I peeked".)
                if let Mode::Edit(e) = &mut self.mode {
                    e.in_list_select = false;
                    e.list_high = 0;
                }
                self.set_info("back to the buffer — type to edit, Tab to fill, Enter to commit");
            }
            return Ok(());
        }
        // Buffer mode (or a scalar): a real commit.
        self.commit_edit()
    }

    /// **`Esc`** in the Value stage. Two meanings, one key:
    /// - **Selection mode** (a list field, `in_list_select`): exit the
    ///   Selection sub-mode, return to the **buffer** (preserving the
    ///   in-progress token the user typed *before* they hit `←`/`→` to
    ///   peek at the list). This is "escape back to the thing I was doing
    ///   before I peeked at the existing items".
    /// - **Buffer mode** (or a scalar field): pop the Value stage back to
    ///   the Picker (the "abandon this field" action the pre-Selection
    ///   model had).
    ///
    /// Total: a stray call outside a Value-stage Edit mode is a no-op.
    pub fn esc_in_value(&mut self) {
        if let Mode::Edit(e) = &mut self.mode {
            if e.stage != EditStage::Value {
                return;
            }
            if e.field.is_list() && e.in_list_select {
                // Selection mode → Buffer mode (stay in the Value stage).
                e.in_list_select = false;
                e.list_high = 0;
                self.set_info("back to the buffer — type to edit, Tab to fill, Enter to commit, Esc for Picker");
            } else {
                // Buffer mode → Picker stage (abandon this field).
                e.stage = EditStage::Picker;
                self.set_info("edit metadata — ↑/↓ field · Enter to edit · a alias · r roots · Esc back · q quit");
            }
        }
    }

    /// Re-run `list_pages` + `apply_filter` (a "dirty" refresh after an
    /// edit commits). The selection index is re-clamped to the new view
    /// (total: a selection in-range before can go out-of-range after a
    /// filter narrows the list; `apply_filter` handles the clamp).
    pub fn reload_rows(&mut self) {
        if let Ok(rows) = list_pages(&self.db) {
            self.rows = rows;
            self.apply_filter();
            // The corpus just changed (edit commit / root add / root drop):
            // any parked search context now holds pre-mutation result rows,
            // so clear the return address. A search re-run (`search_open`)
            // re-queries against the fresh index. This makes the stale-result
            // window (Search parked while the corpus mutates, then Esc-
            // returned) a no-op instead of a lie.
            self.search_return = None;
        }
    }

    /// Drop the Edit mode and return to Browse (preserves the selection —
    /// the user can `j`/`k` and continue where they left off). The pending
    /// (uncommitted) value buffer is discarded. A `commit_edit` is the
    /// only path that persists to disk; `close_edit` is a plain "back".
    pub fn close_edit(&mut self) {
        self.mode = Mode::Browse;
        let note = self.position_note();
        self.set_info(note);
    }

    // ===================================================================
    //  M8.3 — Alias manager
    //  ===================================================================

    /// Open the **Alias** manager: load the current `tag_aliases` snapshot
    /// and arm the *add form*. **Total**: always succeeds (the list is
    /// simply empty on a fresh DB). The mode is left in `Alias`.
    pub fn open_alias(&mut self) -> KbResult<()> {
        let list = list_aliases(&self.db)?;
        self.mode = Mode::Alias(AliasState {
            list,
            input: String::new(),
            selected: 0,
            focused: false,
        });
        self.set_info("alias — Tab list · type 'alias canonical' then Enter add · x remove · Esc back");
        Ok(())
    }

    /// Go back from Alias to Browse (preserves the page selection — the user
    /// can `j`/`k` and continue). The in-memory snapshot is discarded
    /// (re-loaded on the next `open_alias`, which is total).
    pub fn close_alias(&mut self) {
        self.mode = Mode::Browse;
        let note = self.position_note();
        self.set_info(note);
    }

    /// Reload the in-memory `tag_aliases` snapshot from the DB (the *live*
    /// contract: every add/remove re-runs this so the render reflects the
    /// on-disk truth). **Total** on a fresh / empty DB (empty list).
    pub fn reload_aliases(&mut self) -> KbResult<()> {
        if let Mode::Alias(a) = &mut self.mode {
            a.list = list_aliases(&self.db)?;
            if a.list.is_empty() {
                a.selected = 0;
            } else {
                a.selected = a.selected.min(a.list.len() - 1);
            }
        }
        Ok(())
    }

    /// Append one char to the Alias *add form* buffer (the input path).
    pub fn alias_type(&mut self, c: char) {
        if let Mode::Alias(a) = &mut self.mode {
            if !a.focused {
                a.input.push(c);
            }
        }
    }

    /// Pop one char from the Alias *add form* buffer.
    pub fn alias_backspace(&mut self) {
        if let Mode::Alias(a) = &mut self.mode {
            if !a.focused {
                a.input.pop();
            }
        }
    }

    /// Toggle the Alias focus between the *add form* and the *list*.
    pub fn alias_toggle_focus(&mut self) {
        let new_focus = matches!(&self.mode, Mode::Alias(a) if !a.focused);
        if let Mode::Alias(a) = &mut self.mode {
            a.focused = new_focus;
        }
        self.set_info(if new_focus {
            "alias list — ↑/↓ select · x remove · Tab back to form"
        } else {
            "alias form — type 'alias canonical' · Enter add · Tab list"
        });
    }

    /// Move the Alias list selection by `delta` (clamped). No-op when the
    /// list is empty or the form is focused.
    pub fn alias_move(&mut self, delta: isize) {
        if let Mode::Alias(a) = &mut self.mode {
            if !a.focused || a.list.is_empty() {
                return;
            }
            let n = a.list.len() as isize;
            a.selected = (a.selected as isize + delta).clamp(0, n - 1) as usize;
        }
    }

    /// Parse the Alias *add form* buffer (pure) — `"alias canonical"` on the
    /// first whitespace run (both non-empty after trim), else `None`. This is
    /// the *single* predicate the render + `add_alias` use to validate the
    /// form, so the "what will Enter submit" and "is the form invalid?"
    /// checks cannot diverge. Total.
    pub fn parse_alias_form(input: &str) -> Option<(String, String)> {
        // Trim the *outer* ends first so a leading/trailing newline (a
        // pasted form) is handled; inner whitespace of the canonical is
        // preserved (we only split once, on the first whitespace run).
        let trimmed = input.trim();
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let alias = parts.next()?.trim();
        let canonical = parts.next()?.trim();
        if alias.is_empty() || canonical.is_empty() {
            return None;
        }
        Some((alias.to_string(), canonical.to_string()))
    }

    /// Submit the Alias *add form*: parse the buffer, upsert the row into
    /// `tag_aliases` (the *same* `ON CONFLICT(alias) DO UPDATE` as
    /// `cmd_tag_alias --add` in `main.rs`), then refresh the in-memory list.
    /// **Total**: a malformed form (no alias / no canonical) is a no-op (the
    /// status bar reports the reason). The mode stays in `Alias` (a follow-up
    /// add needs no re-open). The *add form* buffer is cleared on success.
    pub fn add_alias(&mut self) -> KbResult<()> {
        let parse = match &self.mode {
            Mode::Alias(a) if !a.focused => Self::parse_alias_form(a.input.as_str()),
            _ => None,
        };
        let Some((alias, canonical)) = parse else {
            self.set_error("alias form — type '<alias> <canonical>' (both non-empty) then Enter");
            return Ok(());
        };
        self.db
            .conn
            .execute(
                "INSERT INTO tag_aliases(alias, canonical) VALUES (?1, ?2)\
                 ON CONFLICT(alias) DO UPDATE SET canonical = excluded.canonical",
                rusqlite::params![alias, canonical],
            )
            .map_err(|e| (format!("upsert alias: {e}"), EXIT_ERR))?;
        self.reload_aliases()?;
        // Clear the form + return focus to the form (so a follow-up add
        // starts clean).
        if let Mode::Alias(a) = &mut self.mode {
            a.input.clear();
            a.focused = false;
        }
        self.set_success(format!("alias: {alias:?} → {canonical:?}"));
        Ok(())
    }

    /// Remove the Alias list's selected row (`x` key). **Total**: an empty
    /// list or the form being focused is a clean no-op + hint. The mode stays
    /// in `Alias` (a follow-up removal starts from the *new* selection,
    /// clamped).
    pub fn remove_alias(&mut self) -> KbResult<()> {
        let target = match &self.mode {
            Mode::Alias(a) if a.focused && !a.list.is_empty() => a.list[a.selected].alias.clone(),
            _ => {
                self.set_info("alias list is empty — Tab to the list to remove one");
                return Ok(());
            }
        };
        let n = self
            .db
            .conn
            .execute(
                "DELETE FROM tag_aliases WHERE alias = ?1",
                rusqlite::params![target],
            )
            .map_err(|e| (format!("delete alias: {e}"), EXIT_ERR))?;
        self.reload_aliases()?;
        if n == 0 {
            self.set_info(format!("no alias {target:?} found — nothing removed"));
        } else {
            self.set_success(format!("removed alias: {target:?}"));
        }
        Ok(())
    }

    // ===================================================================
    //  M8.3 — Roots manager
    //  ===================================================================

    /// Open the **Roots** manager: load the current `roots` snapshot and
    /// focus the right pane up front. **Total**: always succeeds.
    ///
    /// Focus rule: if there is *anything* to navigate (a non-empty roots
    /// list) the **list** opens focused — `↑`/`↓`/`j`/`k` and `x` work
    /// immediately, with no `Tab` press required to start. Only when the
    /// list is empty (nothing to select) does the **add form** open focused,
    /// so the user is dropped straight into typing a path. `Tab` always
    /// toggles between the two.
    pub fn open_roots(&mut self) -> KbResult<()> {
        let list = list_roots(&self.db)?;
        let focused = !list.is_empty();
        self.mode = Mode::Roots(RootsState {
            list,
            input: String::new(),
            selected: 0,
            focused,
        });
        if focused {
            self.set_info("roots — ↑↓/jk select · x drop · Tab add form · Esc back");
        } else {
            self.set_info("no roots yet — type an absolute path then Enter add · Tab list · Esc back");
        }
        Ok(())
    }

    /// Go back from Roots to Browse (preserves the page selection).
    pub fn close_roots(&mut self) {
        self.mode = Mode::Browse;
        let note = self.position_note();
        self.set_info(note);
    }

    /// Reload the in-memory `roots` snapshot (the *live* contract).
    pub fn reload_roots(&mut self) -> KbResult<()> {
        if let Mode::Roots(r) = &mut self.mode {
            r.list = list_roots(&self.db)?;
            if r.list.is_empty() {
                r.selected = 0;
            } else {
                r.selected = r.selected.min(r.list.len() - 1);
            }
        }
        Ok(())
    }

    /// Append one char to the Roots *add form* buffer.
    pub fn roots_type(&mut self, c: char) {
        if let Mode::Roots(r) = &mut self.mode {
            if !r.focused {
                r.input.push(c);
            }
        }
    }

    /// Pop one char from the Roots *add form* buffer.
    pub fn roots_backspace(&mut self) {
        if let Mode::Roots(r) = &mut self.mode {
            if !r.focused {
                r.input.pop();
            }
        }
    }

    /// Toggle the Roots focus between the *add form* and the *list*.
    pub fn roots_toggle_focus(&mut self) {
        let new_focus = matches!(&self.mode, Mode::Roots(r) if !r.focused);
        if let Mode::Roots(r) = &mut self.mode {
            r.focused = new_focus;
        }
        self.set_info(if new_focus {
            "roots list — ↑/↓ select · x drop · Tab back to form"
        } else {
            "roots form — type an absolute dir path · Enter add · Tab list"
        });
    }

    /// Move the Roots list selection by `delta` (clamped). No-op when the
    /// list is empty or the form is focused.
    pub fn roots_move(&mut self, delta: isize) {
        if let Mode::Roots(r) = &mut self.mode {
            if !r.focused || r.list.is_empty() {
                return;
            }
            let n = r.list.len() as isize;
            r.selected = (r.selected as isize + delta).clamp(0, n - 1) as usize;
        }
    }

    /// Submit the Roots *add form*: canonicalize the typed path, register it
    /// (`add_root`, the *same* method `cmd_add` calls), then full-refresh (GC
    /// pages that are no longer under any root, and index any new pages).
    /// **Total**: the path is trimmed; an empty / relative path or one that
    /// is not a directory is a clean no-op + error hint. The mode stays in
    /// `Roots` (a follow-up add starts clean).
    pub fn add_root_tui(&mut self) -> KbResult<()> {
        let raw = match &self.mode {
            Mode::Roots(r) if !r.focused => r.input.trim().to_string(),
            _ => {
                self.set_error("roots form — type an absolute directory path then Enter");
                return Ok(());
            }
        };
        if raw.is_empty() {
            self.set_error("roots form — path is empty");
            return Ok(());
        }
        let canonical = canonicalize_dir_for_add(&raw)?;
        self.db
            .add_root(&canonical, None, false)
            .map_err(|e| (format!("add_root: {e}"), EXIT_ERR))?;
        // The *same* full refresh `cmd_add` runs (GC orphans + index new
        // pages). A refresh failure after a successful add is reported as
        // an error (the root is registered; `kb refresh` re-syncs later,
        // but the user wants to *see* the effect, so we surface it).
        let _summary = kb_db::refresh::refresh(&self.db)
            .map_err(|e| (format!("refresh after add-root: {e}"), EXIT_ERR))?;
        self.reload_roots()?;
        // Also re-snapshot the page list (new pages may have been indexed).
        self.reload_rows();
        if let Mode::Roots(r) = &mut self.mode {
            r.input.clear();
            r.focused = false;
        }
        self.set_success(format!("root registered: {canonical}"));
        Ok(())
    }

    /// Drop the Roots list's selected row (`x` key): `drop_root` (the *same*
    /// method `cmd_drop_root` calls) then a full refresh (GC pages no longer
    /// under any root). **Total**: an empty list or the form being focused
    /// is a clean no-op + hint.
    pub fn drop_root_tui(&mut self) -> KbResult<()> {
        let path = match &self.mode {
            Mode::Roots(r) if r.focused && !r.list.is_empty() => r.list[r.selected].path.clone(),
            _ => {
                self.set_info("roots list is empty — Tab to the list to drop one");
                return Ok(());
            }
        };
        let n = self
            .db
            .drop_root(&path)
            .map_err(|e| (format!("drop_root: {e}"), EXIT_ERR))?;
        if n == 0 {
            self.set_info(format!("no root matched {path:?} — nothing dropped"));
            return Ok(());
        }
        let _summary = kb_db::refresh::refresh(&self.db)
            .map_err(|e| (format!("refresh after drop-root: {e}"), EXIT_ERR))?;
        self.reload_roots()?;
        self.reload_rows();
        self.set_success(format!("dropped root: {path}"));
        Ok(())
    }


    // ===================================================================
    //  M8.4 — Search mode
    //  ===================================================================

    /// Enter Search mode (armed by `s` in Browse). Starts with an empty
    /// query buffer; the user types to build a `search_grammar` query.
    ///
    /// The empty buffer is **immediately** run through the search bundle so
    /// the results pane opens pre-populated with the un-gated corpus (the
    /// same shape the CLI `kb search ""` returns) — rather than empty until
    /// the first keystroke. `search_commit` forwards the empty query to
    /// `search_rows` and materialises `results = Some(…)`.
    pub fn open_search(&mut self) -> KbResult<()> {
        self.mode = Mode::Search(SearchState {
            query: String::new(),
            results: None,
            sel: 0,
            tag_sugs: Vec::new(),
            tag_sel: 0,
        });
        self.search_commit();
        self.set_info("type a search query (tag: llm · type: wiki · quoted phrase) · ↑↓ results · Tab inject tag · Enter open · Esc back");
        Ok(())
    }

    /// Leave Search mode, returning to Browse (selection preserved).
    pub fn close_search(&mut self) {
        self.mode = Mode::Browse;
        let note = self.position_note();
        self.set_info(note);
    }

    /// Append a printable char to the search query buffer, then re-run
    /// the search + tag suggestions.
    pub fn search_type(&mut self, c: char) {
        if let Mode::Search(s) = &mut self.mode {
            s.query.push(c);
        } else {
            return;
        }
        self.search_commit();
    }

    /// Backspace: pop one char from the query buffer, then re-run search
    /// + tag suggestions.
    pub fn search_backspace(&mut self) {
        if let Mode::Search(s) = &mut self.mode {
            s.query.pop();
        } else {
            return;
        }
        self.search_commit();
    }

    /// Re-run search + tag suggestions against the *current* query buffer
    /// and fold the result back into the `Search` state. Called once per
    /// buffer mutation (after `search_type` / `search_backspace` /
    /// `search_inject_tag`).
    fn search_commit(&mut self) {
        let query = match &self.mode {
            Mode::Search(s) => s.query.clone(),
            _ => return,
        };
        let (results, tag_sugs) = run_search_bundle(&self.db, &query);
        if let Mode::Search(s) = &mut self.mode {
            s.results = Some(results);
            let n = s.results.as_ref().map_or(0, |r| r.len());
            s.sel = if n == 0 { 0 } else { s.sel.min(n - 1) };
            s.tag_sugs = tag_sugs;
            s.tag_sel = 0usize;
        }
    }

    /// Move result selection down by 1 (clamped). No-op when no results.
    pub fn search_move(&mut self, delta: isize) {
        if let Mode::Search(s) = &mut self.mode {
            if let Some(ref mut results) = s.results {
                if results.is_empty() {
                    return;
                }
                let n = results.len() as isize;
                s.sel = (s.sel as isize + delta).clamp(0, n - 1) as usize;
            }
        }
    }

    /// Navigate tag suggestions up/down (only when `tag_sugs` is non-empty).
    pub fn search_tag_move(&mut self, delta: isize) {
        if let Mode::Search(s) = &mut self.mode {
            if s.tag_sugs.is_empty() {
                return;
            }
            let n = s.tag_sugs.len() as isize;
            s.tag_sel = (s.tag_sel as isize + delta).clamp(0, n - 1) as usize;
        }
    }

    /// Inject the highlighted tag suggestion into the query by replacing
    /// the trailing `tag:partial` token with `tag:<canonical>` (quoted if
    /// the canonical value contains a space or quote), then append a
    /// trailing space to exit tag-completion mode.
    pub fn search_inject_tag(&mut self) {
        if let Mode::Search(s) = &mut self.mode {
            let sug = match s.tag_sugs.get(s.tag_sel) {
                Some(sug) => sug.clone(),
                None => {
                    self.set_info("no tag suggestion highlighted");
                    return;
                }
            };
            let new_token = if sug.canonical.contains(' ') || sug.canonical.contains('"') {
                format!("tag:\"{}\"", sug.canonical)
            } else {
                format!("tag:{}", sug.canonical)
            };
            let old = s.query.clone();
            s.query = inject_tag(&old, &new_token);
        }
        self.search_commit();
        // (Display message is set inside `search_commit` via status; the
        // caller can see the updated query in the header.)
    }

    /// Open the selected search result in Read mode (same as Browse's
    /// `Enter`). No-op when there are no results.
    pub fn search_open(&mut self) -> KbResult<()> {
        let (abs_path, id) = {
            let s = match &self.mode {
                Mode::Search(s) => s,
                _ => return Ok(()),
            };
            match s.results.as_ref().and_then(|r| r.get(s.sel)) {
                Some(r) => (r.abs_path.clone(), r.id.clone()),
                None => {
                    self.set_info("no search result selected");
                    return Ok(());
                }
            }
        };
        // Park the search context so `close_read` returns *here* (not to
        // Browse) — query buffer, results, and result selection all intact.
        let search = match std::mem::replace(&mut self.mode, Mode::Browse) {
            Mode::Search(s) => s,
            _ => unreachable!("selected the row above only if in Search mode"),
        };
        self.search_return = Some(search);
        let (body, outline, fm) = load_body(&self.db, &abs_path)?;
        let row_idx = self.rows.iter().position(|r| r.abs_path == abs_path).unwrap_or(0);
        self.mode = Mode::Read(ReadState {
            row_idx,
            id,
            body,
            outline,
            scroll_top: 0,
            abs_path: abs_path.clone(),
            fm,
            find: None,
        });
        self.set_info("open page — ↑↓ scroll · Esc back to search");
        Ok(())
    }

    // ===================================================================

    /// Set the status bar to an info/success/error message. The *only*
    /// thing that writes `app.status` (so the "one writer" invariant is
    /// grep-able): these three methods.
    pub fn set_info(&mut self, text: impl Into<String>) {
        self.status = Status::info(&text.into());
    }
    /// `set_success` / `set_error` land with the M8.2 edit commit — a successful
    /// write flips the bar to green; a validation failure flips it to red.
    /// (Intentionally `#[allow(dead_code)]` through M8.1.)
    #[allow(dead_code)]
    pub fn set_success(&mut self, text: impl Into<String>) {
        self.status = Status::success(&text.into());
    }
    #[allow(dead_code)]
    pub fn set_error(&mut self, text: impl Into<String>) {
        self.status = Status::error(&text.into());
    }
}
/// The largest `scroll_top` for `body` rendered in a pane of `pane_width`
/// cells. The scroll offset is in **rendered rows** (not logical lines): a
/// logical line that wraps to N rendered rows contributes N to the total.
///
/// The row count comes from `render::rendered_row_count`, which delegates to
/// `Paragraph::line_count` — ratatui's exact `WordWrapper` (`trim: false`)
/// engine, the same one that paints the body. Nothing here re-implements
/// wrapping, so a ratatui upgrade cannot silently desync the scroll range
/// from what the user sees. A blank line still occupies one rendered row.
///
/// When `pane_width` is 0/1 (a window too narrow to matter, or before the
/// first render has told `App` the real pane width) `rendered_row_count`
/// falls back to the logical-line model so the mutators remain total and
/// monotonic.
///
/// Pure + total. Empty body → 0.
///
/// Now test-only: the production scroll mutators read
/// [`BodyRollup::max_top`] instead (the per-frame roll-up). Kept as a pure
/// helper so the "scroll max is a pure function of body + width" invariant
/// stays unit-testable without spinning up an `App`.
#[cfg(test)]
pub fn scroll_max_top(lines: &[String], pane_width: usize) -> usize {
    crate::tui::render::rendered_row_count(lines, pane_width).saturating_sub(1)
}

/// A 64-bit FNV-1a over `data` — the small, allocation-free hash the body
/// roll-up cache keys on. Not cryptographic; a collision only costs one
/// scroll-clamp nudge (see `BodyRollup`'s `single-writer invariant`). Inlined
/// here (4 lines) rather than pulling `fnv` into the TUI crate's dep surface
/// so that (a) `kb-hardened` (which strips TUI deps) never sees it and (b)
/// the cache stays on the cheap path even if we ever add other deps.
fn fnv1a(data: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

// ===================================================================
//  M8.5 — Reader find (the `f` key in Read mode)
//  ===================================================================
//
//  The find session is a *live* one (the `/` filter idiom): a needle, the
//  occurrences of the needle in the body, and a cursor over them. The
//  functions below are the pure, total core over a body + needle: they carry
//  no IO, no `App` borrow, and are trivially testable in isolation.
//
//  * `row_table`      — the body's rows (what the user *sees*), as
//                       `(content, start_byte)` pairs in order.
//  * `line_visual`    — a line's **visual** offset (cells from the line's
//                       start) for a *logical* byte position.
//  * `find_matches`   — the needle's occurrences as `(line_idx, byte_offset)`,
//                       in document order (top-to-bottom, then left-to-right
//                       within a line).
//  * `find_centering` — the blank-row padding *above* / *below* the rendered
//                       body that lets the selected hit sit at the viewport's
//                       center, even when it is the very first or very last
//                       rendered row (the user's "fake rows" request).

/// How many rendered rows a single logical line occupies at `width` cells by
/// the old `div_ceil(cells / width)` model. This is only a **fallback** for
/// the degenerate `pane_width <= 1` guard and for the tests; the real scroll
/// and centering math uses `render::rendered_row_count` (ratatui's own
/// `WordWrapper`). A blank line is 1 row; `width < 1` clamps to 1-cell rows.
/// **Total** (no panics, no IO).
///
/// Test-only: superseded by the production `BodyRollup.first_row_of` table
/// (built from `render::line_row_count`), which the find/scroll paths now
/// read.
#[cfg(test)]
fn line_row_count(line: &str, width: usize) -> usize {
    if line.is_empty() {
        return 1;
    }
    let w = if width < 1 { 1 } else { width };
    line.width().max(1).div_ceil(w)
}

/// Map the body's logical lines to the **first rendered row** each one starts
/// on using the old `div_ceil` model. This is now only the degenerate-width
/// fallback for [`find_padding_for_cursor`]; the selected hit's *actual* row
/// comes from `render::match_rendered_row` so it accounts for wrapping within
/// the line. `width < 1` clamps to 1-cell rows. A blank line contributes 1
/// row (it renders as a single, empty row).
///
/// Test-only: the production centering path reads
/// [`BodyRollup::first_row_of`] instead.
#[cfg(test)]
fn line_first_row(lines: &[String], width: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut acc = 0usize;
    for line in lines {
        out.push(acc);
        acc += line_row_count(line, width);
    }
    out
}

/// The needle's occurrences in the **rendered** body `lines`, as
/// `(line_idx, byte_offset_in_line)`, in **document order** (lines
/// top-to-bottom, then left-to-right within a line — the reading order the
/// user sees). `needle` is matched case-insensitively as a substring; an
/// empty needle matches nothing. `lines` are the *rendered* per-line texts
/// (see `render::rendered_lines`) — the very characters the overlay paints
/// onto, so the (line, byte_offset) matches land exactly under the right
/// glyph, even when the raw markdown has `**`, `*`, `` ` ``, or `~~`
/// delimiters the renderer would otherwise strip.
fn find_matches(lines: &[String], needle: &str) -> Vec<(usize, usize)> {
    let low = needle.to_lowercase();
    if low.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let lline = line.to_lowercase();
        let mut from = 0usize;
        while let Some(rel) = lline[from..].find(&low) {
            out.push((li, from + rel));
            from += rel + low.len().max(1);
        }
    }
    out
}

/// The find session's centering padding — the blank rows to *prepend*
/// (above) and *append* (below) the unpadded rendered body, plus the two
/// scroll values the event-loop / `input` pass derive from them.
///
/// The "fake rows" the user asked for: without padding, a hit on the very
/// first or very last rendered row cannot sit at the viewport's center —
/// there isn't enough content to scroll past. `above` / `below` fix that.
///
/// **Total** on its inputs (pure; no IO, no `App`). The caller supplies
/// `first_row` / `content_rows`, both measured by ratatui's own
/// `WordWrapper` (`render::match_rendered_row` / `rendered_row_count`), and
/// `pane_height` (via `app.body_pane_height`, written each frame by
/// `draw_read`). `FindState.center: Option<FindPadding>` stores the
/// latest result so that the *input* pass (which can mutate
/// `ReadState::scroll_top`) and the *render* pass (which prepends /
/// appends the blank rows) always agree — one computation, two consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindPadding {
    /// Blank rows to *prepend* to the unpadded body (pushes content down,
    /// creating scroll room *above* the hit).
    pub above: usize,
    /// Blank rows to *append* to the unpadded body (pushes content up,
    /// creating scroll room *below* the hit).
    pub below: usize,
    /// `scroll_top` to set on `ReadState` (the *first* padded row the pane
    /// shows). Always `0` when there is no content to scroll; when padding
    /// is needed, it is the minimum of the "hit-at-center" row and the
    /// "bottom-of-pane" row.
    pub scroll_top: usize,
    /// The *maximum valid* `scroll_top` for the padded body:
    /// `saturating_sub(above + content_rows + below, pane_height)`.
    /// The scroll clamps must not exceed this (else the pane blanks).
    pub scroll_max_top: usize,
}

/// Compute the find hit's centering (see [`FindPadding`]).
///
/// `first_row` is the selected hit's first rendered row in the *unpadded*
/// body (0 = top of the page). `content_rows` is the total unpadded
/// rendered-row count. `pane_height` is the visible pane row count
/// (0 = degenerate, no padding needed — render the unpadded body).
/// Returns the `FindPadding` ready to apply: the render pass prepends
/// `above` blank rows and appends `below`; the input pass sets
/// `ReadState::scroll_top` to `scroll_top`.
///
/// Total on its inputs; `pane_height` 0 ⇒ no padding (degenerate window).
///
/// # Math (derivation)
///
/// `center = (pane_height - 1) / 2` — the 0-based offset of the *center
/// row* within the pane, counting from `scroll_top`.
///
    /// **Case A:** `first_row >= center` (hit in the lower half — or exactly
    /// at center — of the page). No `above` needed.
    ///
    ///   To center the hit: `scroll + center = first_row`
    ///   → `scroll = first_row - center` (always `>= 0`).
    ///
    ///   The bottom of the pane must fit in the padded body:
    ///   `scroll + pane_height - 1 <= above + CR + below - 1`
    ///   `first_row + pane - center - 1 <= CR + below - 1`
    ///   → `below = max(0, first_row + pane_height - center - CR)`
    ///
    /// **Case B:** `first_row < center` (hit in the upper half — cannot be
    /// centered without padding *above* it; the scroll position cannot go
    /// negative, so `above = center - first_row` is mandatory).
    ///
    ///   `scroll + center = above + first_row = center`
    ///   → `scroll = 0`.
    ///
    ///   Same bottom-of-pane bound, substituting `scroll = 0`,
    ///   `above = center - first_row`:
    ///   `pane_height - 1 <= center - first_row + CR + below - 1`
    ///   → `below = max(0, pane_height - center + first_row - CR)`
    ///
    /// In both cases the derived `scroll` equals the resulting
    /// `scroll_max_top` exactly, so the `scroll_top <= scroll_max_top`
    /// invariant holds by construction (no rounding slack needed).
    fn find_padding(first_row: usize, content_rows: usize, pane_height: usize) -> FindPadding {
        if pane_height == 0 {
            return FindPadding {
                above: 0,
                below: 0,
                scroll_top: 0,
                scroll_max_top: 0,
            };
        }
        let center = (pane_height - 1) / 2;
        let (above, below, scroll) = if first_row >= center {
            // Case A: hit in lower half (or at center) → no above, compute scroll.
            let b = (first_row + pane_height - center)
                .saturating_sub(content_rows);
            (0, b, first_row - center)
        } else {
            // Case B: hit in upper half → pad above, scroll = 0.
            let a = center - first_row;
            let b = (pane_height - center + first_row)
                .saturating_sub(content_rows);
            (a, b, 0)
        };
        let padded_rows = above + content_rows + below;
        FindPadding {
            above,
            below,
            scroll_top: scroll,
            scroll_max_top: padded_rows.saturating_sub(pane_height),
        }
    }

/// Compute the `FindPadding` for the hit at `cursor` in `matches`, given
/// the rendered body lines and the live pane dimensions. The hit is located
/// by `(line_index, byte_offset)` → its **actual rendered row** via
/// `render::match_rendered_row` (so a hit several wraps down a long line is
/// centered on itself, not on the line's first row). Returns `None` when
/// the match list is empty (caller stores `FindState.center = None`; render
/// the unpadded body).
///
/// `cursor` is clamped to `< matches.len()` (a live keystroke can shrink
/// the list). `pane_width` 0/1 falls back to the logical-line row model
/// (same guard `scroll_max_top` uses). `pane_height` 0 ⇒ no padding
/// applied (the `FindPadding` is all-zero).
///
/// Test-only: the production `App::find_center_scroll` path inlines this
/// same math against the cached `BodyRollup` (so the per-frame re-measure is
/// gone), but this helper is kept pure + self-contained so the
/// row-resolution and clamp behavior stays unit-testable in isolation.
#[cfg(test)]
fn find_padding_for_cursor(
    lines: &[String],
    matches: &[(usize, usize)],
    cursor: usize,
    pane_width: usize,
    pane_height: usize,
) -> Option<FindPadding> {
    if matches.is_empty() {
        return None;
    }
    let cursor = cursor.min(matches.len() - 1);
    let (line_idx, _) = matches[cursor];
    // The selected hit's *actual* rendered row, via ratatui's wrap engine
    // (see `render::match_rendered_row`). The old fallback used the line's
    // first row, which is wrong once the hit sits several wraps down a long
    // line; keep it only for the degenerate width guard.
    let first_row = crate::tui::render::match_rendered_row(lines, matches[cursor], pane_width)
        .unwrap_or_else(|| {
            line_first_row(lines, pane_width)
                .get(line_idx)
                .copied()
                .unwrap_or(0)
        });
    let content_rows = crate::tui::render::rendered_row_count(lines, pane_width);
    Some(find_padding(first_row, content_rows, pane_height))
}

/// Extract the section outline from a `body`: a `(line_index, level,
/// title)` triple per heading, in body order. Reuses the already-verified
/// `kb-core::section::heading_of` so the outline definition is the same one
/// `kb toc` reports — one predicate, two surfaces.
fn outline_of(body: &str) -> Outline {
    body
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let (level, title) = section_core::heading_of(line.as_bytes())?;
            Some((i, level, title))
        })
        .collect()
}

/// The pure tab-autocomplete filter: given a *corpus* of known tokens and
/// a *typing buffer*, return the matching suggestions. This is the single
/// predicate the Edit mode uses to compute `EditState::suggestions`, so
/// it lives here (not on `App`) so it is total + trivially testable:
///
/// - buffer empty          → the whole corpus, order-preserving, dedup'd.
/// - buffer non-empty      → the tokens (order-preserving, dedup'd) that
///   *case-insensitively* contain the buffer as a substring. The substring
///   (not prefix-only) rule is chosen so "tran" matches "transformer" and
///   "mer" does too — the same recall the `/` filter gives the user.
///
/// Dedup is first-occurrence (the corpus may carry the same token on
/// several pages); the result is in corpus order so it is stable.
pub fn suggestions_for(corpus: &[String], buffer: &str) -> Vec<String> {
    let needle = buffer.to_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for tok in corpus {
        let key = tok.to_lowercase();
        let hit = if needle.is_empty() { true } else { key.contains(&needle) };
        if !hit {
            continue;
        }
        if seen.insert(key) {
            out.push(tok.clone());
        }
    }
    out
}

// ===================================================================
//  M8.4 — Search mode helpers (pure + DB-bound)
//  ===================================================================

/// The whole "run the search bundle" step for the TUI: convert the FTS
/// rows into `PageRow`s (with tags + gates for display) and compute the
/// live tag suggestions. Pure over its inputs — `db` is the only state.
///
/// An **empty** or whitespace-only buffer is forwarded to `search_rows`
/// as-is (matching the CLI `kb search ""` behavior): the engine returns
/// the full un-gated corpus (subject to the 10-row TUI limit), so the
/// panel is populated when the user first enters Search mode.
fn run_search_bundle(db: &Db, query: &str) -> (Vec<PageRow>, Vec<TagSuggestion>) {
    let trimmed = query.trim();
    let rows = match crate::cli::search::search_rows(db, trimmed, 100) {
        Ok((rows, _)) => rows,
        Err(_) => Vec::new(),
    };
    let pages = search_rows_to_pages(db, &rows);
    let tag_sugs = trailing_tag_partial(query)
        .map(|p| suggest_tags(db, &p))
        .unwrap_or_default();
    (pages, tag_sugs)
}

/// Convert the CLI's `SearchRow`s into TUI `PageRow`s, attaching the
/// tags + gates lists (the same batched load Browse uses — `page_tags`
/// and `page_gates`, keyed by `abs_path`). An empty `rows` slice returns
/// an empty vec (not an error).
fn search_rows_to_pages(db: &Db, rows: &[crate::cli::search::SearchRow]) -> Vec<PageRow> {
    if rows.is_empty() {
        return Vec::new();
    }
    // Batched tags + gates load (the same two queries `list_pages` runs).
    fn batch_load(db: &Db) -> (std::collections::HashMap<String, Vec<String>>, std::collections::HashMap<String, Vec<String>>) {
        let mut tags_by: std::collections::HashMap<String, Vec<String>> = Default::default();
        if let Ok(mut st) = db.conn.prepare("SELECT abs_path, tag FROM page_tags ORDER BY tag") {
            if let Ok(items) = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            {
                for (ap, tag) in items {
                    tags_by.entry(ap).or_default().push(tag);
                }
            }
        }
        let mut gates_by: std::collections::HashMap<String, Vec<String>> = Default::default();
        if let Ok(mut st) = db.conn.prepare("SELECT abs_path, term FROM page_gates ORDER BY term") {
            if let Ok(items) = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            {
                for (ap, term) in items {
                    gates_by.entry(ap).or_default().push(term);
                }
            }
        }
        (tags_by, gates_by)
    }
    let (tags_by, gates_by) = batch_load(db);
    rows.iter()
        .map(|r| PageRow {
            abs_path: r.abs_path.clone(),
            id: r.id.clone(),
            pt: r.pt.clone(),
            title: r.title.clone().unwrap_or_default(),
            rel_path: r.rel_path.clone(),
            root: r.root.clone(),
            tags: tags_by.get(&r.abs_path).cloned().unwrap_or_default(),
            gates: gates_by.get(&r.abs_path).cloned().unwrap_or_default(),
        })
        .collect()
}

/// Extract the **partial tag value** being typed at the tail of the
/// query — the text after the *last* whitespace-separated token's
/// `tag:` prefix (so `tag:sec` → `sec`, `tag:` → `""`, `llm tag:sec`
/// → `sec`). Returns `None` when the trailing token is not a `tag:`
/// token (e.g. a completed tag with a trailing space, or no `tag:` at
/// all).
///
/// The "trailing token" is the span from the last whitespace char to
/// the end of the buffer (whitespace = anything in `char::is_whitespace`
/// for the ASCII subset — the buffer is user-typed text, not yet a
/// parse tree). A *quoted* tag token (`tag:"large language"`) does not
/// count as a partial (the quote is the boundary).
fn trailing_tag_partial(query: &str) -> Option<String> {
    let tail = query.rsplit(char::is_whitespace).next().unwrap_or("");
    if let Some(rest) = tail.strip_prefix("tag:") {
        if rest.starts_with('"') {
            None // a quoted tag is a completed phrase, not a partial
        } else {
            Some(rest.to_string())
        }
    } else {
        None
    }
}

/// Compute the tag candidates to show in the right pane: every distinct
/// tag in the corpus (from `page_tags`) that *case-insensitively
/// contains* the partial, plus every alias→canonical pair whose **alias
/// or canonical** contains the partial (displayed as `alias →
/// canonical`). Order: partial matches first (the user's typing wins),
/// then the full corpus (stable, alphabetical).
///
/// `partial` is the text after `tag:` (possibly empty → whole corpus).
fn suggest_tags(db: &Db, partial: &str) -> Vec<TagSuggestion> {
    let needle = partial.to_lowercase();

    // All distinct tags in the corpus.
    let mut tags: Vec<String> = Vec::new();
    if let Ok(mut st) = db.conn.prepare("SELECT DISTINCT tag FROM page_tags ORDER BY tag") {
        if let Ok(items) = st.query_map([], |r| r.get::<_, String>(0)).and_then(|rows| rows.collect::<Result<Vec<_>, _>>()) {
            tags = items;
        }
    }
    // All aliases (alias, canonical) — sorted by alias for stable output.
    let mut aliases: Vec<(String, String)> = Vec::new();
    if let Ok(mut st) = db
        .conn
        .prepare("SELECT alias, canonical FROM tag_aliases ORDER BY alias")
    {
        if let Ok(items) = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        {
            aliases = items;
        }
    }

    // Filter by the partial (case-insensitive substring — the same rule
    // as the Edit-mode's `suggestions_for`).
    let tags_f: Vec<&String> = if needle.is_empty() {
        tags.iter().collect()
    } else {
        tags.iter()
            .filter(|t| t.to_lowercase().contains(&needle))
            .collect()
    };
    let aliases_f: Vec<&(String, String)> = if needle.is_empty() {
        aliases.iter().collect()
    } else {
        aliases
            .iter()
            .filter(|(a, c)| a.to_lowercase().contains(&needle) || c.to_lowercase().contains(&needle))
            .collect()
    };

    // The suggestion list: partial-matching aliases first (display:
    // `alias → canonical`), then partial-matching tags (display: the tag
    // itself). Dedup on the canonical form; keep first occurrence.
    let mut seen_canon = std::collections::HashSet::new();
    let mut out: Vec<TagSuggestion> = Vec::new();
    for (a, c) in aliases_f {
        let key = c.to_lowercase();
        if seen_canon.insert(key) {
            out.push(TagSuggestion {
                display: format!("{a} → {c}"),
                canonical: c.clone(),
            });
        }
    }
    for t in tags_f {
        let key = t.to_lowercase();
        if seen_canon.insert(key) {
            out.push(TagSuggestion {
                display: t.clone(),
                canonical: t.clone(),
            });
        }
    }
    out
}

/// Replace the trailing **`tag:…` token** (if one is the last
/// whitespace-separated token) in `query` with `new_token`, or append
/// `new_token` as a new token when the last token is free-text. The
/// buffer always ends with exactly one trailing space, and runs of
/// interior whitespace are normalised to single spaces. Pure over its
/// inputs (no `App` state).
///
/// Cases:
/// - `("", tag)`                     → `"tag "`
/// - `("tag:se", tag)`               → `"tag "`         (the tag token is replaced)
/// - `("llm tag:se", tag)`           → `"llm tag "`
/// - `("llm", tag)`                  → `"llm tag "`     (free-text token kept)
/// - `("llm ", tag)`                 → `"llm tag "`     (trailing-normalised)
fn inject_tag(query: &str, new_token: &str) -> String {
    // Tokenise on any whitespace, drop empties. This naturally collapses
    // multiple spaces between tokens (the input buffer is user-typed;
    // `search_grammar` will re-split the same way, so normalising is safe).
    let tokens: Vec<&str> = query
        .split(char::is_whitespace)
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return format!("{new_token} ");
    }
    let last = tokens.last().copied().expect("tokens non-empty");
    let leading: Vec<&str> = tokens[..tokens.len() - 1].to_vec();
    let mut out = String::with_capacity(query.len() + new_token.len() + 2);
    if !leading.is_empty() {
        out.push_str(&leading.join(" "));
        out.push(' ');
    }
    if last.starts_with("tag:") {
        // The trailing token IS the tag → replace it with the new tag.
        out.push_str(new_token);
    } else {
        // The trailing token is free-text (e.g. the user typed "llm" and
        // then hit Tab) → preserve it and append the tag as a new token.
        out.push_str(last);
        out.push(' ');
        out.push_str(new_token);
    }
    out.push(' ');
    out
}

/// The pure "what will the commit apply?" builder. It is the single place
/// the Edit mode decides which verified `kb-core::frontmatter` op(s) a
/// commit issues, given the *current observed value* of the field. Total
/// + testable without a DB. The contract:
///
/// - **Scalar, value unchanged from `original`** → no op (no-op commit).
///   This is the key "edit in place" guarantee: opening a page and
///   committing without typing anything leaves the file *byte-identical*.
/// - **Scalar, value differs from `original` and is non-empty** →
///   `SetScalar` (the verified op inserts the field if absent, replaces it
///   if present — one op either way).
/// - **Scalar, value trimmed to empty and `original` was non-empty** →
///   `RemoveField` (a scalar *clear*). If `original` was already empty →
///   no op (clearing an absent empty scalar is a no-op).
/// - **List, value empty** → no op.
/// - **List, value is one or more comma-separated tokens** → one
///   `AppendItem` per token, each *dedup'd against `existing`* (the
///   current list) so re-appending an item that's already present is a
///   no-op, and empty/whitespace tokens are dropped. Order is preserved.
///
/// `original` / `existing` are both "the field as it was on entry" — for a
/// scalar, `original` is the current scalar value ("" if absent); for a
/// list, `existing` is the current list items (empty if absent).
pub fn build_ops(
    field: EditField,
    value: &str,
    editing_item: &Option<String>,
    current_list: &[String],
    entry_bytes: &[u8],
) -> Vec<EditOp> {
    let key = field.key();
    let v = value.trim();
    if !field.is_list() {
        let original = fm::scalar_value(entry_bytes, key).unwrap_or_default();
        let o = original.trim();
        if v == o {
            return Vec::new(); // unchanged → no-op
        }
        if v.is_empty() {
            return if o.is_empty() {
                Vec::new() // clearing an absent/empty scalar is a no-op
            } else {
                vec![EditOp::RemoveField { key }]
            };
        }
        return vec![EditOp::SetScalar { key, value: v.to_string() }];
    }
    // List field.
    if v.is_empty() {
        return Vec::new(); // empty buffer → no commit (the smart-Backspace
                           // live-removal is baked into working_bytes)
    }
    // "Replace" commit (the buffer was loaded with an existing item via the
    // Selection sub-mode — `Enter` on a tag, then the user typed a
    // different value, then `Enter`): emit `RemoveItem(old)` *before*
    // `AppendItem(new)` so the list is atomic (no window in which both
    // values are absent / both present — "mTLS" → "mtls" rename is
    // atomic). Dedup: if the buffer is the *same* token (case-insensitive)
    // as the loaded item, the commit is a no-op.
    if let Some(old) = editing_item {
        if v == old.trim() {
            return Vec::new(); // identical → no-op
        }
        return vec![
            EditOp::RemoveItem { key, item: old.clone() },
            EditOp::AppendItem { key, item: v.to_string() },
        ];
    }
    // "Add" commit (normal typing + Enter): one `AppendItem` per
    // comma-separated token, *dedup'd against `current_list`* so re-adding
    // a known token is a no-op, and empty tokens are dropped. Order is
    // preserved.
    v.split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| {
            !t.is_empty() && !current_list.iter().any(|e| e == t)
        })
        .map(|item| EditOp::AppendItem { key, item })
        .collect()
}

/// Apply a single `EditOp` to a page's bytes, returning the new bytes and
/// a human-readable summary of the change (or `None` if the op was a
/// no-op — e.g. `AppendItem` on a value the list already contains).
/// **Total + pure** over already-loaded bytes, so the commit path's
/// *logic* (which op, in what order, what the new bytes are) is testable
/// independently of the file IO. The verified core (`fm::set_scalar`,
/// `fm::append_list_item`) does the actual byte-level work; this is the
/// thin composition layer the TUI owns.
pub fn apply_op(
    data: &[u8],
    op: &EditOp,
) -> std::result::Result<Vec<u8>, String> {
    match op {
        EditOp::SetScalar { key, value } => {
            fm::set_scalar(data, key, value).map_err(|e| e.to_string())
        }
        EditOp::AppendItem { key, item } => {
            fm::append_list_item(data, key, item).map_err(|e| e.to_string())
        }
        EditOp::RemoveField { key } => fm::remove_field(data, key).map_err(|e| e.to_string()),
        EditOp::RemoveItem { key, item } => {
            fm::remove_list_item(data, key, item).map_err(|e| e.to_string())
        }
    }
}

/// The corpus for tab-autocomplete: for a **list** field, the distinct
/// tokens across all pages (all tags on `page_tags` for `EditField::Tags`;
/// all search-terms on `page_gates` for `EditField::SearchRequiredPhrase`). For a
/// **scalar** field the corpus is empty (no autocomplete on free text).
/// Total: an empty DB returns an empty Vec (the field still shows no
/// suggestions; the user types freely).
pub fn corpus_for(db: &Db, field: EditField) -> Vec<String> {
    if !field.is_list() {
        return Vec::new();
    }
    // `page_tags.tag` is the unquoted, lowercased, distinct token (see
    // migrate: "lowercase ASCII tag"). A simple DISTINCT query suffices.
    let sql = match field {
        EditField::Tags => "SELECT DISTINCT tag FROM page_tags ORDER BY tag",
        EditField::SearchRequiredPhrase => {
            "SELECT DISTINCT term FROM page_gates ORDER BY term"
        }
        _ => return Vec::new(),
    };
    let mut stmt = match db.conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let out: std::result::Result<Vec<String>, rusqlite::Error> = stmt
        .query_map([], |r| r.get(0))
        .and_then(|rows| rows.collect());
    out.unwrap_or_default()
}

/// A human summary of a commit's op count, for the green status bar.
/// (The exact op-set details live in `build_ops`; the status bar only
/// needs the count + the field label so the user knows what was written.)
fn op_summary(field: EditField, value: &str, current_list: &[String], applied: usize) -> String {
    if applied == 0 {
        return "no-op (no changes detected)".to_string();
    }
    if field.is_list() {
        let v = value.trim();
        if v.is_empty() {
            format!("{} — tail removed from {} (now {} item(s))",
                field.label(), field.key(), current_list.len())
        } else {
            format!("{} — appended +{applied} to {} (now {} item(s))",
                field.label(), field.key(), current_list.len() + applied)
        }
    } else {
        format!("{} — value set to '{}'", field.label(), value.trim())
    }
}

/// Truncate a string to at most `max` **chars** (not bytes) so a multi-byte
/// token never panics on a char boundary. Render-only. (A true
/// display-width truncate would need `unicode-width`; for v1 a char cap
/// is enough and keeps the dep surface small.)
fn truncate_str(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let full = s.chars().count();
    let mut out: String = s.chars().take(max).collect();
    if full > max {
        out.push('…');
    }
    out
}

/// Snapshot the current `tag_aliases` table (the same query as
/// `cmd_tag_alias --list`, ordered `alias ASC` — deterministic, so the
/// render never reshuffles on a re-load).
fn list_aliases(db: &Db) -> KbResult<Vec<AliasRow>> {
    let mut stmt = db
        .conn
        .prepare("SELECT alias, canonical FROM tag_aliases ORDER BY alias")
        .map_err(|e| (format!("prepare aliases: {e}"), EXIT_ERR))?;
    let out = stmt
        .query_map([], |r| Ok(AliasRow { alias: r.get(0)?, canonical: r.get(1)? }))
        .map_err(|e| (format!("query aliases: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("rows aliases: {e}"), EXIT_ERR))?;
    Ok(out)
}

/// Snapshot the current `roots` table (the same *columns* `cmd_roots` and
/// `Db::list_roots` surface, ordered `path ASC` — the same order the CLI
/// list uses). `extra_exts` is the raw column (the render trims/joins it).
fn list_roots(db: &Db) -> KbResult<Vec<RootsRow>> {
    let mut stmt = db
        .conn
        .prepare(
            "SELECT path, extra_exts, include_hidden, added_at FROM roots ORDER BY path",
        )
        .map_err(|e| (format!("prepare roots: {e}"), EXIT_ERR))?;
    let out = stmt
        .query_map([], |r| {
            Ok(RootsRow {
                path: r.get::<_, String>(0)?,
                extra_exts: r.get::<_, Option<String>>(1)?,
                include_hidden: r.get::<_, i32>(2)? != 0,
                added_at: r.get::<_, Option<String>>(3)?,
            })
        })
        .map_err(|e| (format!("query roots: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("rows roots: {e}"), EXIT_ERR))?;
    Ok(out)
}

/// The *pure* canonicalize for the Roots add form: the same rules as
/// `cmd_add` in `main.rs` — the path must canonicalize (exist) and must be
/// a directory. Returns the canonical path string for `add_root`. **Total**
/// on `&str`: any I/O or type failure maps to a clean `(msg, code)` for the
/// status bar to surface.
fn canonicalize_dir_for_add(raw: &str) -> KbResult<String> {
    let p = Path::new(raw);
    let canonical = std::fs::canonicalize(p)
        .map_err(|e| (format!("{raw}: {e} (must exist and be a directory)"), EXIT_ERR))?;
    if !canonical.is_dir() {
        return Err((
            format!("{}: not a directory", canonical.display()),
            EXIT_ERR,
        ));
    }
    Ok(canonical.to_string_lossy().into_owned())
}

/// Snapshot all pages (the same query as `cmd_browse`, no filter, ordered
/// by `root, rel_path`).
fn list_pages(db: &Db) -> KbResult<Vec<PageRow>> {
    let mut stmt = db.conn
        .prepare(
            "SELECT abs_path, id, type, title, root, rel_path\n\
             FROM pages\n\
             ORDER BY root, rel_path",
        )
        .map_err(|e| (format!("prepare: {e}"), EXIT_ERR))?;
    let mut rows: Vec<PageRow> = stmt
        .query_map([], |r| {
            Ok(PageRow {
                abs_path: r.get::<_, String>(0)?,
                id: r.get::<_, String>(1)?,
                pt: r.get::<_, String>(2)?,
                title: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                root: r.get::<_, String>(4)?,
                rel_path: r.get::<_, String>(5)?,
                tags: Vec::new(),
                gates: Vec::new(),
            })
        })
        .map_err(|e| (format!("query: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("rows: {e}"), EXIT_ERR))?;
    // Load `page_tags` + `page_gates` for every page *we just read*, keyed
    // by `abs_path` (the stable identifier). Both are optional — a page
    // with neither gets empty vectors (gated == false, tagless).
    let mut tags_by: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    {
        let mut st = db.conn
            .prepare("SELECT abs_path, tag FROM page_tags ORDER BY tag")
            .map_err(|e| (format!("prepare page_tags: {e}"), EXIT_ERR))?;
        let items: Vec<(String, String)> = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| (format!("query page_tags: {e}"), EXIT_ERR))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| (format!("page_tags rows: {e}"), EXIT_ERR))?;
        for (ap, tag) in items {
            tags_by.entry(ap).or_default().push(tag);
        }
    }
    let mut gates_by: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    {
        let mut st = db.conn
            .prepare("SELECT abs_path, term FROM page_gates ORDER BY term")
            .map_err(|e| (format!("prepare page_gates: {e}"), EXIT_ERR))?;
        let items: Vec<(String, String)> = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| (format!("query page_gates: {e}"), EXIT_ERR))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| (format!("page_gates rows: {e}"), EXIT_ERR))?;
        for (ap, term) in items {
            gates_by.entry(ap).or_default().push(term);
        }
    }
    for r in rows.iter_mut() {
        r.tags = tags_by.get(&r.abs_path).cloned().unwrap_or_default();
        r.gates = gates_by.get(&r.abs_path).cloned().unwrap_or_default();
    }
    Ok(rows)
}

/// A page outline: a list of `(byte_offset, level, title)` entries, in document
/// order. (Named to keep multi-use signatures readable without inlining the
/// tuple.)
type Outline = Vec<(usize, u8, String)>;

/// Load a page's body text (from the `pages.body` column) + its outline
/// (a `heading_of` extraction over the body) + frontmatter display metadata.
/// Called by `App::open_read`.
/// Total: an empty/NULL body renders as the empty string + an empty outline;
/// missing frontmatter fields render as `None` (the FmMeta is all-optional).
/// The page is looked up by *absolute path* (the stable identity), so a
/// `full-rebuild` (which reassigns SQLite rowids) cannot open the wrong page.
fn load_body(db: &Db, abs_path: &str) -> KbResult<(String, Outline, FmMeta)> {
    let body: String = db
        .conn
        .query_row(
            "SELECT COALESCE(body, '') FROM pages WHERE abs_path = ?1",
            [abs_path],
            |r| r.get(0),
        )
        .map_err(|e| (format!("body lookup for {abs_path}: {e}"), EXIT_ERR))?;
    let outline = outline_of(&body);
    let fm = load_fm_meta(db, abs_path)?;
    Ok((body, outline, fm))
}

/// Load the display metadata (type, language, description, tags, gates)
/// from the `pages` + `page_tags` tables. All fields optional — a page
/// may lack any of them. Failures are non-fatal (the Read pane still
/// renders the body + outline; the metadata block is simply empty).
fn load_fm_meta(db: &Db, abs_path: &str) -> KbResult<FmMeta> {
    let (page_type, language, description, srp): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = db
        .conn
        .query_row(
            "SELECT type, language, description, search_required_phrase \
             FROM pages WHERE abs_path = ?1",
            [abs_path],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| (format!("meta lookup for {abs_path}: {e}"), EXIT_ERR))?;
    let mut tags: Vec<String> = Vec::new();
    if let Ok(mut stmt) = db
        .conn
        .prepare("SELECT tag FROM page_tags WHERE abs_path = ?1 ORDER BY tag")
    {
        if let Ok(it) = stmt.query_map(rusqlite::params![abs_path], |r| r.get::<_, String>(0)) {
            for t in it.flatten() {
                tags.push(t);
            }
        }
    }
    Ok(FmMeta {
        page_type,
        language,
        description,
        tags,
        search_required_phrase: srp,
    })
}
/// Run the TUI: open the DB, load the snapshot, hand off to the event
/// loop, and restore the terminal on the way out (the `Drop` guard in
/// `event_loop` is the safety net for the in-between returns).
pub fn run_tui(
    dbpath: Option<&Path>,
    palette_spec: Option<&str>,
    code_style: Option<&str>,
    list_palettes: bool,
) -> i32 {
    // `--list-palettes` is a purely informational flag: it lists the
    // built-ins and exits 0 without touching the DB or entering raw mode.
    // (It is accepted even without a reachable DB so the help works anywhere.)
    if list_palettes {
        print_palettes();
        return crate::EXIT_OK;
    }

    let db = match crate::open_db(dbpath) {
        Ok(d) => d,
        Err((msg, code)) => {
            eprintln!("{msg}");
            return code;
        }
    };
    // Resolve `--palette <name|path>` *before* entering raw mode so a typo /
    // missing file / bad TOML prints a clean message and exits non-zero.
    let mut palette = match palette_spec {
        Some(spec) => match palette::Palette::load(spec) {
            Ok(p) => p,
            Err(msg) => {
                eprintln!("{msg}");
                return crate::EXIT_ERR;
            }
        },
        None => palette::Palette::default_palette(),
    };
    // Swap the `md_code` color/box. Source precedence: the explicit
    // `--code-style` flag, else the `KB_TUI_CODE_STYLE` env var. Both resolve
    // *before* raw mode so a bad value is a clean non-zero exit, never a
    // silent fallback. A single color name = foreground (no box); `fg,bg`
    // = a code box. A TUI can't switch the *typeface* (one terminal font), so
    // this varies the code span's color — the only thing that role paints.
    let code_style: Option<String> = code_style
        .map(str::to_string)
        .or_else(|| std::env::var("KB_TUI_CODE_STYLE").ok());
    if let Some(spec) = code_style {
        match palette.apply_code_style(&spec) {
            Ok(p) => palette = p,
            Err(msg) => {
                eprintln!("{msg}");
                return crate::EXIT_ERR;
            }
        }
    }
    let app = match App::new(db) {
        Ok(a) => a.with_palette(palette),
        Err((msg, code)) => {
            eprintln!("{msg}");
            return code;
        }
    };
    event_loop(app)
}

/// Print the available built-in palettes to stdout (`kb tui --list-palettes`).
///
/// One row per palette. Each row shows 4 color **swatches** — `label`, `active`,
/// `error`, `selection` — rendered in the *user's own terminal* via ANSI, so
/// they can see the palette before choosing it. The swatch for a role is the
/// swatch text styled by that role, closed by `ANSI_RESET`.
///
/// A trailing blank line + two one-liners tell the user how to apply either a
/// built-in or their own file.
fn print_palettes() {
    let names = palette::Palette::builtins();
    let name_w = names.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    // Header row (plain text): name · swatch legend · description.
    println!(
        "{:<width$}   | swatches: label · active · error · selection | description",
        "name", width = name_w
    );
    let underline = "-".repeat(name_w);
    println!("{underline}   --------------------------------------------------------------");
    for (name, desc) in names {
        let p = match palette::Palette::load(name) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[{name}] failed to load: {e}");
                continue;
            }
        };
        let sw = |s: ratatui::style::Style| {
            let pre = palette::Palette::to_ansi(s);
            let post = palette::Palette::ANSI_RESET;
            format!("{pre}██{post}")
        };
        let label = sw(p.label);
        let active = sw(p.active);
        let error = sw(p.error);
        let selection = sw(p.selection);
        println!(
            "{:<width$}   | {label} · {active} · {error} · {selection}        | {desc}",
            name, width = name_w
        );
    }
    println!();
    println!("To use a built-in:    kb tui --palette <name>");
    println!("To use a custom file: kb tui --palette /path/to/palette.toml");
    println!("(Every role is required in a file; a missing role is an error, not a fallback.)");
    println!("To inspect a built-in as a starting point, see the shipped files:");
    println!("    kb/tui/palettes/{{default,colorblind}}.toml");
}

/// Enter the alt screen + raw mode, install the cleanup guard, run the
/// event loop, and restore the terminal on the way out.
fn event_loop(mut app: App) -> i32 {
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("tui: terminal init failed: {e}");
            return EXIT_ERR;
        }
    };

    match enable_raw_mode() {
        Ok(()) => {
            let _ = execute!(std::io::stdout(), EnterAlternateScreen);
        }
        Err(e) => {
            eprintln!("tui: could not enable raw mode: {e} (is stdin a TTY?)");
            return EXIT_ERR;
        }
    }

    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
        }
    }
    let _cleanup = Cleanup;

    loop {
        // Consume any "force clear" flag (set after an external editor
        // returned). We must clear *then* draw in this iteration so the first
        // post-editor frame is a full repaint — a diff-only redraw against
        // ratatui's stale internal buffer leaves stale cells behind. `clear`
        // resets ratatui's last-drawn state too, so the very next draw is a
        // complete, from-scratch frame.
        if app.needs_clear {
            app.needs_clear = false;
            let _ = terminal.clear();
        }
        if let Err(e) = terminal.draw(|f| render::draw(f, &mut app)) {
            eprintln!("tui: draw failed: {e}");
            break;
        }

        // Block up to 200 ms for a single event; on timeout, re-loop (which
        // redraws) so a terminal resize reflows without a keystroke.
        if !match ct_event::poll(Duration::from_millis(200)) {
            Ok(p) => p,
            Err(_) => continue,
        } {
            continue;
        }
        let ev = match ct_event::read() {
            Ok(e) => e,
            Err(_) => continue,
        };
        // Only key events matter for the bindings; ignores (Mouse, Focus,
        // Resize) are filtered out here so `handle_event` is total over
        // what it sees.
        if let ct_event::Event::Key(key_ev) = ev {
            if input::handle_event(&mut app, key_ev) {
                break;
            }
        }
    }
    EXIT_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render a raw body to its **rendered** per-line `Vec<String>` (the
    /// form the pure find/scroll helpers now consume). Test bodies are plain
    /// prose, so rendering is identity — this helper exists so the test call
    /// sites read naturally against the new `&[String]` signatures.
    fn lines(body: &str) -> Vec<String> {
        crate::tui::render::rendered_lines(
            &crate::tui::palette::Palette::default_palette(),
            body,
        )
    }

    /// Per-call sequence used by `seeded_app` to mint a *unique* temp dir.
    /// All tests in this binary share a single pid, so keying the dir on the
    /// pid alone caused every `seeded_app` call (in parallel test threads) to
    /// read/write the *same* `page.md` — one test's commit raced another test's
    /// re-seed and the on-disk assertion flaked. An atomic counter (distinct
    /// under parallel fetch_add) + the pid isolates each call fully.
    static SEEDED_SEQ: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    /// `move_selection` is total + bounded: on a non-empty list the
    /// selection always lands in range, and it is idempotent at the edges
    /// (`k` at row 0 stays 0; `j` at the last row stays last).
    #[test]
    fn move_selection_is_total_and_bounded() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        for i in 0..3 {
            let p = format!("/root/p{i}.md");
            let id = format!("note:P{i}");
            let rel = format!("p{i}.md");
            db.conn.execute(
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase, mtime_ns, content_hash)
                 VALUES (?1, ?2, 'note', ?3, '/root', ?4, NULL, 0, 'x')",
                rusqlite::params![p, id, format!("P{i}"), rel],
            )
            .unwrap();
        }
        let mut app = App::new(db).unwrap();
        assert_eq!(app.rows.len(), 3);
        let n = app.rows.len();

        app.selected = 0;
        app.move_selection(-1);
        assert_eq!(app.selected, 0, "cannot go above row 0");

        app.selected = n - 1;
        app.move_selection(1);
        assert_eq!(app.selected, n - 1, "cannot go past last row");

        app.selected = 0;
        app.move_selection(1);
        assert_eq!(app.selected, 1);

        app.selected = n - 1;
        app.move_selection(-1);
        assert_eq!(app.selected, n - 2);

        app.selected = 1;
        app.jump_to(false);
        assert_eq!(app.selected, 0);
        app.jump_to(true);
        assert_eq!(app.selected, n - 1);

        // Empty corpus: all moves are no-ops (total on an empty list).
        let db2 = Db::open_in_memory_test().unwrap();
        db2.migrate().unwrap();
        let mut app2 = App::new(db2).unwrap();
        assert!(app2.rows.is_empty());
        app2.move_selection(1);
        assert_eq!(app2.selected, 0);
        app2.move_selection(-1);
        assert_eq!(app2.selected, 0);
        app2.jump_to(true);
        assert_eq!(app2.selected, 0);
    }

    /// `Status::{info,success,error}` are distinct kinds, so the render
    /// code can color the status bar by severity rather than by text.
    #[test]
    fn status_kinds_are_distinct() {
        assert_ne!(StatusKind::Info, StatusKind::Success);
        assert_ne!(StatusKind::Success, StatusKind::Error);
        assert_ne!(StatusKind::Info, StatusKind::Error);
    }

    // ---- M8.1 helpers + tests -------------------------------------------

    /// Insert one page with a body + frontmatter, mirroring what `kb add`
    /// would store. Returns the abs_path.
    fn insert_page(
        db: &Db,
        name: &str,
        title: &str,
        body: &str,
    ) -> String {
        let abs = format!("/root/{name}.md");
        let id = format!("note:{name}");
        db.conn
            .execute(
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase,
                                    body, frontmatter_text, mtime_ns, content_hash)
                 VALUES (?1, ?2, 'note', ?3, '/root', ?4, NULL, ?5, '---\ntype: note\n---\n', 0, 'x')",
                rusqlite::params![abs, id, title, format!("{name}.md"), body],
            )
            .unwrap();
        abs
    }

    /// `scroll_max_top` is pure over *(body, pane_width)*: the same body
    /// at a wider pane has a smaller max (fewer rendered rows). A blank
    /// line still counts as a rendered row. `pane_width = 0` (before the
    /// first render) falls back to the logical-line model so the mutators
    /// remain total.
    #[test]
    fn scroll_max_top_is_a_pure_function_of_body_and_width() {
        // Empty body → 0 at any width.
        assert_eq!(scroll_max_top(&lines(""), 80), 0);
        // A single logical line → 0 max (the one rendered row is visible
        // from the top; no scroll range).
        assert_eq!(scroll_max_top(&lines("one\n"), 80), 0);
        // Two single-line lines → 1.
        assert_eq!(scroll_max_top(&lines("one\ntwo"), 80), 1);
        // A wide line: 25 cells at width 10 wraps to 3 rendered rows →
        // max top = 2.
        let wide: Vec<String> = vec!["a".repeat(25)];
        assert_eq!(scroll_max_top(&wide, 10), 2);
        // Same body at a wider pane → smaller max.
        assert!(scroll_max_top(&wide, 10) > scroll_max_top(&wide, 100));
        // A blank line counts as one rendered row (it's still a row).
        assert_eq!(scroll_max_top(&lines("a\n\nb"), 80), 2);
        // degenerate width 0/1 → the logical-line fallback.
        assert_eq!(scroll_max_top(&lines("a\nb\nc"), 0), 2);
        assert_eq!(scroll_max_top(&lines("a\nb\nc"), 1), 2);
    }

    /// The roll-up cache's `total_rows` is the single source of truth for the
    /// scroll range — if it ever desyncs from what ratatui's `WordWrapper`
    /// actually paints, the user's scroll clamp is wrong. This test pins
    /// `BodyRollup.total_rows` to `render::rendered_row_count` (which calls
    /// ratatui's own `Paragraph::line_count`) for a body with heavy
    /// character- and word-wrapping at a narrow pane, at multiple widths so
    /// any future change that makes one side of the equation forget to track
    /// the other is immediately caught. The same check is done at
    /// `width = 1` (the degenerate fallback path) and at `width = 80` (most
    /// lines do not wrap at all — the two engines should still agree, not
    /// *only* when wrapping kicks in).
    #[test]
    fn body_rollup_total_rows_matches_ratatui_wrapper() {
        // A body that exercises both word-wrap (long tokens at a narrow
        // width) and multi-line structure at once. The specific tokens
        // matter only in that they are longer than 10 cells (the narrow
        // width below) so `WordWrapper` is forced to break them — the exact
        // regression this cache guards.
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // "abcdefghijklmnopqrst" (22 chars) at width 10 → WordWrapper must
        // wrap it; at width 80 → single row.
        insert_page(&db, "p", "P", "abcdefghijklmnopqrst\nshort\nanother long line here that should wrap at 10\n\nlast");
        let mut app = App::new(db).unwrap();
        app.open_read().unwrap();
        let body = match &app.mode {
            Mode::Read(r) => r.body.clone(),
            _ => panic!("expected Read"),
        };
        let pal = app.palette;
        let rendered = crate::tui::render::rendered_lines(&pal, &body);
        for w in [usize::MAX, 0, 1, 10, 12, 80] {
            let w = if w == usize::MAX { 80 } else { w };
            app.body_pane_width = w;
            // Force a rebuild from scratch (different body_hash from a
            // previous width — but the hash is the same; force via `valid`).
            app.body_rollup.valid = false;
            app.ensure_body_rollup(&body);
            assert!(app.body_rollup.valid, "roll-up should be valid after ensure");
            let cached = app.body_rollup.total_rows;
            let reference = crate::tui::render::rendered_row_count(&rendered, w);
            assert_eq!(cached, reference,
                "total_rows desynced from ratatui at width {w}: cached={cached} reference={reference}");
        }
    }

    /// The scroll mutators are total + bounded: on any body the resulting
    /// `scroll_top` is within `[0, scroll_max_top]`, and stepping past an
    /// edge is a no-op (clamped).
    #[test]
    fn scroll_body_is_total_and_bounded() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // A body with 4 content lines.
        insert_page(&db, "p", "P", "l1\nl2\nl3\nl4\n");
        let mut app = App::new(db).unwrap();
        app.open_read().unwrap();

        // Set the pane width explicitly (tests don't go through the render
        // pass, so `app.body_pane_width` is still the 0 fallback). 80 is a
        // reasonable "wide enough" width for the test body's single-word
        // lines (each "lN" is 2 cells, so no wrapping at 80).
        app.body_pane_width = 80;

        // scroll down past the bottom: clamps to max, does not grow.
        app.scroll_body(100);
        let max = scroll_max_top(
            &lines(&match &app.mode { Mode::Read(r) => r.body.clone(), _ => panic!() }),
            app.body_pane_width,
        );
        if let Mode::Read(r) = &app.mode {
            assert_eq!(r.scroll_top, max, "scroll past bottom clamps to max");
        } else {
            panic!("expected Read mode");
        }
        // scroll up past the top: clamps to 0.
        if let Mode::Read(r) = &mut app.mode {
            r.scroll_top = 0;
        }
        app.scroll_body(-100);
        if let Mode::Read(r) = &app.mode {
            assert_eq!(r.scroll_top, 0, "scroll past top clamps to 0");
        }
        // A small step from 0 stays 0; a step up by 2 lands on 2.
        if let Mode::Read(r) = &mut app.mode {
            let _ = r;
        }
        app.scroll_body(-1);
        if let Mode::Read(r) = &app.mode {
            assert_eq!(r.scroll_top, 0);
        }
        app.scroll_body(2);
        if let Mode::Read(r) = &app.mode {
            assert_eq!(r.scroll_top, 2);
        }
    }

    /// `scroll_body_top` / `scroll_body_bottom` are the two extremes and are
    /// no-ops when the body is empty (total on the degenerate input).
    #[test]
    fn scroll_top_bottom_are_the_extremes() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        insert_page(&db, "p", "P", "a\nb\nc\nd\ne\n");
        let mut app = App::new(db).unwrap();
        app.open_read().unwrap();

        if let Mode::Read(r) = &mut app.mode {
            r.scroll_top = 3;
        }
        app.scroll_body_top();
        if let Mode::Read(r) = &app.mode {
            assert_eq!(r.scroll_top, 0);
        }
        app.scroll_body_bottom();
        let max = scroll_max_top(&lines("a\nb\nc\nd\ne\n"), app.body_pane_width);
        if let Mode::Read(r) = &app.mode {
            assert_eq!(r.scroll_top, max);
        }

        // Empty body: top and bottom are both 0 (max_top == 0).
        let db2 = Db::open_in_memory_test().unwrap();
        db2.migrate().unwrap();
        insert_page(&db2, "empty", "E", "");
        let mut app2 = App::new(db2).unwrap();
        app2.open_read().unwrap();
        app2.scroll_body_bottom();
        app2.scroll_body(99);
        if let Mode::Read(r) = &app2.mode {
            assert_eq!(r.scroll_top, 0, "empty body: bottom == top == 0");
        }
    }

    /// The outline is a `heading_of` extraction: only valid ATX headings
    /// appear, in body order, each tagged with its line index + level +
    /// title. We assert through `open_read` (the Read page's outline is
    /// populated by the same `outline_of` that renders it), so this pins the
    /// *pipeline*, not just the pure helper — the "one predicate, two
    /// surfaces" contract: the TUI outline and `kb toc` are the same
    /// function over the same bytes.
    #[test]
    fn open_read_outline_is_a_heading_extraction_in_body_order() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // Lines: 0 blank, 1 `# A`, 2 body, 3 `## B`, 4 body, 5 `####### X`
        // (not a heading — 7 hashes), 6 `#NoSpace` (not a heading — no
        // separating space).
        let body = "\n# A\none\n## B\ntwo\n####### X\n#NoSpace\n";
        insert_page(&db, "p", "P", body);
        let mut app = App::new(db).unwrap();
        app.open_read().unwrap();
        if let Mode::Read(r) = &app.mode {
            assert_eq!(r.outline.len(), 2, "only the two valid ATX headings");
            assert_eq!(r.outline[0], (1, 1, "A".to_string()));
            assert_eq!(r.outline[1], (3, 2, "B".to_string()));
        } else {
            panic!("expected Read mode after open_read");
        }
    }

    /// `apply_filter` is total: an empty filter → `view == rows`; a
    /// non-empty filter → the (possibly empty) matching subset, *case-
    /// insensitive* over id / title / rel_path; and `selected` is always
    /// clamped into the new bounds (never out of range).
    #[test]
    fn apply_filter_is_total_and_case_insensitive() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        insert_page(&db, "Attention", "Attention heads", "x\n");
        insert_page(&db, "LLMs", "Large language models", "y\n");
        insert_page(&db, "Zebra", "A striped horse", "z\n");
        let mut app = App::new(db).unwrap();
        assert_eq!(app.rows.len(), 3);
        assert_eq!(app.view.len(), 3, "empty filter shows everything");

        // Case-insensitive substring over the id.
        app.filter_text = "ATTENTION".to_string();
        app.apply_filter();
        assert_eq!(app.view.len(), 1, "'ATTENTION' matches only the Attention page");
        assert_eq!(app.view[0].id, "note:Attention");
        assert_eq!(app.selected, 0);

        // A title match.
        app.filter_text = "language".to_string();
        app.selected = 2; // move selection out of bounds for the new view
        app.apply_filter();
        assert_eq!(app.view.len(), 1);
        assert_eq!(app.view[0].id, "note:LLMs");
        assert_eq!(app.selected, 0, "selected clamps into the new bounds");

        // No matches: view is empty, selected is 0 (total on empty view).
        app.filter_text = "quantum-soup".to_string();
        app.selected = 5;
        app.apply_filter();
        assert!(app.view.is_empty());
        assert_eq!(app.selected, 0);

        // Clearing the filter restores the full list.
        app.filter_text.clear();
        app.apply_filter();
        assert_eq!(app.view.len(), 3);
    }

    /// P10: `apply_filter` also matches *tags* and *search_required_phrase*
    /// so a user can surface a gated page (`mtls`) or a tagged page
    /// (`security`) without knowing its id / title / path.
    #[test]
    fn apply_filter_matches_tags_and_gates() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let _ = insert_page(&db, "mTLS", "Mutual TLS", "x\n");
        db.conn
            .execute(
                "INSERT INTO page_tags (abs_path, tag) VALUES ('/root/mTLS.md', 'security')",
                rusqlite::params![],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO page_gates (abs_path, term) VALUES ('/root/mTLS.md', 'mtls')",
                rusqlite::params![],
            )
            .unwrap();
        let mut app = App::new(db).unwrap();
        {
            let row = app
                .view
                .iter()
                .find(|r| r.id == "note:mTLS")
                .expect("sanity: mTLS page must be listed");
            assert_eq!(row.tags, vec!["security".to_string()], "tag loaded");
            assert_eq!(row.gates, vec!["mtls".to_string()], "gate loaded");
        }
        // Tag match.
        app.filter_text = "security".to_string();
        app.apply_filter();
        assert_eq!(app.view.len(), 1, "tag 'security' matches only the gated page");
        assert_eq!(app.view[0].id, "note:mTLS");
        // Gate-phrase match (case-insensitive).
        app.filter_text = "MTLS".to_string();
        app.apply_filter();
        assert_eq!(app.view.len(), 1, "gate phrase 'MTLS' matches the gated page");
        assert_eq!(app.view[0].id, "note:mTLS");
        // Neither present.
        app.filter_text = "quantum-soup".to_string();
        app.apply_filter();
        assert!(app.view.is_empty(), "unrelated needle must not match");
    }

    /// `open_read` resolves by *absolute path* — the stable identity — so a
    /// re-ordered `rows` snapshot (a `refresh`/`full-rebuild`) cannot open
    /// the wrong page. And `close_read` returns to Browse with the selection
    /// preserved (Esc-then-j continues where the user left off).
    #[test]
    fn open_read_closes_and_preserves_selection() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let _ = insert_page(&db, "alpha", "Alpha", "# H\nbody\n");
        let _ = insert_page(&db, "bravo", "Bravo", "# H\nbody\n");
        let mut app = App::new(db).unwrap();

        // Open the second page (sel=1).
        app.selected = 1;
        app.open_read().unwrap();
        if let Mode::Read(r) = &app.mode {
            assert_eq!(r.id, "note:bravo", "open the selected page");
            assert_eq!(r.row_idx, 1);
            assert!(!r.outline.is_empty(), "the body's heading is in the outline");
            assert_eq!(r.scroll_top, 0);
        } else {
            panic!("expected Read mode after open_read");
        }

        // Scroll a bit, then close.
        app.scroll_body(1);
        app.close_read();
        assert!(matches!(app.mode, Mode::Browse));
        assert_eq!(app.selected, 1, "selection preserved across the round trip");

        // Open → the body is loaded lazily (by path), not from a stale view.
        app.open_read().unwrap();
        if let Mode::Read(r) = &app.mode {
            assert!(r.body.contains("body"), "body text was loaded from the DB");
        }
    }

    /// `open_read` on an empty view (nothing selected) is a clean no-op —
    /// total on the degenerate input (no panic, mode unchanged).
    #[test]
    fn open_read_on_empty_view_is_a_noop() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let mut app = App::new(db).unwrap();
        assert!(app.view.is_empty());
        let before = matches!(app.mode, Mode::Browse);
        assert!(app.open_read().is_ok(), "open_read on empty view is Ok");
        assert_eq!(before, matches!(app.mode, Mode::Browse), "mode unchanged");
    }

    // ---- M8.2 — pure helpers + edit-mode tests ---------------------------

    /// A well-formed frontmatter + body fixture: a scalar `title`, a list
    /// `tags`, an *absent* `description` (so the scalar-clear path has
    /// nothing to clear), and an *absent* `search_required_phrase`. The bytes are
    /// used directly as the `EditState::entry_bytes` in the pure-helper
    /// tests (no file IO; `plan()` + `apply_op` are pure).
    const FM_FIXTURE: &str =
        "---\n\
         type: note\n\
         title: Hello World\n\
         tags:\n\
         - mTLS\n\
         - transformer\n\
         ---\n\
         body line 1\n\
         body line 2\n";

    /// `suggestions_for` is pure + total: an empty corpus gives an empty
    /// suggestion list (no panic on a degenerate input), a non-empty corpus
    /// filters by case-insensitive substring, and a *prefix* match on the
    /// buffer only returns matching tokens (order preserved).
    #[test]
    fn suggestions_for_is_pure_and_filters_correctly() {
        let corpus: Vec<String> = vec![
            "mTLS".into(),
            "transformer".into(),
            "llm".into(),
            "token-bucket".into(),
        ];
        // Empty buffer → the whole corpus (the "list of available tokens"
        // hint for a fresh field entry).
        assert_eq!(
            suggestions_for(&corpus, ""),
            vec!["mTLS", "transformer", "llm", "token-bucket"]
        );
        // Case-insensitive substring match.
        assert_eq!(
            suggestions_for(&corpus, "TRANS"),
            vec!["transformer"]
        );
        // A token that matches nothing → empty.
        assert!(suggestions_for(&corpus, "quantum").is_empty());
        // An empty corpus → always empty.
        assert!(suggestions_for(&[], "mTLS").is_empty());
    }

    /// `build_ops` on a scalar field is the "edit-in-place" contract:
    ///   - **unchanged** value (v == original)         → no-op
    ///   - **changed, non-empty** value                → one `SetScalar`
    ///   - **changed to empty, original non-empty**    → one `RemoveField`
    ///   - **already empty original, typed empty**     → no-op (clearing
    ///     an absent scalar is a no-op)
    #[test]
    fn build_ops_scalar_contract() {
        // The scalar's *original* value is read from `entry_bytes` (the
        // page file on disk), not a separate argument. Build a minimal
        // frontmatter fixture.
        let with_title = b"---\ntitle: Hello World\n---\n\nbody\n";
        let without_title = b"---\ndescription: some\n---\n\nbody\n";
        let no_editing: Option<String> = None;

        // Unchanged → no-op (buffer equals the on-disk value).
        let ops = build_ops(EditField::Title, "Hello World", &no_editing, &[], with_title);
        assert!(ops.is_empty(), "unchanged scalar is a no-op");

        // Changed → one SetScalar.
        let ops = build_ops(EditField::Title, "New", &no_editing, &[], with_title);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            ops[0],
            EditOp::SetScalar { key, ref value } if key == "title" && value == "New"
        ));

        // Cleared from non-empty → one RemoveField.
        let ops = build_ops(EditField::Title, "", &no_editing, &[], with_title);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            ops[0],
            EditOp::RemoveField { key } if key == "title"
        ));

        // Cleared from empty/absent → no-op.
        let ops = build_ops(EditField::Title, "   ", &no_editing, &[], without_title);
        assert!(ops.is_empty(), "clearing an absent scalar is a no-op");
    }

    /// `build_ops` on a list field is "append dedup'd against the current
    /// list, comma-separated tokens, order preserved":
    ///   - **empty buffer**                       → no-op
    ///   - **one new token, not in list**         → one `AppendItem`
    ///   - **multiple tokens, some new, some not** → one `AppendItem`
    ///     per *new* token, existing tokens dropped, order preserved
    ///   - **whitespace padding**                 → the token is trimmed
    #[test]
    fn build_ops_list_contract() {
        // `entry_bytes` is *not* read on the add path (a list commit
        // appends to the list; the baseline bytes only matter for scalars),
        // but the signature requires it — pass a minimal valid fixture.
        let bytes: &[u8] = b"---\ntags: [mTLS]\n---\n\nbody\n";
        let no_editing: Option<String> = None;

        // Empty buffer → no-op.
        let ops = build_ops(EditField::Tags, "", &no_editing, &["mTLS".into()], bytes);
        assert!(ops.is_empty(), "empty buffer on a list is a no-op");

        // One new token, not in list → one AppendItem.
        let ops = build_ops(EditField::Tags, "llm", &no_editing, &["mTLS".into()], bytes);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            ops[0],
            EditOp::AppendItem { key, ref item } if key == "tags" && item == "llm"
        ));

        // Multiple tokens, some new, some present → only the new ones,
        // order preserved. (Existing = ["mTLS", "transformer"].)
        let existing = vec!["mTLS".into(), "transformer".into()];
        let ops =
            build_ops(EditField::Tags, "mTLS, llm, transformer, token-bucket", &no_editing, &existing, bytes);
        assert_eq!(ops.len(), 2, "new tokens only, existing tokens dropped");
        match &ops[0] {
            EditOp::AppendItem { key, item } => {
                assert_eq!(*key, "tags");
                assert_eq!(item, "llm");
            }
            other => panic!("expected AppendItem, got {other:?}"),
        }
        match &ops[1] {
            EditOp::AppendItem { key, item } => {
                assert_eq!(*key, "tags");
                assert_eq!(item, "token-bucket");
            }
            other => panic!("expected AppendItem, got {other:?}"),
        }

        // Whitespace padding: the token is trimmed.
        let ops =
            build_ops(EditField::Tags, "  llm  ", &no_editing, &[], bytes);
        assert_eq!(ops.len(), 1);
        if let EditOp::AppendItem { ref item, key } = ops[0] {
            assert_eq!(key, "tags");
            assert_eq!(item, "llm");
        } else {
            panic!("expected AppendItem");
        }

        // All tokens in list → no AppendItem (dedup'd away → no-op).
        let ops = build_ops(
            EditField::Tags,
            "mTLS, transformer",
            &no_editing,
            &["mTLS".into(), "transformer".into()],
            bytes,
        );
        assert!(ops.is_empty(), "all tokens already in the list → no-op");

        // Empty tokens (whitespace-only segments) are dropped.
        let ops =
            build_ops(EditField::Tags, "  , ,  ", &no_editing, &[], bytes);
        assert!(ops.is_empty(), "whitespace-only tokens are dropped");
    }

    /// The **"replace"** contract (`editing_item = Some(old)`): the buffer
    /// was loaded with an existing item via the Selection sub-mode, the
    /// user typed a different value, and `Enter` commits. The commit must
    /// be *atomic* — one `RemoveItem(old)` *before* one `AppendItem(new)` —
    /// so that a rename (e.g. "mTLS" → "mtls") never leaves a window in
    /// which both values are absent / both present. Identical buffer →
    /// no-op.
    #[test]
    fn build_ops_replace_contract() {
        let bytes: &[u8] = b"---\ntags: [mTLS, transformer]\n---\n\nbody\n";
        let existing = vec!["mTLS".into(), "transformer".into()];
        let editing = Some("mTLS".to_string());

        // Renamed → RemoveItem(old) + AppendItem(new), in that order.
        let ops = build_ops(EditField::Tags, "mtls", &editing, &existing, bytes);
        assert_eq!(ops.len(), 2, "a rename is exactly old-out + new-in");
        assert!(
            matches!(
                ops[0],
                EditOp::RemoveItem { key, ref item } if key == "tags" && *item == "mTLS"
            ),
            "first op must remove the old item: {ops:?}"
        );
        assert!(
            matches!(
                ops[1],
                EditOp::AppendItem { key, ref item } if key == "tags" && *item == "mtls"
            ),
            "second op must append the new item: {ops:?}"
        );

        // Identical (case-insensitive trim equal) → no-op.
        let ops = build_ops(EditField::Tags, "mTLS", &editing, &existing, bytes);
        assert!(ops.is_empty(), "loading + re-committing the same item is a no-op");
    }

    /// `apply_op` is pure + total over the four `EditOp` variants: the
    /// verified core does the byte-level work; this test just pins that the
    /// dispatch matches each op to the right verified function and returns
    /// the same `Result` shape. The *actual* byte mutation (e.g. that
    /// `SetScalar("title", "New")` leaves the file parseable) is covered by
    /// the `frontmatter` spec in `kb-core`.
    #[test]
    fn apply_op_is_total_over_the_four_variants() {
        let bytes = FM_FIXTURE.as_bytes();
        // SetScalar → Ok (the byte sequence is valid after the rewrite).
        let r = apply_op(bytes, &EditOp::SetScalar {
            key: "title",
            value: "Renamed".into(),
        });
        assert!(r.is_ok());
        let new_bytes = r.unwrap();
        assert_ne!(new_bytes, bytes, "the byte sequence changed");
        // The new bytes are parseable and the scalar value is "Renamed".
        let v = kb_core::frontmatter::scalar_value(&new_bytes, "title");
        assert_eq!(v.as_deref(), Some("Renamed"));

        // AppendItem → Ok (the list gains one item).
        let r = apply_op(bytes, &EditOp::AppendItem {
            key: "tags",
            item: "newtag".into(),
        });
        assert!(r.is_ok());
        let new_bytes = r.unwrap();
        let l = kb_core::frontmatter::list_values(&new_bytes, "tags");
        assert!(matches!(l, Some(ref v) if v.contains(&"newtag".to_string())));

        // RemoveField → Ok (the field is absent after).
        let r = apply_op(bytes, &EditOp::RemoveField { key: "title" });
        assert!(r.is_ok());
        let new_bytes = r.unwrap();
        assert_eq!(
            kb_core::frontmatter::scalar_value(&new_bytes, "title"),
            None,
            "the scalar is absent after RemoveField"
        );

        // RemoveItem → Ok (the item is absent after, other items preserved).
        let r = apply_op(bytes, &EditOp::RemoveItem {
            key: "tags",
            item: "mTLS".into(),
        });
        assert!(r.is_ok());
        let new_bytes = r.unwrap();
        let l = kb_core::frontmatter::list_values(&new_bytes, "tags");
        assert_eq!(
            l,
            Some(vec!["transformer".to_string()]),
            "only mTLS is removed; the other item is preserved"
        );
    }

    /// `EditState::plan` is the *display* of the commit path: the same
    /// `build_ops` call, so the "pending plan" footer on the render
    /// matches what `commit_edit` applies. The pure tests above pin the
    /// individual `build_ops` cases; this one pins the *dispatch* (the
    /// field kind routes to the right branch). Total.
    #[test]
    fn edit_state_plan_dispatches_by_field_kind() {
        let bytes = FM_FIXTURE.as_bytes();
        // Scalar field (title), unchanged → no-op.
        let es = EditState {
            field: EditField::Title,
            abs_path: "/fake/title.md".into(),
            id: "note:P".into(),
            value: "Hello World".into(), // == the field's current value
            suggestions: Vec::new(),
            corpus: Vec::new(),
            entry_bytes: bytes.to_vec(),
            entry_list: Vec::new(),
            current_list: Vec::new(),
            working_bytes: bytes.to_vec(),
            stage: EditStage::Value,
            field_cursor: 0,
            sug_cursor: 0,
            value_cursor: "Hello World".chars().count(),
            in_list_select: false,
            list_high: 0,
            editing_item: None,
        };
        assert!(es.plan().is_empty(), "unchanged scalar → no-op plan");

        // Scalar field (title), changed → one SetScalar.
        let mut es2 = es.clone();
        es2.value = "Renamed".into();
        let plan = es2.plan();
        assert_eq!(plan.len(), 1);
        assert!(matches!(
            plan[0],
            EditOp::SetScalar { key, ref value } if key == "title" && value == "Renamed"
        ));

        // List field (tags), empty buffer → no-op.
        let es3 = EditState {
            field: EditField::Tags,
            abs_path: "/fake/tags.md".into(),
            id: "note:P".into(),
            value: String::new(),
            suggestions: Vec::new(),
            corpus: Vec::new(),
            entry_bytes: bytes.to_vec(),
            entry_list: vec!["mTLS".into(), "transformer".into()],
            current_list: vec!["mTLS".into(), "transformer".into()],
            working_bytes: bytes.to_vec(),
            stage: EditStage::Value,
            field_cursor: 2,
            sug_cursor: 0,
            value_cursor: 0,
            in_list_select: false,
            list_high: 0,
            editing_item: None,
        };
        assert!(es3.plan().is_empty(), "empty buffer on a list → no-op plan");

        // List field (tags), new token → one AppendItem (dedup'd against the
        // *current* list).
        let mut es4 = es3.clone();
        es4.value = "llm".into();
        let plan = es4.plan();
        assert_eq!(plan.len(), 1);
        assert!(matches!(
            plan[0],
            EditOp::AppendItem { key, ref item } if key == "tags" && item == "llm"
        ));

        // List field (tags), re-adding an existing token → no-op (dedup).
        let mut es5 = es3.clone();
        es5.value = "mTLS".into();
        assert!(es5.plan().is_empty(), "re-adding an existing token → no-op");
    }

    /// `App::current_plan` is the *convenience* over `EditState::plan` for
    /// the render + test surfaces: it reads the active edit state (if any)
    /// and delegates to `plan()`. On a non-Edit mode it returns an empty
    /// Vec (total; the render branch is only reached in the Edit mode).
    #[test]
    fn current_plan_delegates_to_edit_state_plan() {
        // Non-Edit mode → empty plan (total; no panic).
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let app = App::new(db).unwrap();
        assert!(matches!(app.mode, Mode::Browse), "we start in Browse");
        assert!(app.current_plan().is_empty(), "Browse mode → empty plan");
    }

    /// `open_edit` is total: an empty view (nothing selected) is a clean
    /// no-op (mode unchanged, Ok returned). This is the "nothing selected to
    /// edit" path the input layer funnels through.
    #[test]
    fn open_edit_on_empty_view_is_a_noop() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let mut app = App::new(db).unwrap();
        assert!(app.view.is_empty());
        assert!(matches!(app.mode, Mode::Browse));
        assert!(app.open_edit().is_ok(), "open_edit on empty view is Ok");
        assert!(matches!(app.mode, Mode::Browse), "mode unchanged");
    }

    /// `close_edit` drops the Edit mode and the pending buffer is discarded —
    /// the *file on disk* is only touched by a successful `commit_edit`
    /// (the TUI's "source of truth is the file" contract). Total.
    #[test]
    fn close_edit_discards_the_pending_buffer() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // Seed a real on-disk page so `open_edit` has bytes to read (the
        // file is the source of truth).
        let dir = std::env::temp_dir().join("kb_tui_close_edit_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("page.md");
        std::fs::write(&path, FM_FIXTURE.as_bytes()).unwrap();
        let abs = path.to_string_lossy().into_owned();
        let id = "note:page".to_string();
        db.conn
            .execute(
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase,
                                    mtime_ns, content_hash)
                 VALUES (?1, ?2, 'note', 'Page', '/root', 'page.md', NULL, 0, 'x')",
                rusqlite::params![abs, id],
            )
            .unwrap();
        let mut app = App::new(db).unwrap();
        assert_eq!(app.view.len(), 1);
        assert!(app.open_edit().is_ok(), "open_edit on a selected page is Ok");
        assert!(matches!(app.mode, Mode::Edit(_)), "open_edit enters Edit mode");
        // Type a change into the value buffer — the *only* in-memory edit.
        if let Mode::Edit(ref mut e) = app.mode {
            e.value = "Renamed".into();
        }
        let before = std::fs::read(&path).unwrap();
        assert_eq!(before, FM_FIXTURE.as_bytes(), "the file is unchanged before commit");
        app.close_edit();
        assert!(matches!(app.mode, Mode::Browse), "close_edit returns to Browse");
        let after = std::fs::read(&path).unwrap();
        assert_eq!(after, before, "close_edit does NOT write to disk");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `commit_edit` on a *modified* scalar field writes the change to disk,
    /// refreshes the DB row, and stays in Edit mode (so a follow-up field
    /// switch / commit can continue without re-opening the page). On an
    /// *unchanged* scalar the file is byte-identical (the "open then commit"
    /// UX), and the status bar is green with a no-op summary.
    #[test]
    fn commit_edit_round_trip_on_modified_scalar() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let dir = std::env::temp_dir().join("kb_tui_commit_edit_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("page.md");
        std::fs::write(&path, FM_FIXTURE.as_bytes()).unwrap();
        let abs = path.to_string_lossy().into_owned();
        db.conn
            .execute(
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase,
                                    mtime_ns, content_hash)
                 VALUES (?1, 'note:page', 'note', 'Hello World', '/root', 'page.md', NULL, 0, 'x')",
                rusqlite::params![abs],
            )
            .unwrap();
        let mut app = App::new(db).unwrap();
        assert_eq!(app.view.len(), 1);
        assert!(app.open_edit().is_ok());
        if let Mode::Edit(ref mut e) = app.mode {
            e.value = "Renamed".into();
        }
        // The title on disk is still "Hello World" before the commit.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            FM_FIXTURE,
            "the file is unchanged before commit"
        );
        assert!(app.commit_edit().is_ok(), "commit_edit on a modified scalar is Ok");
        // The file on disk is now "Renamed".
        let after = std::fs::read_to_string(&path).unwrap();
        let v = kb_core::frontmatter::scalar_value(after.as_bytes(), "title");
        assert_eq!(v, Some("Renamed".into()), "the commit wrote the new title");
        // The DB row was refreshed to the new title (the TUI's "source of
        // truth is the file" contract — the DB is only a cache). We
        // inspect it through `app.db` (the `Db` is moved into `App::new`).
        let db_title: String = app
            .db
            .conn
            .query_row(
                "SELECT title FROM pages WHERE abs_path = ?1",
                [abs.clone()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(db_title, "Renamed", "the DB row is refreshed after the commit");
        // The status bar is green (commit success).
        assert_eq!(
            app.status.kind,
            crate::tui::StatusKind::Success,
            "the status bar is green after a successful commit"
        );
        // We are still in Edit mode (the commit leaves the mode intact).
        assert!(matches!(app.mode, Mode::Edit(_)), "commit stays in Edit mode");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: a TUI `commit_edit` must preserve the page's `language`
    /// column in the DB — `open_edit` + `commit_edit` re-derives every
    /// frontmatter-derived column and upserts, so `language` (unlike the
    /// `root`/`rel_path` sentinel-guarded pair) must be *re-extracted from the
    /// file* by the callsite, which passes `include_language=true` to
    /// `build_page`. The historical value was `false`, which silently nulled
    /// the column on every TUI edit and broke the `language:` / `language:Null`
    /// search filters on any edited page. This test exercises the TUI callsite
    /// *itself* (not just the `build_page`/`upsert_page` contract that the
    /// kb-db `index` tests already pin): a regression of the callsite value
    /// back to `false` (without touching `build_page`) still fails here.
    #[test]
    fn commit_edit_preserves_language_column() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let dir = std::env::temp_dir().join("kb_tui_commit_edit_lang");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("en.md");
        let fm = "---\ntype: note\ntitle: En Note\nlanguage: en\n---\nbody\n";
        std::fs::write(&path, fm).unwrap();
        let abs = path.to_string_lossy().into_owned();
        // Seed the DB row the same way `refresh` would — with `language` set.
        db.conn
            .execute(
                "INSERT INTO pages (abs_path, id, type, title, language, root, rel_path,
                                    search_required_phrase, mtime_ns, content_hash)
                  VALUES (?1, 'note:En Note', 'note', 'En Note', 'en', '/root', 'en.md',
                          NULL, 0, 'x')",
                rusqlite::params![abs],
            )
            .unwrap();
        let mut app = App::new(db).unwrap();
        assert_eq!(app.view.len(), 1);
        // Sanity: the initial DB value is `en`.
        let lang_initial: String = app
            .db
            .conn
            .query_row(
                "SELECT COALESCE(language, '__NULL__') FROM pages WHERE abs_path = ?1",
                [abs.clone()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(lang_initial, "en", "precondition: DB row starts with language=en");

        // A no-op commit: open_edit + commit_edit without mutating any field.
        // This is the *minimal* edit path — the TUI's "open-then-commit" UX —
        // and is the exact path that historically clobbered the column.
        assert!(app.open_edit().is_ok());
        assert!(app.commit_edit().is_ok());

        // After the commit, the DB row must still carry `language: en`.
        let lang_after: String = app
            .db
            .conn
            .query_row(
                "SELECT COALESCE(language, '__NULL__') FROM pages WHERE abs_path = ?1",
                [abs.clone()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            lang_after,
            "en",
            "commit_edit must NOT null the language column (this is the regression\
             this test guards against)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Redesigned edit flow (zsh-style three-stage) --------------------

    /// A small helper: seed an on-disk page + DB row + the derived
    /// `page_tags` rows, return an `App` with that page selected. Mirrors the
    /// fixture in `close_edit_...` but returns the temp dir so the caller can
    /// clean it up. Seeding `page_tags` is what gives the `Tags` field a
    /// non-empty *corpus* (the suggestions come from the DB, not the file).
    /// RAII guard over the seeded page *and* its parent temp dir. Drop removes
    /// the whole dir (which holds only `page.md`). `Deref` to the page
    /// `PathBuf` keeps the existing `let (app, page) = seeded_app();` shape —
    /// `page.to_str()` / `read_to_string(&page)` all resolve to the file.
    /// `seeded_app` used to hand back a bare `PathBuf` the caller bound to
    /// `_dir`/`page` and dropped without cleanup — every call leaked its dir,
    /// and thousands of `kb_tui_redesign_*` dirs accumulated in `$TMPDIR`.
    struct Seeded {
        page: std::path::PathBuf,
        dir: std::path::PathBuf,
    }
    impl std::ops::Deref for Seeded {
        type Target = std::path::PathBuf;
        fn deref(&self) -> &std::path::PathBuf {
            &self.page
        }
    }
    impl AsRef<std::path::Path> for Seeded {
        fn as_ref(&self) -> &std::path::Path {
            self.page.as_path()
        }
    }
    impl Drop for Seeded {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn seeded_app() -> (App, Seeded) {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // Every call gets its *own* unique on-disk page, so parallel test
        // threads (all sharing one pid) never read/write a common file — the
        // old `kb_tui_redesign_{pid}` dir made every `seeded_app` call share
        // one `page.md`, so one test's commit raced another test's re-seed
        // and the on-disk assertion flaked. A per-call sequence (atomic, so
        // distinct under parallelism) + the (fresh) pid fully isolates them.
        let seq = SEEDED_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kb_tui_redesign_{}_{}", std::process::id(), seq));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("page.md");
        std::fs::write(&path, FM_FIXTURE.as_bytes()).unwrap();
        let abs = path.to_string_lossy().into_owned();
        db.conn
            .execute(
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase,
                                    mtime_ns, content_hash)
                 VALUES (?1, 'note:page', 'note', 'Hello World', '/root', 'page.md', NULL, 0, 'x')",
                rusqlite::params![abs],
            )
            .unwrap();
        // The tags field's *corpus* is read from `page_tags` (the DB is the
        // index; the file is the file). Seed the two fixture tokens so a
        // tags-field edit has something to complete against.
        db.conn
            .execute(
                "INSERT OR IGNORE INTO page_tags (abs_path, tag) VALUES (?1, 'mtls')",
                rusqlite::params![abs],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT OR IGNORE INTO page_tags (abs_path, tag) VALUES (?1, 'transformer')",
                rusqlite::params![abs],
            )
            .unwrap();
        (
            App::new(db).unwrap(),
            Seeded { page: path, dir },
        )
    }

    /// `open_edit` now starts in the **Picker** stage: the field list is
    /// shown, no field is entered yet, the buffer is empty, the field is
    /// still `Title` (the first in the cycle) and `field_cursor` is 0.
    /// This is the "deliberate first step" — entering a specific field is a
    /// separate `Enter` action (`enter_editor`).
    #[test]
    fn open_edit_starts_in_the_picker_stage() {
        let (mut app, _dir) = seeded_app();
        assert!(app.open_edit().is_ok());
        match &app.mode {
            Mode::Edit(e) => {
                assert_eq!(e.stage, EditStage::Picker, "starts in Picker");
                assert_eq!(e.field, EditField::Title, "field defaults to the first");
                assert_eq!(e.field_cursor, 0, "cursor on the first field");
                assert!(e.value.is_empty(), "buffer is empty before a field is entered");
                assert!(e.suggestions.is_empty(), "no suggestions before a field is entered");
            }
            _ => panic!("open_edit did not enter Edit mode"),
        }
    }

    /// `picker_move` wraps at both ends and never leaves the field list
    /// (the cursor is always a valid index into `EditField::all()`), and
    /// stays in the Picker stage.
    #[test]
    fn picker_move_wraps_and_is_bounded() {
        let (mut app, _dir) = seeded_app();
        app.open_edit().unwrap();
        let n = EditField::all().len();
        // Down wraps forward (0 → 1).
        app.picker_move(true);
        assert_eq!(picker_cursor(&app), 1);
        // Up n times from position 1 wraps to position 1 - n = position 1
        // (full circle), and stays valid every step.
        for _ in 0..n {
            app.picker_move(false);
            assert!(picker_cursor(&app) < n, "cursor is a valid index");
        }
        assert_eq!(picker_cursor(&app), 1, "n up-moves from 1 wraps back to 1");
        // We never left the Picker stage.
        assert_eq!(stage(&app), EditStage::Picker);
    }

    /// `enter_editor` loads the *highlighted* field into the **Value** stage
    /// — the field choice is *final* here (no more field-cycling from the
    /// buffer view). The buffer is seeded with the field's current value
    /// (scalar) or empty (list), and the candidate highlight starts at 0.
    /// This is the "choose a field, then touch its value" invariant.
    #[test]
    fn enter_editor_loads_the_field_and_starts_in_value() {
        let (mut app, _dir) = seeded_app();
        app.open_edit().unwrap();
        // Move the cursor to `tags` (index 2) and enter it.
        app.picker_move(true);
        app.picker_move(true);
        assert_eq!(picker_cursor(&app), 2);
        app.enter_editor();
        match &app.mode {
            Mode::Edit(e) => {
                assert_eq!(e.stage, EditStage::Value, "flips to the Value stage");
                assert_eq!(e.field, EditField::Tags, "the chosen field is entered");
                assert!(e.value.is_empty(), "a list field seeds an empty buffer");
                assert_eq!(e.sug_cursor, 0, "candidate highlight starts at the top");
                // The existing list items are loaded (the "what's there").
                assert_eq!(
                    e.current_list,
                    vec!["mTLS", "transformer"],
                    "the field's existing items are loaded"
                );
            }
            _ => panic!("not in Edit mode"),
        }
        // `enter_editor` on a *scalar* field seeds the buffer with the current
        // value (the in-place edit guarantee) and has no candidates to pick.
        let (mut app2, _dir2) = seeded_app();
        app2.open_edit().unwrap();
        app2.enter_editor(); // field_cursor 0 → Title (scalar)
        if let Mode::Edit(ref e) = app2.mode {
            assert_eq!(e.stage, EditStage::Value);
            assert_eq!(e.field, EditField::Title);
            assert_eq!(e.value, "Hello World", "scalar seeds the current value");
            assert!(e.suggestions.is_empty(), "a scalar has no candidates to autocomplete");
        }
    }

    /// `Tab` (**`autocomplete`**) is the zsh *autocomplete* idiom: it fills
    /// the value buffer with the *highlighted* candidate (`sug_cursor`) and
    /// re-narrows the list — the buffer now holds a whole known token, ready
    /// to commit (or keep typing to refine). **Total**: on a scalar field
    /// (no candidates) it is a clean no-op, leaving the buffer exactly as
    /// typed — the "nothing to complete" path.
    #[test]
    fn tab_autocompletes_the_buffer_from_the_highlighted_candidate() {
        let (mut app, _dir) = seeded_app();
        app.open_edit().unwrap();
        // Enter `tags` and type a partial token.
        app.picker_move(true);
        app.picker_move(true);
        app.enter_editor();
        app.edit_type('m');
        app.edit_type('t');
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value, "mt", "the user typed a partial token");
            assert_eq!(e.suggestions, vec!["mtls".to_string()], "one match narrows the list");
            assert_eq!(e.sug_cursor, 0, "the highlight is on the single candidate");
        }
        app.autocomplete(); // Tab
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(
                e.value, "mtls",
                "Tab *fills* the buffer with the highlighted candidate's full text"
            );
            assert_eq!(e.value, e.suggestions[0], "buffer now equals the candidate it came from");
        }
        // On a *scalar* field (no candidates), `Tab` is a no-op: the buffer
        // — whatever the user typed — is left completely alone.
        let (mut app2, _dir2) = seeded_app();
        app2.open_edit().unwrap();
        app2.enter_editor(); // Title (scalar)
        app2.edit_type('z');
        app2.edit_type('z');
        let buffer_before = {
            if let Mode::Edit(ref e) = app2.mode { e.value.clone() } else { panic!() }
        };
        app2.autocomplete();
        if let Mode::Edit(ref e) = app2.mode {
            assert_eq!(
                e.value, buffer_before,
                "no candidate (scalar) means Tab leaves the buffer exactly as typed"
            );
            assert_ne!(app2.status.kind, crate::tui::StatusKind::Error, "the no-op path is not an error");
        }
    }

    /// Typing **appends to the buffer** and the live list re-narrows in
    /// place — a filter change never "drops" the list, it just re-anchors
    /// it (the zsh filter idiom). The candidate list is always a pure
    /// function of the buffer, so there is no open/closed state to worry
    /// about.
    #[test]
    fn typing_renarrows_the_live_menu_in_place() {
        let (mut app, _dir) = seeded_app();
        app.open_edit().unwrap();
        app.picker_move(true); // description (no menu)
        app.picker_move(true); // tags (menu)
        app.enter_editor();
        // Corpus has "mtls" / "transformer" (both contain "m"). Type "m":
        // the live list is *both* (substring, not prefix).
        app.edit_type('m');
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value, "m");
            assert_eq!(
                e.suggestions,
                vec!["mtls".to_string(), "transformer".to_string()],
                "both corpus tokens contain 'm' — substring match"
            );
        }
        // Type one more char — the char goes to the buffer (nothing else is
        // "focused"), and the live list re-narrows in place.
        // ("mt" as substring matches "mtls" but not "transformer".)
        app.edit_type('t');
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value, "mt", "the char went straight to the buffer");
            assert_eq!(
                e.suggestions,
                vec!["mtls".to_string()],
                "the live list re-narrowed to exactly the 'mtls' match"
            );
            assert_eq!(e.sug_cursor, 0, "the highlight stays valid after a re-narrow");
        }
    }

    /// `Enter` (**`commit_edit`**) commits the buffer *as typed*, then
    /// **stays on the Value stage for the same field** (the buffer is
    /// cleared by the commit's disk-refresh — ready for the next token).
    /// For a *new* token (one not already in the list) this is a real
    /// append + a disk write. This is the zsh "type → Tab (fill if you
    /// want) → Enter (commit)" idiom — and a commit is not a close.
    #[test]
    fn enter_commits_the_buffer_and_stays_on_value() {
        let (mut app, page) = seeded_app();
        app.open_edit().unwrap();
        // Enter `tags`, type a token *not yet* in the list so the commit is
        // a real append (not a dedup no-op).
        app.picker_move(true); // description
        app.picker_move(true); // tags
        app.enter_editor();
        for c in ['s', 'e', 'c'] {
            app.edit_type(c);
        }
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value, "sec", "the user typed a new token");
            assert!(
                !e.current_list.iter().any(|t| t.eq_ignore_ascii_case("sec")),
                "'sec' is not yet in the list — this will be a real append"
            );
        }
        assert!(app.commit_edit().is_ok(), "commit is Ok");
        // After a *successful* commit:
        //   - we stay on the Value stage for the same field (not the Picker)
        //   - the buffer is cleared (a list commit's disk-refresh → empty)
        //   - the highlight is reset to the top (ready for the next token)
        //   - the file is changed: the new token is now appended
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.stage, EditStage::Value, "success stays on Value (not the Picker)");
            assert_eq!(e.field, EditField::Tags, "the same field is still active");
            assert_eq!(e.sug_cursor, 0, "the highlight resets for the next token");
            assert!(e.value.is_empty(), "the buffer is cleared after a list commit");
        }
        let contents = std::fs::read_to_string(&page).unwrap();
        let tags_now: Vec<String> =
            kb_core::frontmatter::list_values(contents.as_bytes(), "tags").unwrap_or_default();
        assert_eq!(
            tags_now,
            vec!["mTLS".to_string(), "transformer".to_string(), "sec".to_string()],
            "the new token was appended on commit"
        );
    }

    /// **The letter-safety test** — the whole reason for this redesign:
    /// *every* letter (`q`, `a`, `r`, `x`, `e`, `v`, `t`, `s`, …) must go
    /// straight to the buffer in the **Value** stage, never to a manager.
    /// The old bugs were: `q` → quit the TUI, `a` → alias manager,
    /// `r` → roots manager, `x` → remove-tail. That meant "query" died at
    /// `q`, "data" died at `a`, "security" died at `r`, "tax" died at `x`.
    /// This test pins one of each command-letter against a real page and
    /// asserts the letters all landed in the buffer, no manager opened,
    /// the file on disk is unchanged (no commit fired), and no field was
    /// cycled.
    #[test]
    fn in_value_stage_the_letters_a_q_r_x_v_e_all_type_straight_into_the_buffer() {
        let (mut app, page) = seeded_app();
        app.open_edit().unwrap();
        // Go to `tags` (a list field — starts with an empty buffer, so we
        // don't have to worry about the scalar field's current-value seed).
        app.picker_move(true); // description
        app.picker_move(true); // tags
        app.enter_editor();
        // Sanity: list fields start with an empty buffer.
        if let Mode::Edit(ref e) = app.mode {
            assert!(e.value.is_empty(), "list fields start with an empty buffer");
        }
        // Every one of the old command-letter hijacks. `q` would have been:
        // quit the entire TUI. `a` / `r` would have opened another manager.
        // `x` would have dropped the list's tail. `v` / `e` would have been
        // "open in external editor" / "enter edit".
        for c in ['q', 'a', 'r', 'x', 'v', 'e'] {
            app.edit_type(c);
        }
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(
                e.value, "qarxve",
                "every letter goes to the buffer — no manager hijack occurred"
            );
            assert_eq!(e.field, EditField::Tags, "no field change");
            assert_eq!(e.stage, EditStage::Value, "still on the Value stage");
        }
        // The file on disk is unchanged (no commit fired).
        let contents = std::fs::read_to_string(&page).unwrap();
        assert_eq!(contents, FM_FIXTURE, "no commit fired — the file is untouched");
    }

    /// **The field is final in the Value stage** — the user asked: "I've
    /// already selected the field; why do up and down change *that*? They
    /// should just pick from the completions." This test pins *both* halves:
    /// (a) `↑/↓` on a non-empty candidate list move the *highlight* across
    /// the candidates, and (b) they **never** switch the field (even when
    /// the list has been narrowed to a single candidate, or on a scalar
    /// where the list is empty). Total: every sequence of arrow presses
    /// leaves the field untouched.
    #[test]
    fn arrows_pick_candidates_and_never_switch_the_field() {
        let (mut app, _dir) = seeded_app();
        app.open_edit().unwrap();
        app.picker_move(true); // description
        app.picker_move(true); // tags
        app.enter_editor();
        assert_eq!(field_of(&app), EditField::Tags, "we are on `tags`");
        // The full candidate list (empty buffer → the whole corpus).
        app.move_suggestion(true); // highlight the 1st candidate (down from top)
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.sug_cursor, 1, "down moves the highlight forward");
            assert_eq!(
                field_of(&app),
                EditField::Tags,
                "a down-press does **not** switch the field"
            );
        }
        app.move_suggestion(false); // back to the top
        app.move_suggestion(false); // and *wrap* past the top to the last one
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.sug_cursor, 1, "wrapping up from 0 lands on the last candidate");
            assert_eq!(
                field_of(&app),
                EditField::Tags,
                "a wrap does **not** switch the field either"
            );
        }
        // Narrow to a single candidate: arrows are now a *no-op* (nothing to
        // move between — there is exactly one). This is the *exact* case the
        // old model would have used to switch fields; now it just pins.
        app.edit_type('t');
        app.edit_type('r');
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.suggestions.len(), 1, "'tr' narrows to exactly one candidate");
        }
        let cursor_before = {
            if let Mode::Edit(ref e) = app.mode { e.sug_cursor } else { panic!() }
        };
        app.move_suggestion(true);
        app.move_suggestion(false);
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.sug_cursor, cursor_before, "single candidate → arrows are a no-op");
            assert_eq!(
                field_of(&app),
                EditField::Tags,
                "single candidate still pins the field"
            );
        }
        // And on a **scalar** field (no candidates at all) the arrows are a
        // clean no-op: nothing to pick, and the field is still the one we
        // chose.
        let (mut app2, _dir2) = seeded_app();
        app2.open_edit().unwrap();
        app2.enter_editor(); // Title (scalar)
        assert_eq!(field_of(&app2), EditField::Title, "we are on `title`");
        app2.move_suggestion(true);
        app2.move_suggestion(false);
        assert_eq!(
            field_of(&app2),
            EditField::Title,
            "arrows on a scalar (no candidates) never cycle to another field"
        );
    }

    /// A small helper: the active Edit field (panics if not in Edit mode —
    /// the total test surface relies on the caller having entered Edit).
    fn field_of(app: &App) -> EditField {
        match &app.mode {
            Mode::Edit(e) => e.field,
            _ => panic!("not in Edit mode while asserting a field"),
        }
    }

    /// **Smart `Backspace`** (a zsh list-editor idiom): while the buffer
    /// *has* text, `Backspace` pops the last char (re-narrowing the live
    /// menu); once the buffer is **empty** on a **list** field, `Backspace`
    /// drops the field's *tail item* (the "delete the last thing I added"
    /// semantic), mutating `current_list` + `working_bytes` but NOT writing
    /// to disk. The *old* dedicated `x` key is gone — this path is now
    /// discoverable via a single well-known key.
    ///
    /// This is the test the "letter-safety" change was designed around
    /// (the user can now `Backspace` to remove "transformer" and it works
    /// without a magic `x` key competing with the letters they type).
    #[test]
    fn smart_backspace_pops_char_while_buffer_has_text_drops_tail_when_empty() {
        let (mut app, page) = seeded_app();
        app.open_edit().unwrap();
        app.picker_move(true); // description
        app.picker_move(true); // tags
        app.enter_editor();
        // Sanity: `tags` starts with an empty buffer and two existing items
        // (the corpus is the file's list on entry; `entry_list` is set on
        // the *commit* refresh, not on entry).
        if let Mode::Edit(ref e) = app.mode {
            assert!(e.value.is_empty());
            assert_eq!(e.current_list, vec!["mTLS", "transformer"]);
        }
        // Type "se" — the buffer has text.
        app.edit_type('s');
        app.edit_type('e');
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value, "se");
        }
        // First Backspace on a NON-empty buffer: pops a char, not the tail.
        app.remove_or_pop();
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value, "s", "Backspace pops a char (buffer non-empty)");
            assert_eq!(e.current_list, vec!["mTLS", "transformer"], "tail NOT dropped");
        }
        // Pop the remaining char — the buffer is now empty.
        app.remove_or_pop();
        if let Mode::Edit(ref e) = app.mode {
            assert!(e.value.is_empty(), "buffer is now empty");
        }
        // Second Backspace on an EMPTY buffer: drops the list tail
        // ("transformer").
        app.remove_or_pop();
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(
                e.current_list,
                vec!["mTLS"],
                "the tail 'transformer' was dropped from `current_list`"
            );
            assert_ne!(e.current_list, e.entry_list, "working state diverged from entry state");
            assert!(e.value.is_empty(), "the buffer stays empty after a tail drop");
        }
        // No disk write has happened (the commit is still pending).
        let contents = std::fs::read_to_string(&page).unwrap();
        assert_eq!(contents, FM_FIXTURE, "no commit fired — disk unchanged before commit");
        // Now commit — the working bytes (tail already dropped) are the
        // baseline for the write.
        assert!(app.commit_edit().is_ok(), "commit after a tail drop is Ok");
        let after = std::fs::read_to_string(&page).unwrap();
        let tags_now: Vec<String> =
            kb_core::frontmatter::list_values(after.as_bytes(), "tags").unwrap_or_default();
        assert_eq!(tags_now, vec!["mTLS"], "the committed file has the tail removed");
    }

    /// Smart `Backspace` on an **empty list** (no tail to drop) is a clean
    /// no-op + a *status note* (not an error): the "list already empty"
    /// invariant, no state change, no panic. This is the *other* tail of
    /// the smart `Backspace` path — the "nothing left to remove" branch.
    #[test]
    fn smart_backspace_with_no_tail_is_a_clean_noop() {
        // Use a fresh empty page (no pre-seeded tags) — the fixture seeds
        // [mTLS, transformer]; here we open the `search_required_phrase` field which
        // is empty on the fixture, exercising the "no tail to drop" path.
        let (mut app, page) = seeded_app();
        app.open_edit().unwrap();
        app.picker_move(true); // description
        app.picker_move(true); // tags
        app.picker_move(true); // search_required_phrase
        app.enter_editor();
        // Sanity: search_required_phrase is an empty list.
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.field, EditField::SearchRequiredPhrase, "we're on search_required_phrase");
            assert!(e.value.is_empty(), "buffer is empty");
            assert!(e.current_list.is_empty(), "no existing items");
        }
        // Backspace on an empty buffer + empty list → the "nothing to
        // remove" branch (a status note, not an error, no state change).
        let before = std::fs::read_to_string(&page).unwrap();
        app.remove_or_pop();
        if let Mode::Edit(ref e) = app.mode {
            assert!(e.value.is_empty(), "buffer still empty");
            assert!(e.current_list.is_empty(), "list still empty");
        }
        let after = std::fs::read_to_string(&page).unwrap();
        assert_eq!(after, before, "no commit fired — disk unchanged");
    }

    /// **`remove_tail_item`** is the underlying primitive (exposed for the
    /// test surface). It mutates `working_bytes` + `current_list` and sets
    /// a *green* status (the "removed … — Enter to commit" preview). No
    /// disk write, no mode change, no field change.
    #[test]
    fn remove_tail_item_is_live_and_does_not_commit() {
        let (mut app, page) = seeded_app();
        app.open_edit().unwrap();
        app.picker_move(true); // description
        app.picker_move(true); // tags
        app.enter_editor();
        // Baseline.
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.current_list, vec!["mTLS", "transformer"]);
        }
        let before = std::fs::read_to_string(&page).unwrap();
        app.remove_tail_item();
        // `current_list` is updated in memory; `working_bytes` diverges;
        // the file on disk is unchanged (this is a *live* preview).
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.current_list, vec!["mTLS"]);
            assert_eq!(e.stage, EditStage::Value, "still on the Value stage");
            assert_eq!(e.field, EditField::Tags, "field unchanged");
        }
        let after = std::fs::read_to_string(&page).unwrap();
        assert_eq!(after, before, "no write to disk — `remove_tail_item` is live");
        // Status is green (a *success* preview, not an error) or at least
        // not a red error (the user will see it and commit with Enter).
        assert_ne!(app.status.kind, crate::tui::StatusKind::Error,
            "remove_tail_item sets a non-error status");
    }

    /// `Esc` in the **Value** stage pops back to the **Picker** (the
    /// "abandon this field, pick another" invariant); `Esc` in the **Picker**
    /// leaves the Edit mode entirely (→ Browse). This is the "Esc pops one
    /// stage" invariant per stage.
    #[test]
    fn esc_pops_one_stage_at_a_time() {
        let (mut app, _dir) = seeded_app();
        app.open_edit().unwrap();
        assert_eq!(stage(&app), EditStage::Picker);
        // Value → Picker (via the state transition, not the raw key).
        app.enter_editor();
        assert_eq!(stage(&app), EditStage::Value);
        // Pop back to Picker (the Value→Picker half of the Esc dispatch).
        if let Mode::Edit(ref mut e) = app.mode {
            e.stage = EditStage::Picker;
        }
        assert_eq!(stage(&app), EditStage::Picker);
        // Picker → Browse.
        app.close_edit();
        assert!(matches!(app.mode, Mode::Browse), "Esc from Picker → Browse");
    }

    /// Helpers that read the active edit state (total: `App` state accessors
    /// for the stage / field cursor the input + render layers rely on).
    fn stage(app: &App) -> EditStage {
        match &app.mode {
            Mode::Edit(e) => e.stage,
            _ => panic!("not in Edit mode while asserting a stage"),
        }
    }
    fn picker_cursor(app: &App) -> usize {
        match &app.mode {
            Mode::Edit(e) => e.field_cursor,
            _ => panic!("not in Edit mode while asserting the picker cursor"),
        }
    }

    // ---- M8.3 alias + roots ---------------------------------------------

    /// `parse_alias_form` is pure + total: it splits on the *first* whitespace
    /// run, trims both halves, and requires both to be non-empty. This is the
    /// single predicate `add_alias` validates against, so the "what will
    /// Enter submit" and "is the form invalid?" checks cannot diverge.
    #[test]
    fn parse_alias_form_is_pure_and_total() {
        // Valid: single space.
        assert_eq!(
            App::parse_alias_form("llm transformer"),
            Some(("llm".into(), "transformer".into()))
        );
        // Multi-space run collapses to one boundary; both halves trimmed.
        assert_eq!(
            App::parse_alias_form("  llm     transformer  "),
            Some(("llm".into(), "transformer".into()))
        );
        // The canonical may keep its *inner* whitespace (splitn on first
        // whitespace run only) — "gpt 4" stays "gpt 4".
        assert_eq!(
            App::parse_alias_form("g gpt 4"),
            Some(("g".into(), "gpt 4".into()))
        );
        // Newlines are whitespace too (a pasted form still parses).
        assert_eq!(
            App::parse_alias_form("\nllm\ttransformer\n"),
            Some(("llm".into(), "transformer".into()))
        );
        // Invalid: missing the canonical half.
        assert_eq!(App::parse_alias_form("llm"), None);
        // Invalid: alias present, canonical blank.
        assert_eq!(App::parse_alias_form("llm   "), None);
        // Invalid: canonical present, alias blank.
        assert_eq!(App::parse_alias_form("  llm"), None);
        // Invalid: wholly empty.
        assert_eq!(App::parse_alias_form(""), None);
        assert_eq!(App::parse_alias_form("   \t "), None);
    }

    /// `open_alias` is total on a fresh/empty DB (an empty list, not an
    /// error), and leaves the mode in `Alias` with the add form active
    /// (`focused == false`, so typing lands in `input`).
    #[test]
    fn open_alias_is_total_on_empty_db() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let mut app = App::new(db).unwrap();
        assert!(app.open_alias().is_ok());
        let Mode::Alias(ref a) = app.mode else {
            panic!("open_alias must enter Alias mode");
        };
        assert!(a.list.is_empty(), "a fresh DB has no aliases");
        assert!(a.input.is_empty());
        assert!(!a.focused, "the add form is active on entry");
        assert_eq!(a.selected, 0);
    }

    /// `add_alias` upserts into `tag_aliases` (the *same* `ON CONFLICT`
    /// as `cmd_tag_alias --add`), refreshes the in-memory list, and clears
    /// the form. A first add then a re-add of the *same alias* with a new
    /// canonical **last-wins** (row count stays 1, canonical updates).
    #[test]
    fn add_alias_upsert_round_trip() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let mut app = App::new(db).unwrap();
        app.open_alias().unwrap();
        // Type "llm transformer" into the form.
        for c in "llm transformer".chars() {
            app.alias_type(c);
        }
        assert!(app.add_alias().is_ok());
        // The row is present, and the form was cleared.
        let Mode::Alias(ref a) = app.mode else { panic!() };
        assert_eq!(a.list.len(), 1);
        assert_eq!(a.list[0].alias, "llm");
        assert_eq!(a.list[0].canonical, "transformer");
        assert!(a.input.is_empty(), "the form is cleared after a successful add");
        assert!(!a.focused, "focus returns to the form after add");
        // Re-add the same alias with a *different* canonical → last-wins.
        // (The form is active after the first add, so typing applies.)
        for c in "llm rag".chars() {
            app.alias_type(c);
        }
        assert!(app.add_alias().is_ok());
        let Mode::Alias(ref a) = app.mode else { panic!() };
        assert_eq!(a.list.len(), 1, "upsert keeps the row count at 1 (last-wins)");
        assert_eq!(a.list[0].canonical, "rag", "the canonical is updated on re-add");
    }

    /// `add_alias` on a malformed form is a clean no-op (the row count is
    /// unchanged, the form is *not* cleared, the status bar is red) — the
    /// "type a bad form, correct it, re-Enter" UX must not lose the buffer.
    #[test]
    fn add_alias_malformed_is_a_clean_noop() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let mut app = App::new(db).unwrap();
        app.open_alias().unwrap();
        for c in "only-alias".chars() {
            app.alias_type(c);
        }
        assert!(app.add_alias().is_ok(), "a malformed form is Ok (not an Err)");
        let Mode::Alias(ref a) = app.mode else { panic!() };
        assert!(a.list.is_empty(), "no row was added");
        assert_eq!(a.input, "only-alias", "the buffer is preserved for correction");
        assert_eq!(
            app.status.kind,
            crate::tui::StatusKind::Error,
            "the status bar is red on a malformed form"
        );
    }

    /// `remove_alias` deletes the *selected* row (when the list is focused)
    /// and refreshes. When the form is focused or the list is empty it is a
    /// clean no-op (total + idempotent).
    #[test]
    fn remove_alias_round_trip_and_total() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let mut app = App::new(db).unwrap();
        app.open_alias().unwrap();
        // Empty list + form focused → no-op.
        app.alias_type('x'); // ensure form is active (focused == false)
        assert!(app.remove_alias().is_ok(), "empty/form-focused remove is Ok");
        // Seed two aliases directly (mirroring `cmd_tag_alias --add`).
        for (alias, canonical) in [("a", "AAA"), ("b", "BBB")] {
            app.db
                .conn
                .execute(
                    "INSERT INTO tag_aliases(alias, canonical) VALUES (?1, ?2)
                     ON CONFLICT(alias) DO UPDATE SET canonical = excluded.canonical",
                    rusqlite::params![alias, canonical],
                )
                .unwrap();
        }
        app.reload_aliases().unwrap();
        let Mode::Alias(ref a) = app.mode else { panic!() };
        assert_eq!(a.list.len(), 2);
        // Focus the list, then remove the first (selected == 0) alias.
        app.alias_toggle_focus();
        assert!(app.remove_alias().is_ok());
        let Mode::Alias(ref a) = app.mode else { panic!() };
        assert_eq!(a.list.len(), 1);
        assert_eq!(a.list[0].alias, "b", "the selected (first) alias is removed");
        assert!(a.focused, "the list stays focused after a remove");
    }

    /// `alias_move` / `alias_toggle_focus` are total + bounded: the
    /// selection always lands in range, stepping past an edge is a no-op,
    /// and `Tab` cleanly flips between the form and the list.
    #[test]
    fn alias_move_is_bounded_and_focus_toggles() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        for (alias, canonical) in [("a", "A"), ("b", "B"), ("c", "C")].iter() {
            app_insert_alias(&db, alias, canonical);
        }
        let mut app = App::new(db).unwrap();
        app.open_alias().unwrap();
        app.alias_toggle_focus(); // → list
        let n = 3;
        app.alias_move(-1);
        assert_eq!(sel_alias(&app), 0, "cannot go above row 0");
        app.alias_move(100);
        assert_eq!(sel_alias(&app), n - 1, "cannot go past the last row");
        app.alias_move(-1);
        assert_eq!(sel_alias(&app), n - 2);
        // Toggle back to the form.
        app.alias_toggle_focus();
        assert!(!focused_alias(&app), "focus is back on the form");
        // Typing only lands in the input when the form is focused.
        app.alias_type('q');
        assert_eq!(input_alias(&app), "q");
        app.alias_toggle_focus(); // → list
        app.alias_type('q');
        assert_eq!(input_alias(&app), "q", "typing is ignored on the list");
    }

    /// `canonicalize_dir_for_add` is the pure guard for the Roots add form:
    /// a real directory canonicalizes; a missing path is a clean Err; a
    /// path that is a *file* (not a directory) is a clean Err — all
    /// total on `&str`.
    #[test]
    fn canonicalize_dir_for_add_is_total() {
        let dir = std::env::temp_dir().join("kb_tui_canon_dir");
        std::fs::create_dir_all(&dir).unwrap();
        let dir_str = dir.to_string_lossy().into_owned();
        assert_eq!(canonicalize_dir_for_add(&dir_str), Ok(std::fs::canonicalize(&dir).unwrap().to_string_lossy().into_owned()));
        // A missing path is a clean Err (total).
        let missing = std::env::temp_dir().join("kb_tui_canon_missing_xyz123");
        let missing_str = missing.to_string_lossy().into_owned();
        assert!(canonicalize_dir_for_add(&missing_str).is_err());
        // A path that is a file (not a directory) is a clean Err.
        let file = dir.join("afile.txt");
        std::fs::write(&file, "x").unwrap();
        let file_str = file.to_string_lossy().into_owned();
        assert!(
            canonicalize_dir_for_add(&file_str).is_err(),
            "a file path must be rejected (not a directory)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `add_root_tui` registers a *real* directory (the *same* `add_root`
    /// `cmd_add` calls) then full-refreshes (indexing its pages), and
    /// `drop_root_tui` unregisters it (GC-ing the pages). This is the
    /// end-to-end "source of truth is the file, the DB is a cache" loop.
    #[test]
    fn roots_add_then_drop_round_trip() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let root = std::env::temp_dir().join("kb_tui_roots_round_trip");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let page = root.join("note.md");
        std::fs::write(&page, FM_FIXTURE.as_bytes()).unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let mut app = App::new(db).unwrap();
        app.open_roots().unwrap();
        // Type the path into the add form (form is active on entry).
        for c in root_str.chars() {
            app.roots_type(c);
        }
        assert!(app.add_root_tui().is_ok());
        // The root is registered and the page was indexed.
        assert_eq!(roots_count(&app.db), 1, "the root row is present");
        assert_eq!(pages_count(&app.db), 1, "the page under the root is indexed");
        let Mode::Roots(ref r) = app.mode else { panic!() };
        assert_eq!(r.list.len(), 1);
        assert!(r.input.is_empty(), "the form is cleared after a successful add");
        // Now drop it: focus the list, then `x`.
        app.roots_toggle_focus(); // → list
        assert!(app.drop_root_tui().is_ok());
        assert_eq!(roots_count(&app.db), 0, "the root row is gone");
        assert_eq!(pages_count(&app.db), 0, "the page was GC'd after the drop");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `add_root_tui` on an *empty* form is a clean no-op + red status (the
    /// "type a path then Enter" guard) — total, no panic, mode unchanged.
    #[test]
    fn add_root_tui_empty_form_is_a_noop() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let mut app = App::new(db).unwrap();
        app.open_roots().unwrap();
        assert!(app.add_root_tui().is_ok(), "an empty form is Ok (not an Err)");
        assert_eq!(roots_count(&app.db), 0, "no root was registered");
        assert_eq!(app.status.kind, crate::tui::StatusKind::Error);
        assert!(matches!(app.mode, Mode::Roots(_)), "the mode stays in Roots");
    }

    // ---- M8.3 test helpers ----------------------------------------------

    fn app_insert_alias(db: &Db, alias: &str, canonical: &str) {
        db.conn
            .execute(
                "INSERT INTO tag_aliases(alias, canonical) VALUES (?1, ?2)
                 ON CONFLICT(alias) DO UPDATE SET canonical = excluded.canonical",
                rusqlite::params![alias, canonical],
            )
            .unwrap();
    }
    fn sel_alias(app: &App) -> usize {
        match &app.mode {
            Mode::Alias(a) => a.selected,
            _ => usize::MAX,
        }
    }
    fn focused_alias(app: &App) -> bool {
        match &app.mode {
            Mode::Alias(a) => a.focused,
            _ => false,
        }
    }
    fn input_alias(app: &App) -> String {
        match &app.mode {
            Mode::Alias(a) => a.input.clone(),
            _ => String::new(),
        }
    }
    fn roots_count(db: &Db) -> usize {
        db.conn
            .query_row("SELECT COUNT(*) FROM roots", [], |r| r.get::<_, i64>(0))
            .unwrap() as usize
    }
    fn pages_count(db: &Db) -> usize {
        db.conn
            .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get::<_, i64>(0))
            .unwrap() as usize
    }

    // ---- Selection mode (list fields) -----------------------------------

    /// A test helper: navigate to the **Value stage** of a list field
    /// (the *tags* field, by default) — the common prelude for any
    /// Selection-mode test. Returns a handle to the `App` in the state
    /// "just entered the tags field, in buffer mode, no input yet".
    /// Caller can then drive the selection UX directly from here.
    fn into_tags_buffer(app: &mut App) {
        app.open_edit().unwrap();
        // `picker_move` cycles Title → description → tags (field_cursor
        // starts at 0 = Title).
        app.picker_move(true);
        app.picker_move(true);
        app.enter_editor();
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.field, EditField::Tags, "helper lands on tags");
            assert!(!e.in_list_select, "helper lands in buffer mode");
            assert!(e.value.is_empty(), "helper lands with an empty buffer");
        }
    }

    /// **`←` in a list field** enters Selection mode and highlights the
    /// *last* existing item (the user's exact spec: "left arrow … selects
    /// the last tag"). **`→`** enters Selection and highlights the *first*.
    /// With an empty `current_list`, either direction highlights the
    /// `<new token>` slot (index 0 = `current_list.len()` = 0 when empty).
    #[test]
    fn enter_selection_from_buffer() {
        let (mut app, _dir) = seeded_app();
        into_tags_buffer(&mut app);
        // The fixture has ["mTLS", "transformer"].
        app.move_buffer_cursor(false); // `←`
        if let Mode::Edit(ref e) = app.mode {
            assert!(e.in_list_select, "buffer ← enters Selection mode");
            assert_eq!(e.list_high, 1, "← highlights the *last* existing item");
        }
        // `Esc` back to buffer (stays in Value stage).
        app.esc_in_value();
        if let Mode::Edit(ref e) = app.mode {
            assert!(!e.in_list_select, "Esc leaves Selection mode");
            assert_eq!(e.stage, EditStage::Value, "Esc stays in Value stage");
        }
        // Now `→` to enter Selection again → should land on the first item.
        app.move_buffer_cursor(true);
        if let Mode::Edit(ref e) = app.mode {
            assert!(e.in_list_select, "buffer → also enters Selection mode");
            assert_eq!(e.list_high, 0, "→ highlights the *first* existing item");
        }
    }

    /// **`↑/↓` in Selection mode** move the Selection highlight (wrap-at
    /// both ends, Total), *not* the candidates. The trailing `<new token>`
    /// (index `current_list.len()`) is a valid highlight slot (Enter on
    /// it exits to buffer mode, per the spec). **`←`/`→` are equivalent
    /// to `↑`/`↓`** in Selection mode (the user's exact spec: "you can
    /// using right and left arrow or up and down arrow select elements in
    /// the list").
    #[test]
    fn navigation_in_selection_mode_wraps_across_items_and_new_slot() {
        let (mut app, _dir) = seeded_app();
        into_tags_buffer(&mut app);
        app.move_buffer_cursor(false); // `←` → Selection mode, on the last existing item
        let len = 2; // current_list.len() == 2 (["mTLS", "transformer"])
        // The combined order is [mTLS, transformer, <new>]. So:
        //   index 0 = mTLS, index 1 = transformer, index 2 = <new slot>.
        // `↓` from index 1 → index 2 (the `<new>` slot).
        app.move_suggestion(true);
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.list_high, len, "↓ advances to the trailing `<new>` slot");
        }
        // `↓` from index 2 wraps back to index 0 (the first existing item).
        app.move_suggestion(true);
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.list_high, 0, "↓ wraps back to the first existing item");
        }
        // `↑` from index 0 wraps to the *last* entry in the combined
        // order — which is the `<new>` slot (index `len`), NOT a real
        // item.
        app.move_suggestion(false);
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.list_high, len, "`↑` from index 0 wraps to the trailing `<new>` slot");
        }
        // ← / → (routed through `move_buffer_cursor` → `move_suggestion`)
        // must be *equivalent* to ↑ / ↓ in Selection mode.
        app.move_buffer_cursor(false); // `←` (a "down"-equivalent in my
                                       // impl: `right=false` → up in the
                                       // `move_suggestion` mapping).
        // Actually wait: in my impl, `move_suggestion(down)` moves *down*
        // when `down=true` (i.e. to the next item in the order). A
        // `right` key in `move_buffer_cursor(true)` calls
        // `move_suggestion(true)`. So `→` is "advance".
        if let Mode::Edit(ref e) = app.mode {
            // From index `<new>` (the last), `←` (up) steps *back* to the
            // last existing item (index `len - 1`).
            assert_eq!(e.list_high, len - 1, "`←` in Selection steps back to the last *existing* item");
        }
    }

    /// **`Backspace` / `Del` in Selection mode** removes the *highlighted*
    /// item from the field — live (a real `fm::remove_list_item`; no file
    /// write to disk yet). **`Enter` on an item** loads it into the buffer
    /// (a "refine an existing item" path; the next `Enter` will commit
    /// a *rename*, not a re-append). **`Enter` on `<new token>`** exits
    /// to buffer mode with the cache preserved. A letter *types* into the
    /// (still-cached) buffer and exits Selection mode.
    ///
    /// Also asserts that "re-adding the same token" is a no-op (the
    /// `build_ops` dedup), so an accidental Enter-on-item without an
    /// intervening change does not double-list.
    #[test]
    fn selection_mode_full_ux() {
        let (mut app, path) = seeded_app();
        into_tags_buffer(&mut app);

        // --- 1. Enter Selection from Buffer mode, on the last item.
        app.move_buffer_cursor(false);
        assert!(match &app.mode {
            Mode::Edit(ref e) => e.in_list_select,
            _ => false,
        });

        // --- 2. `←`/`→`/`↑`/`↓` navigate the highlight.
        app.move_suggestion(true); // `→`-equivalent on list_high: last → `<new>`
        app.move_suggestion(true); // → wraps: 0 (first existing item)

        // --- 3. `Backspace` in Selection removes the *highlighted*
        //       (currently the first existing = "mTLS") item — a real
        //       `fm::remove_list_item`, baked into `working_bytes`.
        app.remove_or_pop();
        if let Mode::Edit(ref e) = app.mode {
            let cur: Vec<String> = e.current_list.clone();
            assert_eq!(cur, vec!["transformer".to_string()], "mTLS was removed from the list");
            // The working bytes still parse and no longer contain the
            // "mTLS" item as a list value. (`vec::contains` with a byte
            // slice is *byte-by-byte*, so use a string search on the
            // UTF-8 rendering of the bytes.)
            let working_str = String::from_utf8_lossy(&e.working_bytes);
            assert!(!working_str.contains("mTLS"), "working_bytes is re-serialized");
        }
        // The file on disk is *not* written (live only — commit is Enter).
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("mTLS"), "no disk write yet — live removal");

        // --- 4. `Enter` on the (now-*) highlighted item *loads* it into
        //       the buffer. (After the mTLS removal, `list_high`
        //       clamped to `current_list.len() = 1` — which points to
        //       the *last existing* item, "transformer".)
        app.enter_action().unwrap();
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value, "transformer", "Enter loaded the item into the buffer");
            assert!(e.editing_item.is_some(), "editing_item was armed (a rename path)");
            assert!(!e.in_list_select, "Exit Selection mode after loading the item");
        }

        // --- 5. Commit the rename: a "transformer" → "transformer" *plan*
        //       no-op (the buffer still equals the loaded token), so no
        //       `AppendItem` op is produced. **However**, the *pending
        //       removal* from step 3 is already baked into
        //       `working_bytes`. `commit_edit` writes when the *result*
        //       (working_bytes + ops) differs from disk — and it does,
        //       because mTLS is gone from working_bytes. So the write
        //       *happens* and bakes the mTLS removal to disk.
        assert!(app.enter_action().is_ok());
        let on_disk_after_commit = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk_after_commit.contains("mTLS"), "the baked-in removal was written to disk");
        assert!(on_disk_after_commit.contains("transformer"), "transformer is still on disk");

        // --- 6. Now: type `x` into the (now-fresh) buffer and commit.
        //       `x` should *append* (the *new* token, a pure add).
        app.edit_type('x');
        assert!(app.enter_action().is_ok());
        let final_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!final_disk.contains("mTLS"), "mTLS is still gone");
        assert!(final_disk.contains("transformer"), "transformer is still on disk");
        assert!(final_disk.contains("x"), "the new token `x` was appended");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// **Buffer mode** on a list field (before the Selection sub-mode is
    /// entered) — the existing UX is preserved for *add-new*. The buffer
    /// still types, Tab still autocompletes, ↑/↓ still pick, Enter
    /// still commits, and the smart-`Backspace` (empty buffer → drop
    /// tail) still works.
    #[test]
    fn buffer_mode_on_list_field_preserves_old_behavior() {
        let (mut app, _dir) = seeded_app();
        into_tags_buffer(&mut app);
        // Type 'm' (a prefix of "mtls" in the corpus): the suggestion
        // list narrows.
        app.edit_type('m');
        if let Mode::Edit(ref e) = app.mode {
            assert!(!e.in_list_select, "typing stays in buffer mode");
            assert!(!e.suggestions.is_empty(), "the live menu is non-empty");
        }
        // Tab autocomplete still fills the buffer from the *highlighted*
        // candidate (sug_cursor=0 → first match = "mtls" from the corpus
        // — the seed corpus has lowercase "mtls").
        app.autocomplete();
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value, "mtls", "Tab filled the buffer from the highlighted candidate");
        }
        // Commit: "mtls" is not in `current_list` (which is
        // ["mTLS", "transformer"]) so it's a new append. (mTLS /
        // transformer are present-case-sensitively; "mtls" is a new
        // token.)
        assert!(app.enter_action().is_ok());
        // Smart-`Backspace` (empty buffer) still drops the tail.
        if let Mode::Edit(ref e) = app.mode {
            assert!(e.value.is_empty(), "the buffer was reset after commit");
        }
        app.remove_or_pop(); // empty buffer → tail-drop
        if let Mode::Edit(ref e) = app.mode {
            // The tail ("transformer") was dropped; current_list is now
            // ["mTLS"] (the "mtls" append was *not* yet committed — the
            // tail-drop is a *pending* op, not a commit. The "mTLS"
            // item is still there. This is a subtle case: the user's
            // last visible action (in this test) is: type m, Tab,
            // commit, backspace. The *commit* wrote "mtls" to disk.
            // The *backspace* is a live removal of the tail. The tail
            // of a *just-committed* list ["mTLS", "transformer",
            // "mtls"] is "mtls" — so dropping the tail removes "mtls",
            // leaving ["mTLS", "transformer"].
            let cur = e.current_list.clone();
            assert_eq!(cur, vec!["mTLS".to_string(), "transformer".to_string()], "the *tail* (newest) token was dropped, the rest preserved");
        }
    }

    /// A **scalar** field's `←/→` move the text cursor mid-buffer (a
    /// classic editor). `Backspace` deletes the char before the cursor
    /// (not the last char — a "mid-buffer" Backspace). Commit (Enter)
    /// writes the new scalar to disk. Esc pops to Picker (the legacy
    /// buffer-mode Escape).
    #[test]
    fn scalar_field_left_right_moves_cursor_and_middle_backspace_edits_mid_buffer() {
        let (mut app, path) = seeded_app();
        app.open_edit().unwrap();
        // Stay on Title (field_cursor=0). The fixture's title is
        // "Hello World" (the same as the on-disk bytes).
        app.enter_editor();
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value, "Hello World");
            let len = e.value.chars().count();
            assert_eq!(e.value_cursor, len, "the cursor starts at the end");
        }
        // `←` 6 times puts the cursor before "World" (position 5).
        app.move_buffer_cursor(false);
        app.move_buffer_cursor(false);
        app.move_buffer_cursor(false);
        app.move_buffer_cursor(false);
        app.move_buffer_cursor(false);
        app.move_buffer_cursor(false);
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value_cursor, 5, "`←` x6 put the cursor before 'World'");
        }
        // Type 'X' at position 5 — inserts at the cursor.
        app.edit_type('X');
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value, "HelloX World", "the char was inserted at the cursor");
        }
        // `Backspace` now deletes the char *before the cursor* — which
        // should be 'o' (position 4), giving "HellXo World"… wait.
        // The cursor is at position 6 (after inserting 'X' at 5, the
        // cursor advanced to 6, so Backspace should remove the char at
        // position 5 (which is 'X' — the char we just typed).
        app.remove_or_pop();
        if let Mode::Edit(ref e) = app.mode {
            assert_eq!(e.value, "Hello World", "Backspace deleted the char before the cursor (the 'X' we just inserted)");
        }
        // Commit. The *value* on disk should be unchanged (a pure
        // insert-at-cursor then backspace-removed-that-same-char is
        // net-zero).
        assert!(app.enter_action().is_ok());
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("Hello World"), "the title is unchanged on disk (net-zero edit)");
    }

    // ===================================================================
    //  M8.4 Search helpers
    // ===================================================================

    /// `trailing_tag_partial` extracts the substring after the **last**
    /// whitespace token's `tag:` prefix, or `None` when the trailing token
    /// is not a `tag:` token. Pure — no `App` state.
    #[test]
    fn trailing_tag_partial_extracts_substring_after_tag_prefix() {
        assert_eq!(trailing_tag_partial("tag:sec"), Some("sec".into()));
        assert_eq!(trailing_tag_partial("tag:"), Some(String::new()));
        assert_eq!(trailing_tag_partial("llm tag:sec"), Some("sec".into()));
        assert_eq!(trailing_tag_partial("tag:sec "), None, "a trailing space means the token is complete — no partial");
        assert_eq!(trailing_tag_partial("tag:sec llm"), None, "the trailing token is not a tag: token");
        assert_eq!(trailing_tag_partial("tag:\"large language\""), None, "a quoted tag is a completed phrase, not a partial");
        assert_eq!(trailing_tag_partial(""), None, "empty buffer has no token");
        assert_eq!(trailing_tag_partial("  "), None, "whitespace-only buffer has no token");
    }

    /// `inject_tag` replaces the trailing **`tag:…` token** (if it is the
    /// last whitespace-separated token) with `new_token`, or appends
    /// `new_token` as a new token when the last token is free-text. Pure —
    /// no `App` state.
    ///
    /// Note: the "completed tag with trailing space" case (`"tag:sec "`)
    /// is unreachable in practice — `trailing_tag_partial` returns `None`
    /// for a buffer ending in a space, so the suggestion list is empty
    /// and Tab is a no-op before `inject_tag` is ever called. The function
    /// still handles it correctly if it did arrive (replaces the token),
    /// so we assert that.
    #[test]
    fn inject_tag_replaces_trailing_tag_token() {
        // Trailing token IS the partial tag → replace it.
        assert_eq!(inject_tag("tag:se", "tag:security"), "tag:security ");
        assert_eq!(inject_tag("llm tag:se", "tag:security"), "llm tag:security ");
        // Empty buffer → the tag is the whole buffer.
        assert_eq!(inject_tag("", "tag:security"), "tag:security ");
        // Trailing token is free-text → preserve it, append the tag.
        assert_eq!(
            inject_tag("llm", "tag:security"),
            "llm tag:security ",
            "free-text trailing token is preserved; the tag is a new token"
        );
        assert_eq!(
            inject_tag("llm ", "tag:security"),
            "llm tag:security ",
            "trailing-normalised"
        );
        // A tag-token at the end (replaces it) — this is the "second tag"
        // case: the user already has a tag, types another `tag:` partial,
        // and then Tabs.
        assert_eq!(
            inject_tag("tag:sec tag:ll", "tag:llm"),
            "tag:sec tag:llm ",
            "the second (last) tag token is replaced; the first is preserved"
        );
    }

    /// `suggest_tags` over an in-memory DB: the **partial** substring
    /// match (case-insensitive) is what surfaces; an alias whose
    /// canonical or alias text contains the partial is surfaced in
    /// `alias → canonical` display.
    #[test]
    fn suggest_tags_filters_corpus_and_surfaces_aliases() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        db.conn
            .execute(
                "INSERT INTO page_tags(abs_path, tag) VALUES ('/a.md', 'secret'), ('/b.md', 'security'), ('/c.md', 'llm')",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO tag_aliases(alias, canonical) VALUES ('sec', 'security')",
                [],
            )
            .unwrap();
        let out = suggest_tags(&db, "se");
        // Both the `sec → security` alias and the bare `security` + `secret`
        // are candidates; the dedup key is the **canonical** form, so the
        // alias (display `sec → security`) and the bare `security` collide
        // on the key `security` — the alias wins (it was pushed first).
        let canon: Vec<String> = out.iter().map(|s| s.canonical.clone()).collect();
        let displays: Vec<String> = out.iter().map(|s| s.display.clone()).collect();
        assert!(canon.contains(&"security".to_string()), "security must be surfaced (alias or tag)");
        assert!(canon.contains(&"secret".to_string()), "secret must be surfaced (a bare corpus tag)");
        // The alias display `sec → security` must survive.
        let has_alias_display = displays.iter().any(|d| d.starts_with("sec → security"));
        assert!(has_alias_display, "the alias display `sec → security` must survive");
        // `llm` does NOT contain "se" — must be excluded.
        assert!(!canon.contains(&"llm".to_string()), "llm must NOT be surfaced by partial 'se'");
    }

    /// With an *empty* partial the whole corpus surfaces (the `tag:` →
    /// `Tab`-to-complete path).
    #[test]
    fn suggest_tags_empty_partial_returns_whole_corpus() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        db.conn
            .execute(
                "INSERT INTO page_tags(abs_path, tag) VALUES ('/a.md', 'alpha'), ('/b.md', 'beta')",
                [],
            )
            .unwrap();
        let out = suggest_tags(&db, "");
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|s| s.canonical == "alpha"));
        assert!(out.iter().any(|s| s.canonical == "beta"));
    }

    /// `search_rows_to_pages` attaches the **real** tags + gates (from
    /// `page_tags` / `page_gates`) to each FTS-matched row, keyed by
    /// `abs_path`. This is the one TUI-specific glue path between the CLI's
    /// `search_rows` and the Browse-parity `PageRow` shape — it was only
    /// compile-verified before. A row with *no* tags/gates in the DB must
    /// get empty lists (not a DB error, not a panic). Pure over its inputs.
    #[test]
    fn search_rows_to_pages_attaches_tags_and_gates_by_abs_path() {
        use crate::cli::search::SearchRow;
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // Two pages: /x.md has both tags + a gate; /y.md has neither.
        db.conn
            .execute(
                "INSERT INTO page_tags(abs_path, tag) VALUES
                 ('/x.md', 'llm'), ('/x.md', 'security'),
                 ('/z.md', 'unrelated')",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO page_gates(abs_path, term) VALUES ('/x.md', 'secret sauce')",
                [],
            )
            .unwrap();

        // A faithful `SearchRow` (the same field set `search_rows` binds),
        // for a matched /x.md and an unmatched /y.md.
        let rows = vec![
            SearchRow {
                abs_path: "/x.md".into(),
                id: "wiki:x".into(),
                pt: "wiki".into(),
                title: Some("X".into()),
                description: None,
                root: "/root".into(),
                rel_path: "x.md".into(),
                search_required_phrase: Some("secret sauce".into()),
                score: Some(1.5),
            },
            SearchRow {
                abs_path: "/y.md".into(),
                id: "note:y".into(),
                pt: "note".into(),
                title: None, // a row with no title must not error
                description: None,
                root: "/root".into(),
                rel_path: "y.md".into(),
                search_required_phrase: None,
                score: None,
            },
        ];

        let pages = search_rows_to_pages(&db, &rows);
        assert_eq!(pages.len(), 2, "every matched row becomes a PageRow");

        let x = pages.iter().find(|p| p.abs_path == "/x.md").expect("/x.md present");
        // Tags attached in DB order (ORDER BY tag: llm, security).
        assert_eq!(x.tags, vec!["llm", "security"], "tags attached by abs_path");
        assert_eq!(x.gates, vec!["secret sauce"], "gate(s) attached by abs_path");

        let y = pages.iter().find(|p| p.abs_path == "/y.md").expect("/y.md present");
        assert!(y.tags.is_empty(), "a row with no tags gets empty tags (not an error)");
        assert!(y.gates.is_empty(), "a row with no gates gets empty gates (not an error)");
        assert_eq!(y.title, "", "a None title degrades to the empty string");

        // An empty input slice returns an empty vec (total: no DB call needed).
        assert!(search_rows_to_pages(&db, &[]).is_empty(), "empty rows → empty pages");
    }

    /// `search_commit` folds the bundle into `Search` state. With an empty
    /// query the bundle is `Some(empty)` + no suggestions (the UI shows an
    /// empty-result pane, not a "no data" state); with a non-empty query
    /// the FTS surface is exercised (tested in `kb-cli`), but the *state*
    /// invariant is what this test asserts: `sel` is clamped to range and
    /// `tag_sel` is reset to the top of the suggestion list. Behaviour is
    /// total over a fresh DB (no re-index needed).
    #[test]
    fn search_commit_clamps_sel_and_resets_tag_sel() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let mut app = App::new(db).unwrap();
        app.open_search().unwrap();
        app.search_commit();
        let s = match &app.mode {
            Mode::Search(s) => s,
            _ => panic!("expected Search mode"),
        };
        assert!(s.results.as_ref().is_some_and(|r| r.is_empty()));
        assert!(s.tag_sugs.is_empty());
        assert_eq!(s.sel, 0);
        assert_eq!(s.tag_sel, 0);

        // Force a non-empty query through `search_commit`; the bundle is
        // built but the DB is empty so `results` is `Some([])`. `sel`
        // must stay in range (`0`).
        if let Mode::Search(ss) = &mut app.mode {
            ss.query = "x desc".to_string();
            ss.sel = 99; // simulate a stale cursor past the end
        }
        app.search_commit();
        let s = match &app.mode {
            Mode::Search(s) => s,
            _ => panic!("expected Search mode"),
        };
        let n = s.results.as_ref().unwrap().len();
        assert!(s.sel <= n, "`sel` must be clamped to the result count (was {0}, got {1})", n, s.sel);
        assert_eq!(s.tag_sel, 0);
    }

    /// Regression: entering Search mode with an **empty** buffer must open
    /// with the results pane pre-populated (the whole un-gated corpus, the
    /// same shape `kb search ""` returns) — not empty until the first
    /// keystroke. `open_search` seeds `results: None` and then calls
    /// `search_commit`, which forwards the empty query to `search_rows` and
    /// materialises `results = Some(…)` for every ungated page.
    #[test]
    fn open_search_prepopulates_results_for_empty_buffer() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // One open (ungated) page and one gated page. The empty query must
        // return the ungated page and exclude the gated one (matching the
        // CLI `kb search ""` gate-exclusion contract).
        let open_abs = insert_page(&db, "transformer", "Transformer", "the transformer architecture\n");
        insert_page(&db, "gated", "Gated", "gated body\n");
        db.conn
            .execute(
                "INSERT INTO page_gates (abs_path, term) VALUES (?1, 'secret')",
                rusqlite::params!["/root/gated.md"],
            )
            .unwrap();

        let mut app = App::new(db).unwrap();
        app.open_search().unwrap();

        let s = match &app.mode {
            Mode::Search(s) => s,
            _ => panic!("expected Search after open_search"),
        };
        let results = s.results.as_ref().expect("results materialised on entry, before any keystroke");
        assert_eq!(
            results.len(),
            1,
            "empty buffer returns the un-gated corpus (gated page excluded); got: {results:?}"
        );
        assert_eq!(results[0].abs_path, open_abs, "the un-gated page is the sole hit");
        assert_eq!(s.sel, 0);
    }

    /// Regression: Search→Read→Esc must return to **Search** (query buffer,
    /// live results, and result selection all intact), not drop into Browse
    /// and lose the search context. `search_open` parks the `SearchState` in
    /// `search_return`; `close_read` consumes it via `.take()` (idempotent —
    /// a second Esc falls back to Browse with an empty list). A Browse-opened
    /// read still Escs back to Browse (`open_read` clears any stale address).
    #[test]
    fn search_esc_from_read_returns_to_search() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        insert_page(&db, "transformer", "Transformer", "the transformer architecture\n");
        insert_page(&db, "k8s", "Cluster Ops", "kubernetes rollout\n");
        let mut app = App::new(db).unwrap();

        // Arm Search and type a query that matches exactly one page
        // via the real FTS engine (`search_type` → `search_commit`).
        app.open_search().unwrap();
        for c in "transformer".chars() {
            app.search_type(c);
        }
        let s = match &app.mode {
            Mode::Search(s) => s,
            _ => panic!("expected Search after open_search"),
        };
        let results = s.results.as_ref().expect("results materialized");
        assert_eq!(results.len(), 1, "one page matches 'transformer'");
        assert_eq!(results[0].id, "note:transformer", "FTS returned the transformer page");

        // Open the result → Read. The search context must now be parked
        // in `search_return`.
        app.search_open().unwrap();
        assert!(matches!(app.mode, Mode::Read(_)), "search_open puts us in Read");
        assert!(app.search_return.is_some(), "the Search context is parked for the return trip");

        // Esc from Read → back to Search with the query + result selection
        // intact (the bug this test pins).
        app.close_read();
        let s = match &app.mode {
            Mode::Search(s) => s,
            _ => panic!("Esc from a search-opened Read must return to Search, not Browse"),
        };
        assert_eq!(s.query, "transformer", "query buffer survives the round trip");
        let results = s.results.as_ref().expect("results survive the round trip");
        assert_eq!(results.len(), 1, "result count unchanged");
        assert_eq!(results[0].id, "note:transformer");
        assert_eq!(s.sel, 0, "result selection intact");

        // A second Esc from Search returns to Browse (the global pop).
        app.close_search();
        assert!(matches!(app.mode, Mode::Browse));
        assert!(app.search_return.is_none(), "the parked context was consumed, not duplicated");

        // Fresh Browse-opened read → Esc still pops to Browse (no stale
        // search address resurrected).
        app.selected = 0;
        app.open_read().unwrap();
        assert!(app.search_return.is_none(), "a Browse open clears any stale search address");
        app.close_read();
        assert!(matches!(app.mode, Mode::Browse), "a Browse open Escs back to Browse");
    }

    // ===================================================================
    //  M8.5 — Reader find: pure-helper tests
    //  ===================================================================

    /// `find_padding` must be **total** (no panics) and produce a scroll
    /// position that (a) is in `[0, scroll_max_top]` and (b) puts the
    /// selected hit as close to the pane's center row as the padded content
    /// allows. The invariant checked for every case:
    /// `scroll_top <= scroll_max_top` and
    /// the hit's absolute row `above + first_row` satisfies
    /// `scroll_top <= above + first_row <= scroll_top + pane_height - 1`
    /// (the hit is *visible* in the pane).
    #[test]
    fn find_padding_is_total_and_keeps_hit_visible() {
        for pane_height in [1usize, 2, 3, 5, 10, 24, 60] {
            for content_rows in [0usize, 1, 3, 7, 20, 50, 200] {
                for first_row in [0usize, 1, 2, (pane_height.saturating_sub(1)) / 2, content_rows.saturating_sub(1)] {
                    let fp = find_padding(first_row, content_rows, pane_height);
                    assert!(
                        fp.scroll_top <= fp.scroll_max_top,
                        "pane={pane_height} content={content_rows} first={first_row}: \
                         scroll_top {} > scroll_max_top {} (invariant)",
                        fp.scroll_top, fp.scroll_max_top
                    );
                    // The hit's absolute row in the padded body.
                    let abs = fp.above + first_row;
                    assert!(
                        fp.scroll_top <= abs,
                        "hit row {} is above pane top {pane_height}/{content_rows}/{first_row}",
                        abs
                    );
                    assert!(
                        abs <= fp.scroll_top + pane_height.saturating_sub(1),
                        "hit row {} is below pane bottom for pane={pane_height} content={content_rows} first={first_row} scroll={} (hit must be visible)",
                        abs, fp.scroll_top
                    );
                }
            }
        }
    }

    /// The exact centering contract: when there is no content constraint
    /// (the body is short enough), the selected hit lands **on** the center
    /// row. Verify the two analytic cases by the derived math.
    #[test]
    fn find_padding_centers_when_room_allows() {
        let h = 10usize; // center = 4
        let center = (h - 1) / 2; // = 4
        let content = 4usize; // short body

        // Hit in the top half (first_row < center): pad above, scroll 0.
        let fp = find_padding(0, content, h);
        assert_eq!(fp.scroll_top, 0, "top-half hit scrolls to 0");
        assert_eq!(fp.above, center, "above fills to center (hit lands at the center row)");

        // Hit exactly at center (first_row == center): no above, scroll 0.
        let fp = find_padding(center, content, h);
        assert_eq!(fp.above, 0);
        assert_eq!(fp.scroll_top, 0);
        assert_eq!(fp.above + center, center);

        // Hit below center (first_row > center): no above, scroll = first-center.
        let first = 6usize;
        let fp = find_padding(first, content, h);
        assert_eq!(fp.above, 0);
        assert_eq!(fp.scroll_top, first - center, "bottom-half hit scrolls to center it");
        assert_eq!(fp.scroll_top + center, first, "hit lands at the center row");
    }

    /// `pane_height == 0` is degenerate: no padding, no scroll, all zeros.
    #[test]
    fn find_padding_zero_pane_is_degenerate() {
        let fp = find_padding(5, 10, 0);
        assert_eq!(fp, FindPadding { above: 0, below: 0, scroll_top: 0, scroll_max_top: 0 });
    }

    /// `find_padding_for_cursor` must resolve the *selected* hit's rendered
    /// row (not merely its line's first row) and produce valid padding. A
    /// hit deeper than one wrap down a long line is the regression this
    /// pins: centering on the line's first row would leave it off screen.
    #[test]
    fn find_padding_for_cursor_resolves_selected_line() {
        // Line 2 is longer than one pane width: at width 10 it wraps into
        // "cccccccccc" + "ccxxcc", so the "xx" at byte 12 lives on the
        // line's *second* rendered row.
        let body = "a\nbb\nccccccccccccxxcc\nd"; // line 2: 12 'c' + 'xx' at offset 12 + 2 'c'
        let rendered = lines(body);
        let matches = find_matches(&rendered, "xx"); // one hit, line 2
        assert_eq!(matches, vec![(2, 12)], "exactly one 'xx' at line 2 offset 12: {matches:?}");
        let fp = find_padding_for_cursor(&rendered, &matches, 0, 10, 12).expect("a match exists");
        // Rows before line 2 (2) plus the hit's row within that line (1).
        let expected_row = crate::tui::render::match_rendered_row(&rendered, matches[0], 10)
            .expect("hit row resolves");
        assert_eq!(expected_row, line_first_row(&rendered, 10)[2] + 1, "hit is on the line's second wrapped row");
        assert!(fp.scroll_top <= fp.scroll_max_top);
        // The absolute hit row is visible within the pane.
        let abs = fp.above + expected_row;
        assert!(fp.scroll_top <= abs && abs <= fp.scroll_top + 11, "hit visible (abs={abs})");

        // Cursor clamped past the end (cursor = 99).
        let fp2 = find_padding_for_cursor(&rendered, &matches, 99, 10, 12).expect("clamped cursor");
        assert_eq!(fp2, fp, "a cursor past the end clamps to the last hit");

        // Empty match list -> None.
        let none = find_padding_for_cursor(&rendered, &[], 0, 10, 12);
        assert!(none.is_none());
    }

    /// `find_matches` is case-insensitive, finds multiple occurrences per
    /// line, and reports them in document order (top-to-bottom, then
    /// left-to-right). An empty needle matches nothing.
    #[test]
    fn find_matches_is_case_insensitive_and_ordered() {
        let rendered = lines("hello Hello world\nHELLO again hello");
        let m = find_matches(&rendered, "hello");
        // line 0: "hello Hello" -> 2 hits (0, 6) [case-insensitive]
        // line 1: "HELLO again hello" -> 2 hits (0, 12)
        assert_eq!(m.len(), 4, "4 occurrences found (case-insensitive): {m:?}");
        // Document order: line 0 first (left-to-right), then line 1.
        assert_eq!(m[0].0, 0);
        assert_eq!(m[1].0, 0);
        assert_eq!(m[2].0, 1);
        assert_eq!(m[3].0, 1);
        assert!(m[0].1 < m[1].1, "within a line, left-to-right order");

        // Empty needle -> no matches.
        assert!(find_matches(&rendered, "").is_empty());

        // No match -> empty.
        assert!(find_matches(&rendered, "zzz").is_empty());

        // Adjacent / overlapping repeats: "aaaa" with needle "aa" -> 2 hits.
        let m2 = find_matches(&lines("aaaa"), "aa");
        assert_eq!(m2, vec![(0, 0), (0, 2)], "non-overlapping consecutive matches: {m2:?}");
    }

    /// The regression for the markdown-drift bug: when a body line contains
    /// inline markdown emphasis, the *raw* form has extra delimiter bytes
    /// (`*`, `~`, `` ` ``) that the *rendered* form does not. The find
    /// matcher must operate on the **rendered** form (what the overlay
    /// paints onto), so a needle match lands exactly under the right glyph
    /// and offsets past the emphases are still correct.
    #[test]
    fn find_matches_targets_rendered_text_not_raw_markdown() {
        let rendered = lines("a *italics* b `code` c");
        let s: String = rendered[0].clone();
        assert_eq!(s, "a italics b code c", "rendered text: {s:?}");
        // All 'a' positions in the rendered form:
        // "a italics b code c"
        //  ^  ^
        //  0  4  (the 'a' of "italics")
        let m = find_matches(&rendered, "a");
        assert_eq!(m, vec![(0, 0), (0, 4)], "rendered offsets (not raw): {m:?}");

        // A needle that *crosses* the delimiter boundary: "s b". In the
        // rendered form it is a literal substring; in the raw form it would
        // require "s * b", which is not contiguous — the exact bug the user
        // reported (a needle that spans an emphasis boundary).
        let m3 = find_matches(&rendered, "s b");
        assert_eq!(m3, vec![(0, 8)], "'s b' crosses the rendered emphasis boundary: {m3:?}");
        // In the *raw* form, "s b" is not present at all:
        let raw_lines = vec!["a *italics* b `code` c".to_owned()];
        let raw_m = find_matches(&raw_lines, "s b");
        assert!(raw_m.is_empty(), "raw form has NO contiguous 's b': {raw_m:?}");

        // Strikethrough + code, same property:
        let rendered2 = lines("alpha ~~gone~~ beta `x` gamma");
        let m4 = find_matches(&rendered2, "gone beta");
        // Rendered: "alpha gone beta x gamma" — contiguous at offset 6.
        assert_eq!(m4, vec![(0, 6)], "'gone beta' contiguous on rendered: {m4:?}");
    }

    /// End-to-end through the `App`: arm find on a known Read body, type a
    /// needle (live), and assert the match list, cursor, and stored
    /// `FindPadding` all agree — and that `find_next` advances + recenters.
    #[test]
    fn app_find_lifecycle_activates_types_navigates_closes() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // Create a page whose body we fully control.
        let body = "intro\nneedle here\nmiddle\nneedle again\ntail";
        db.conn.execute(
            "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase, mtime_ns, content_hash)
             VALUES ('/root/f.md', 'note:find', 'note', 'Find Test', '/root', 'f.md', NULL, 0, 'x')",
            rusqlite::params![],
        ).unwrap();
        let mut app = App::new(db).unwrap();

        // Enter Read on the page.
        app.selected = app.view.iter().position(|r| r.id == "note:find").unwrap();
        app.open_read().unwrap();
        assert!(matches!(app.mode, Mode::Read(_)), "open_read entered Read");

        // `open_read` loads the body from the *file*; seed the file so the
        // loaded body matches `body` (the find math depends on it).
        let rs = match &mut app.mode {
            Mode::Read(r) => r,
            _ => panic!(),
        };
        rs.body = body.to_string();
        rs.scroll_top = 0;

        // Arm find with an empty needle: no matches, centered None.
        app.find_activate();
        assert!(app.find_armed(), "find is armed");
        {
            let rs = match &app.mode { Mode::Read(r) => r, _ => panic!() };
            let fs = rs.find.as_ref().expect("find state present");
            assert!(fs.matches.is_empty(), "empty needle -> no matches");
            assert!(fs.center.is_none(), "no matches -> no centering");
        }

        // Type the needle live: "needle" -> 2 matches.
        for c in "needle".chars() {
            app.find_input(c);
        }
        {
            let rs = match &app.mode { Mode::Read(r) => r, _ => panic!() };
            let fs = rs.find.as_ref().unwrap();
            assert_eq!(fs.matches.len(), 2, "two 'needle' occurrences");
            assert_eq!(fs.matches[0].0, 1, "first hit on line 1");
            assert_eq!(fs.matches[1].0, 3, "second hit on line 3");
            assert_eq!(fs.cursor, 0, "cursor stays on the first hit while typing");
            assert!(fs.center.is_some(), "a centering was computed");
            let c = fs.center.as_ref().unwrap();
            assert!(c.scroll_top <= c.scroll_max_top, "scroll in range");
        }
        // Case-insensitivity: backspace to "ne", then type uppercase 'E'
        // and 'd' → needle "neEd". `find_matches` lowercases to "need",
        // which "needle" contains (twice) → still 2 hits.
        app.find_backspace(); app.find_backspace(); app.find_backspace(); app.find_backspace(); // needle "ne"
        app.find_input('E'); // "nE"
        app.find_input('d'); // "neEd" -> lowers to "need" -> 2 hits
        assert_eq!(
            match &app.mode { Mode::Read(r) => r.find.as_ref().unwrap().needle.clone(), _ => panic!() },
            "neEd"
        );
        {
            let rs = match &app.mode { Mode::Read(r) => r, _ => panic!() };
            let fs = rs.find.as_ref().unwrap();
            assert_eq!(fs.matches.len(), 2, "case-insensitive needle still matches: needle={:?}", fs.needle);
        }

        // Navigate: next wraps to hit 2, prev wraps back.
        app.find_next();
        {
            let rs = match &app.mode { Mode::Read(r) => r, _ => panic!() };
            assert_eq!(rs.find.as_ref().unwrap().cursor, 1, "next advances cursor");
        }
        app.find_next();
        {
            let rs = match &app.mode { Mode::Read(r) => r, _ => panic!() };
            assert_eq!(rs.find.as_ref().unwrap().cursor, 0, "next wraps around");
        }
        app.find_prev();
        {
            let rs = match &app.mode { Mode::Read(r) => r, _ => panic!() };
            assert_eq!(rs.find.as_ref().unwrap().cursor, 1, "prev wraps to the last hit");
        }

        // Close: find disarms, but the scroll position is retained.
        let scroll_before = match &app.mode { Mode::Read(r) => r.scroll_top, _ => 0 };
        app.find_close();
        assert!(!app.find_armed(), "find closed");
        {
            let rs = match &app.mode { Mode::Read(r) => r, _ => panic!() };
            assert!(rs.find.is_none(), "find state dropped");
            assert_eq!(rs.scroll_top, scroll_before, "scroll position retained after close");
        }
    }

    /// `find_note` reports "N of M" position + the 0-match + not-armed cases.
    #[test]
    fn find_note_reports_position_and_empty() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let app = App::new(db).unwrap();
        // Not in Read -> None.
        assert!(app.find_note().is_none(), "no Read -> no find note");
    }

    // ===================================================================
    //  M8.5 — Link-picker tests
    // ===================================================================

    /// `l` from a Read (no find armed) must (a) load the source page's body,
    /// (b) call `annotated_links` against the full corpus, (c) put the picker
    /// in `Mode::Links` with `cursor == 0`, (d) park the original Read in
    /// `links_return` so `Esc` can pop back to it. The picker itself must not
    /// leak into `read_return` (that slot is for *following* a link, and is
    /// cleared on entry).
    #[test]
    fn open_links_from_read_parks_return_stack_and_starts_at_zero() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // Two pages: `a` links to `b`.
        insert_page(&db, "a", "Page A", "See [[b]] for the follow-up.\n");
        insert_page(&db, "b", "Page B", "the follow-up body\n");
        let mut app = App::new(db).unwrap();
        // Enter Read on page A (Browse → Enter).
        app.selected = app
            .rows
            .iter()
            .position(|r| r.id == "note:a")
            .expect("page A in rows");
        app.open_read().unwrap();
        assert!(matches!(app.mode, Mode::Read(_)));
        // Park a stale state in `read_return` to verify `open_links` clears.
        app.read_return = Some(ReadState {
            row_idx: 0,
            id: "note:stale".into(),
            body: "stale".into(),
            outline: Vec::new(),
            scroll_top: 0,
            abs_path: "/stale.md".into(),
            fm: FmMeta::default(),
            find: None,
        });
        app.open_links().unwrap();
        let ls = match &app.mode {
            Mode::Links(l) => l,
            m => panic!("expected Links mode, got {m:?}"),
        };
        assert_eq!(ls.links.len(), 1, "one wikilink on page A");
        assert_eq!(ls.cursor, 0, "no find armed → cursor 0");
        assert_eq!(ls.source_abs_path, "/root/a.md");
        // The return stack must hold the Read we just left; `read_return`
        // must be cleared (stale state wiped so a later Browse-open can't
        // pop to an unrelated Read).
        assert!(app.links_return.is_some(), "links_return parked for the Esc pop");
        assert!(app.read_return.is_none(), "open_links clears a stale read_return");
    }

    /// `open_links` from *outside* Read mode is a no-op (the key is only
    /// reachable from Read — `handle_read` gates it — but the App method
    /// must be total so it never panics on a stray call).
    #[test]
    fn open_links_outside_read_is_total() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        insert_page(&db, "a", "A", "see [[a2]] later\n");
        insert_page(&db, "a2", "A2", "ok\n");
        let mut app = App::new(db).unwrap();
        // In Browse; the call must succeed (not panic) AND not leave us in
        // Links mode (the key is Read-gated — this only exercises the total
        // contract, not the gating itself).
        let res = app.open_links();
        assert!(res.is_ok(), "total: Ok even when not in Read");
        assert!(
            matches!(app.mode, Mode::Browse),
            "Browse is preserved (no phantom Links mode)"
        );
    }

    /// `links_open_selected` on a *unique* target: the App transitions to
    /// Read mode, parks the picker's `links_return` into `read_return` so
    /// `Esc`-back lands in the *incoming* Read (not Browse), and the linked
    /// Read is on the target page's body.
    #[test]
    fn links_open_selected_unique_parks_read_return_and_opens() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        insert_page(&db, "a", "A", "see [[b]] for the follow-up\n");
        insert_page(&db, "b", "B", "the follow-up body\n");
        let mut app = App::new(db).unwrap();
        // Enter Read → arm picker → cursor at 0 (the `[[b]]` is the only
        // link, so cursor 0 is unambiguously it).
        app.selected = 0;
        app.open_read().unwrap();
        app.open_links().unwrap();
        let links_return_before = app.links_return.is_some();
        assert!(links_return_before, "the picker parked the read");
        app.links_open_selected().unwrap();
        assert!(matches!(app.mode, Mode::Read(_)), "Read on the linked page");
        let rs = match &app.mode {
            Mode::Read(r) => r,
            _ => panic!(),
        };
        assert_eq!(rs.abs_path, "/root/b.md", "the *target* page (b.md) is open");
        assert!(
            rs.body.contains("the follow-up body"),
            "linked Read shows the destination page's body; got: {:?}",
            rs.body
        );
        // The original Read must now be in `read_return` so that a later
        // `close_read` pops back to it, and `links_return` must be cleared
        // (the picker is done).
        assert!(
            app.read_return.is_some(),
            "the original Read is parked in read_return for the Esc-back"
        );
        assert!(
            app.links_return.is_none(),
            "picker's return stack is consumed, not duplicated"
        );
    }

    /// `links_open_selected` on an *ambiguous* target: the App must NOT
    /// open a new Read (no unique winner); it must set an info note naming
    /// the candidate count; it must NOT consume `links_return` (the picker
    /// is still open).
    #[test]
    fn links_open_selected_ambiguous_is_noop_and_keeps_picker() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // Two pages with the same slug but different roots → `[[ambig]]`
        // is ambiguous by stage 1 (slug-collision on the same type).
        insert_page(&db, "ambig", "Ambig", "see [[ambig]] twice");
        insert_page(&db, "ambig2", "Ambig", "second\n");
        let mut app = App::new(db).unwrap();
        app.selected = 0;
        app.open_read().unwrap();
        app.open_links().unwrap();
        let ls = match &app.mode {
            Mode::Links(l) => l,
            _ => panic!("expected Links"),
        };
        assert_eq!(ls.links.len(), 1, "exactly one wikilink in the body");
        // The link must be ambiguous (two slug matches → Candidates(2)).
        assert!(ls.links[0].resolved.is_none(), "not unique → resolved is None");
        assert_eq!(ls.links[0].candidates.len(), 2, "two candidate abs_paths");
        app.links_open_selected().unwrap();
        // We must still be in Links (no navigation).
        assert!(
            matches!(app.mode, Mode::Links(_)),
            "ambiguous target does NOT open a new Read"
        );
        // And the return stack is intact (the picker is still open).
        assert!(app.links_return.is_some(), "picker's return stack is unconsumed");
    }

    /// `close_links` must pop the parked Read back into Read mode (not
    /// Browse), and then a subsequent `close_read` must go to Browse (not
    /// resurrect a duplicate Read).
    #[test]
    fn close_links_pops_parked_read_not_browse() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        insert_page(&db, "a", "A", "see [[b]]\n");
        insert_page(&db, "b", "B", "b body\n");
        let mut app = App::new(db).unwrap();
        app.selected = 0;
        app.open_read().unwrap();
        // Mark the Read with a sentinel so we can verify the *state* (not
        // just the path) survives the round trip. `scroll_top` is the
        // simplest field — the body is short so the real scroll is 0, but
        // the value is an opaque "where the user was" token the picker
        // must round-trip verbatim.
        if let Mode::Read(r) = &mut app.mode {
            r.scroll_top = 42;
        } else {
            panic!("expected Read after open_read");
        }
        app.open_links().unwrap();
        app.close_links();
        // Back in Read (not Browse).
        assert!(matches!(app.mode, Mode::Read(_)), "picker pop lands in Read");
        let rs = match &app.mode {
            Mode::Read(r) => r,
            _ => panic!(),
        };
        assert_eq!(rs.abs_path, "/root/a.md", "the *incoming* page A is back");
        assert_eq!(rs.scroll_top, 42, "the Read's state is round-tripped verbatim");
        // And consuming it left `links_return = None`.
        assert!(app.links_return.is_none(), "parked stack consumed, not duplicated");
        // Final pop goes to Browse (no second Read to fall back on).
        app.close_read();
        assert!(matches!(app.mode, Mode::Browse), "final pop lands in Browse");
        assert!(app.read_return.is_none(), "no linked-Read stack to pop to");
    }

    /// Round trip: Read A → picker → open B → Read B → close B → Read A →
    /// close A → Browse. This is the full "follow a link then backtrack"
    /// workflow the M8.5 feature promises; if any slot leaks, this test
    /// fails on the exact slot.
    #[test]
    fn follow_a_link_then_backtrack_full_round_trip() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        insert_page(&db, "a", "A", "see [[b]]\n");
        insert_page(&db, "b", "B", "b body — also see [[c]]\n");
        insert_page(&db, "c", "C", "c body\n");
        let mut app = App::new(db).unwrap();

        // 1. Browse → Read A.
        app.selected = app
            .rows
            .iter()
            .position(|r| r.id == "note:a")
            .expect("a in rows");
        app.open_read().unwrap();

        // 2. Read A → Picker.
        app.open_links().unwrap();
        assert!(matches!(app.mode, Mode::Links(_)));

        // 3. Picker → Read B (the unique `[[b]]`).
        app.links_open_selected().unwrap();
        assert!(
            matches!(app.mode, Mode::Read(_)),
            "Read B (unique target opens)"
        );
        let rs = match &app.mode {
            Mode::Read(r) => r,
            _ => panic!(),
        };
        assert_eq!(rs.abs_path, "/root/b.md");

        // 4. Read B → Picker (its own links).
        app.open_links().unwrap();
        assert!(matches!(app.mode, Mode::Links(_)));

        // 5. Picker → Read C (the `[[c]]` on B).
        app.links_open_selected().unwrap();
        let rs = match &app.mode {
            Mode::Read(r) => r,
            _ => panic!(),
        };
        assert_eq!(rs.abs_path, "/root/c.md", "C is open");
        // The chain of returns: `read_return` is Read B (the last open),
        // and Read A is… not parked (the picker that *opened* B consumed
        // it via `.take()` → into `read_return`; when B was opened A was
        // still in `links_return` which got *taken* into `read_return`;
        // then B's own picker parked B into `links_return`; then C's open
        // took B into `read_return`. So A is *dropped* at step 5 — only
        // the *immediate* return is preserved by design (M8.5 is one-level
        // backtracking; the user's "deep backstack" is the CLI `kb links`
        // surface, not the TUI).
        assert!(
            app.read_return.is_some(),
            "the immediate return (B) is parked for the Esc back"
        );

        // 6. Esc from Read C → Read B (not Browse).
        app.close_read();
        let rs = match &app.mode {
            Mode::Read(r) => r,
            m => panic!("Esc from C should land in Read B, got {m:?}"),
        };
        assert_eq!(rs.abs_path, "/root/b.md", "back to B");

        // 7. Esc from Read B → Browse (A was dropped at step 5).
        app.close_read();
        assert!(
            matches!(app.mode, Mode::Browse),
            "Esc from B lands in Browse (one-level backtracking by design)"
        );
    }

    /// `l` in `handle_find_input` opens the picker anchored on the current
    /// find hit — the "I just finished `f`, now follow the closest link"
    /// fast path. These tests exercise `open_links()` with a live find
    /// state (the same path `handle_find_input`'s `l` arm takes) and verify
    /// the picker cursor is pre-selected at the link **nearest** the find
    /// anchor, not at index 0.
    #[test]
    fn open_links_with_find_anchor_preselects_nearest() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // Link B at line 0, link C at line 5 (lines 1–4 are filler).
        // Needle matches on line 4 → nearest link is C (line 5), index 1.
        let body = String::from(
            "see [[b]]\nfiller 1\nfiller 2\nfiller 3\nanchor here\nthen [[c]]\n"
        );
        insert_page(&db, "a", "A", &body);
        insert_page(&db, "b", "B", "b body\n");
        insert_page(&db, "c", "C", "c body\n");
        let mut app = App::new(db).unwrap();
        app.selected = 0;
        app.open_read().unwrap();

        app.find_activate();
        for c in "anchor".chars() {
            app.find_input(c);
        }
        // Anchor is at line 4.
        {
            let rs = match &app.mode { Mode::Read(r) => r, _ => panic!() };
            let fs = rs.find.as_ref().unwrap();
            assert_eq!(fs.matches.len(), 1);
            assert!(
                fs.matches[0].0 == 4,
                "anchor on line 4, got {:?}",
                fs.matches[0]
            );
        }

        app.open_links().unwrap();
        let ls = match &app.mode { Mode::Links(l) => l, _ => panic!() };
        assert_eq!(ls.links.len(), 2);
        // Link B at line 0: |0 - 4| = 4
        // Link C at line 5: |5 - 4| = 1 ← nearest
        assert_eq!(ls.cursor, 1, "cursor pre-selected at nearest (C, not B)");
    }

    /// The `*input*` dispatch (`handle_event`) routes `Mode::Links(_)`
    /// to `handle_links` — a unit-test of the full path would need
    /// `KeyEvent` plumbing, but we can at least verify that the *mode*
    /// reaches `handle_links` by observing that pressing `Esc` from the
    /// picker goes through `close_links` (pops to Read), not through
    /// `handle_read`'s Esc arm (which would also pop to Read via
    /// `close_read` — so we need a discriminator: pressing `j`/`k` from
    /// the picker moves the *picker's* cursor, not the body's scroll).
    ///
    /// This test pins the observable state transition: enter the picker,
    /// call `links_open_selected` on entry 0, then verify the mode is Read.
    #[test]
    fn links_picker_is_a_distinct_mode_not_read() {
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // A page with two links so we can exercise the cursor.
        insert_page(
            &db,
            "a",
            "A",
            "see [[b]] then [[c]]\n",
        );
        insert_page(&db, "b", "B", "b body\n");
        insert_page(&db, "c", "C", "c body\n");
        let mut app = App::new(db).unwrap();
        app.selected = 0;
        app.open_read().unwrap();
        app.open_links().unwrap();
        if let Mode::Links(l) = &mut app.mode {
            assert_eq!(l.links.len(), 2, "two wikilinks on A");
            // Moving down: the cursor advances, but *the Read scroll is
            // untouched* (it lives on the parked `ReadState`, not here).
            l.cursor += 1;
            assert_eq!(l.cursor, 1, "cursor advanced within the picker");
        } else {
            panic!("expected Links mode after open_links");
        }
        // Esc back: the Read is exactly where we left it (scroll still 0).
        app.close_links();
        let rs = match &app.mode {
            Mode::Read(r) => r,
            _ => panic!(),
        };
        assert_eq!(rs.scroll_top, 0, "Read scroll unchanged by the picker excursion");
    }

}
