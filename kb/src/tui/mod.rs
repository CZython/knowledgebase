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
use kb_core::frontmatter as fm;
use kb_core::section as section_core;

use crate::EXIT_ERR;
use crate::EXIT_OK;

pub mod input;
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
    /// `Tab` toggles.
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
    /// Manage `tag_aliases`: list, add (upsert), remove. M8.3.
    Alias(AliasState),
    /// Manage registered roots: list, add, drop. M8.3.
    Roots(RootsState),
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
    /// The first body line to render (scroll offset, total on any body).
    /// 0 = top; values beyond the last line are no-ops (clamped by the
    /// scroll mutators, never the render pass).
    pub scroll_top: usize,
}

/// A field the Edit mode can show — either a *scalar* (free text) or a
/// *list* (the value is the next item to append). Both are edited over the
/// already-verified `kb-core::frontmatter` ops; the TUI just composes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    Title,
    Description,
    Tags,
    SearchTerms,
}

impl EditField {
    /// The key the verified frontmatter ops address this field by.
    pub fn key(self) -> &'static str {
        match self {
            EditField::Title => "title",
            EditField::Description => "description",
            EditField::Tags => "tags",
            EditField::SearchTerms => "search_terms",
        }
    }
    /// A short, human label for the render surface (the "title:" / "tags:"
    /// caption in the Edit form).
    pub fn label(self) -> &'static str {
        match self {
            EditField::Title => "title",
            EditField::Description => "description",
            EditField::Tags => "tags",
            EditField::SearchTerms => "search_terms",
        }
    }
    /// Whether the field's value is a comma/list-style *item* (so the
    /// "append an item" semantic + tab-autocomplete applies) vs. a scalar
    /// (so "set the value" applies). Title/description are scalars;
    /// tags/search_terms are lists.
    pub fn is_list(self) -> bool {
        matches!(self, EditField::Tags | EditField::SearchTerms)
    }
    /// The ordered field list the Edit mode cycles through. Total + stable.
    pub fn all() -> &'static [EditField] {
        &[EditField::Title, EditField::Description, EditField::Tags, EditField::SearchTerms]
    }
    /// The next field in the cycle (down), or the first (wrap). Total.
    pub fn next(self) -> EditField {
        let all = Self::all();
        let i = all.iter().position(|f| f == &self).expect("EditField is in its own list");
        all[(i + 1) % all.len()]
    }
    /// The previous field in the cycle (up), or the last (wrap). Total.
    pub fn prev(self) -> EditField {
        let all = Self::all();
        let i = all.iter().position(|f| f == &self).expect("EditField is in its own list");
        all[(i + all.len() - 1) % all.len()]
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
    /// tags/search_terms from the corpus").
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
    /// (the list we saw when we opened this page). Used by `plan()` to
    /// enumerate the `RemoveItem` ops for any tail items removed via `x`.
    /// Empty for scalar fields.
    pub entry_list: Vec<String>,
    /// The items on the active **list** field **in the working state**
    /// (after any `x` removals). This is the "what's there now" the user
    /// sees, and the dedup baseline for `AppendItem` ops. Empty for
    /// scalar fields.
    pub current_list: Vec<String>,
    /// The file bytes **in the working state** (updated by `remove_tail_item`).
    /// `commit_edit` applies ops to this, not to `entry_bytes`, so that
    /// tail-removals are already baked in and `AppendItem` only needs to
    /// add the new tokens. For scalar fields these are always equal to
    /// `entry_bytes` (scalars don't do tail removal).
    pub working_bytes: Vec<u8>,
}

impl EditState {
    /// The pure "what will commit apply from this exact state?" — the
    /// *same* `build_ops` helper the render surfaces as the "pending"
    /// footer, so the display + commit paths cannot diverge. For a scalar
    /// field the no-op rule compares the buffer against the field's value
    /// *on entry* (the `build_ops` "original"); for a list field the
    /// dedup rule compares against the *current* list (after any `x`
    /// removals tracked in `current_list`). Total.
    pub fn plan(&self) -> Vec<EditOp> {
        if self.field.is_list() {
            build_ops(self.field, &self.value, "", &self.current_list)
        } else {
            let original = fm::scalar_value(self.entry_bytes.as_slice(), self.field.key())
                .unwrap_or_default();
            build_ops(self.field, &self.value, &original, &[])
        }
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
    /// A single list-item removal (`fm::remove_list_item`) — the tail-removal
    /// is applied live by `remove_tail_item`; this variant exists for the
    /// pure `apply_op` surface + test coverage.
    #[allow(dead_code)]
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
    /// The status-bar text (last event message).
    pub status: Status,
}

/// A `Result` over the `(message, exit-code)` tuple the rest of `kb`
/// uses, so a failed TUI maps to the same stderr + exit-code contract.
pub type KbResult<T> = Result<T, (String, i32)>;

impl App {
    /// Load the DB snapshot into the TUI state.
    pub fn new(db: Db) -> KbResult<Self> {
        let rows = list_pages(&db)?;
        let status = if rows.is_empty() {
            Status::info("kb tui — no pages indexed yet (run `kb add <root>` then `kb tui` again)")
        } else {
            Status::info("kb tui · ↑↓/jk move · / filter · e edit · a alias · r roots · Enter open · q quit")
        };
        Ok(App {
            view: rows.clone(),
            db,
            rows,
            filter_text: String::new(),
            filter_active: false,
            selected: 0,
            mode: Mode::Browse,
            status,
        })
    }

    /// Recompute `view` (the filtered row list) from `filter_text` and
    /// clamp `selected` into the new bounds. Total: on an empty filter the
    /// view is the full `rows`; on a non-empty filter it is the (possibly
    /// empty) matching subset, and `selected` never points out of range.
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

    /// Scroll the Read body by `delta` lines (total: clamped to
    /// `[0, max_top]`, where `max_top` is the last first-visible line).
    pub fn scroll_body(&mut self, delta: isize) {
        if let Mode::Read(r) = &mut self.mode {
            let max_top = scroll_max_top(&r.body);
            let n = r.scroll_top as isize + delta;
            r.scroll_top = n.clamp(0, max_top as isize) as usize;
        }
    }

    /// Scroll the Read body to the top.
    pub fn scroll_body_top(&mut self) {
        if let Mode::Read(r) = &mut self.mode {
            r.scroll_top = 0;
        }
    }

    /// Scroll the Read body to the bottom (the last line visible).
    pub fn scroll_body_bottom(&mut self) {
        if let Mode::Read(r) = &mut self.mode {
            r.scroll_top = scroll_max_top(&r.body);
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
        let (body, outline) = load_body(&self.db, &row.abs_path)?;
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
        };
        self.mode = Mode::Read(rs);
        Ok(())
    }

    /// Go back from Read to Browse (keeps `view` / `selected` intact — the
    /// selection is preserved across the round trip, so `Esc` in Read then
    /// `j` continues where the user left off).
    pub fn close_read(&mut self) {
        self.mode = Mode::Browse;
    }

    // ===================================================================
    //  M8.2 — Edit mode
    //  ===================================================================

    /// Open the selected page into Edit mode. The file bytes (from disk)
    /// are loaded and cached as `entry_bytes` (the disk guard + baseline for
    /// the no-op rule), and the field's *current* values are read so the
    /// user edits in place (scalars) and sees what's there (lists).
    /// **Total**: an open on an empty `view` is a no-op.
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
        let corpus = corpus_for(&self.db, field);
        let value = if field.is_list() {
            String::new()
        } else {
            fm::scalar_value(bytes.as_slice(), field.key()).unwrap_or_default()
        };
        let current_list = if field.is_list() {
            fm::list_values(bytes.as_slice(), field.key()).unwrap_or_default()
        } else {
            Vec::new()
        };
        // Tab-autocomplete: on a list field the suggestions mirror the
        // corpus (empty buffer → the full corpus, the "available tokens"
        // hint). On a scalar there is no autocomplete (free text).
        let suggestions = if field.is_list() {
            suggestions_for(&corpus, "")
        } else {
            Vec::new()
        };
        let entry_list = current_list.clone();
        let working_bytes = bytes.clone();
        self.mode = Mode::Edit(EditState {
            field,
            abs_path: row.abs_path,
            id: row.id,
            value,
            suggestions,
            corpus,
            entry_bytes: bytes,
            entry_list,
            current_list,
            working_bytes,
        });
        self.set_info("edit — ↑/j field · type value · Tab autocomplete (lists) · Enter commit · Esc back");
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
            self.set_info(Self::edit_field_note(field));
        }
    }

    /// Append a single character to the value buffer (the typing path).
    /// Recomputes the live autocomplete when the field is a list; no-ops on
    /// a scalar (the buffer is free text, no suggestions).
    pub fn edit_type(&mut self, c: char) {
        if let Mode::Edit(e) = &mut self.mode {
            e.value.push(c);
            if e.field.is_list() {
                e.suggestions = suggestions_for(&e.corpus, &e.value);
            }
        }
    }

    /// Pop the last character from the value buffer (the Backspace path).
    /// Recomputes the live autocomplete the same way as `edit_type`.
    pub fn edit_backspace(&mut self) {
        if let Mode::Edit(e) = &mut self.mode {
            let popped = e.value.pop();
            if e.field.is_list() {
                e.suggestions = suggestions_for(&e.corpus, &e.value);
            }
            let _ = popped;
        }
    }

    /// Cycle to the next field (down) or previous (up). Wraps at the edge.
    pub fn edit_move_field(&mut self, down: bool) {
        if let Mode::Edit(e) = &self.mode {
            let f = if down { e.field.next() } else { e.field.prev() };
            self.cycle_field(f);
        }
    }

    /// The human note for a field switch (the status bar after `cycle_field`).
    fn edit_field_note(field: EditField) -> String {
        format!("editing {} — type a value · Enter commit · Esc back", field.label())
    }

    /// Drop the last item from the active **list** field (the `x` key — a
    /// "delete tail" semantic, like Backspace on the last item of a list
    /// editor). No-op on a scalar field or an empty list. The removal
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
            self.set_info("x — not on a list field (arrow to tags / search_terms)");
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
        // any tail-removals from `x` — they are "baked in"). `apply_op`
        // never mutates its input, so a mid-way failure leaves the disk
        // untouched (the write below is the only thing that changes bytes).
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
        if acc != working_snap {
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
        let rec = match kb_db::index::build_page(
            std::path::Path::new(&abs_path),
            "",
            "",
            false,
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
        }
        Ok(())
    }

    /// Re-run `list_pages` + `apply_filter` (a "dirty" refresh after an
    /// edit commits). The selection index is re-clamped to the new view
    /// (total: a selection in-range before can go out-of-range after a
    /// filter narrows the list; `apply_filter` handles the clamp).
    pub fn reload_rows(&mut self) {
        if let Ok(rows) = list_pages(&self.db) {
            self.rows = rows;
            self.apply_filter();
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

    /// Open the **Roots** manager: load the current `roots` snapshot and arm
    /// the *add form*. **Total**: always succeeds.
    pub fn open_roots(&mut self) -> KbResult<()> {
        let list = list_roots(&self.db)?;
        self.mode = Mode::Roots(RootsState {
            list,
            input: String::new(),
            selected: 0,
            focused: false,
        });
        self.set_info("roots — Tab list · type an absolute path then Enter add · x drop · Esc back");
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

/// The highest legitimate `scroll_top` for a body — such that the *last*
/// line can be the first visible one. We cap at `lines - 1` (not `lines`)
/// so every reachable scroll position renders a non-empty window. The value
/// is a pure function of the body (no viewport math in here), which keeps
/// the scroll mutators total + trivially testable. Degenerate: an empty body
/// caps at 0.
pub fn scroll_max_top(body: &str) -> usize {
    // `lines()` splits on `\n` and drops a single trailing newline, so
    // "one\n" → 1 line, "one\ntwo" → 2 lines, "" → 0 lines.
    body.lines().count().max(1) - 1
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
    original: &str,
    existing: &[String],
) -> Vec<EditOp> {
    let key = field.key();
    let v = value.trim();
    if !field.is_list() {
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
        return Vec::new();
    }
    v.split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| {
            !t.is_empty() && !existing.iter().any(|e| e == t)
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
/// all search-terms on `page_gates` for `EditField::SearchTerms`). For a
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
        EditField::SearchTerms => {
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
    let out = stmt
        .query_map([], |r| {
            Ok(PageRow {
                abs_path: r.get::<_, String>(0)?,
                id: r.get::<_, String>(1)?,
                pt: r.get::<_, String>(2)?,
                title: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                root: r.get::<_, String>(4)?,
                rel_path: r.get::<_, String>(5)?,
            })
        })
        .map_err(|e| (format!("query: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("rows: {e}"), EXIT_ERR))?;
    Ok(out)
}

/// A page outline: a list of `(byte_offset, level, title)` entries, in document
/// order. (Named to keep multi-use signatures readable without inlining the
/// tuple.)
type Outline = Vec<(usize, u8, String)>;

/// Load a page's body text (from the `pages.body` column) + its outline
/// (a `heading_of` extraction over the body). Called by `App::open_read`.
/// Total: an empty/NULL body renders as the empty string + an empty outline.
/// The page is looked up by *absolute path* (the stable identity), so a
/// `full-rebuild` (which reassigns SQLite rowids) cannot open the wrong page.
fn load_body(db: &Db, abs_path: &str) -> KbResult<(String, Outline)> {
    let body: String = db
        .conn
        .query_row(
            "SELECT COALESCE(body, '') FROM pages WHERE abs_path = ?1",
            [abs_path],
            |r| r.get(0),
        )
        .map_err(|e| (format!("body lookup for {abs_path}: {e}"), EXIT_ERR))?;
    let outline = outline_of(&body);
    Ok((body, outline))
}
/// Run the TUI: open the DB, load the snapshot, hand off to the event
/// loop, and restore the terminal on the way out (the `Drop` guard in
/// `event_loop` is the safety net for the in-between returns).
pub fn run_tui(dbpath: Option<&Path>) -> i32 {
    let db = match crate::open_db(dbpath) {
        Ok(d) => d,
        Err((msg, code)) => {
            eprintln!("{msg}");
            return code;
        }
    };
    let app = match App::new(db) {
        Ok(a) => a,
        Err((msg, code)) => {
            eprintln!("{msg}");
            return code;
        }
    };
    event_loop(app)
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
        if let Err(e) = terminal.draw(|f| render::draw(f, &app)) {
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
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_terms, mtime_ns, content_hash)
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
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_terms,
                                    body, frontmatter_text, mtime_ns, content_hash)
                 VALUES (?1, ?2, 'note', ?3, '/root', ?4, NULL, ?5, '---\ntype: note\n---\n', 0, 'x')",
                rusqlite::params![abs, id, title, format!("{name}.md"), body],
            )
            .unwrap();
        abs
    }

    /// `scroll_max_top` is a pure function of the body (total, no IO): it
    /// caps at `lines - 1` so every reachable scroll position renders a
    /// non-empty window. Empty body → 0.
    #[test]
    fn scroll_max_top_is_a_pure_function_of_the_body() {
        assert_eq!(scroll_max_top(""), 0);
        assert_eq!(scroll_max_top("one\n"), 0); // 1 line → max top 0
        assert_eq!(scroll_max_top("one\ntwo"), 1); // 2 lines → max top 1
        assert_eq!(scroll_max_top("one\ntwo\n"), 1);
        assert_eq!(scroll_max_top("# H\nbody line\ntrailer"), 2);
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

        // scroll down past the bottom: clamps to max, does not grow.
        app.scroll_body(100);
        let max = scroll_max_top(&match &app.mode { Mode::Read(r) => r.body.clone(), _ => panic!() });
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
        let max = scroll_max_top("a\nb\nc\nd\ne\n");
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
    /// nothing to clear), and an *absent* `search_terms`. The bytes are
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
        // Unchanged → no-op.
        let ops = build_ops(EditField::Title, "Hello World", "Hello World", &[]);
        assert!(ops.is_empty(), "unchanged scalar is a no-op");

        // Changed → one SetScalar.
        let ops = build_ops(EditField::Title, "New", "Hello World", &[]);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            ops[0],
            EditOp::SetScalar { key, ref value } if key == "title" && value == "New"
        ));

        // Cleared from non-empty → one RemoveField.
        let ops = build_ops(EditField::Title, "", "Hello World", &[]);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            ops[0],
            EditOp::RemoveField { key } if key == "title"
        ));

        // Cleared from empty → no-op.
        let ops = build_ops(EditField::Title, "   ", "", &[]);
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
        // Empty buffer → no-op.
        let ops = build_ops(EditField::Tags, "", "", &["mTLS".into()]);
        assert!(ops.is_empty(), "empty buffer on a list is a no-op");

        // One new token, not in list → one AppendItem.
        let ops = build_ops(EditField::Tags, "llm", "", &["mTLS".into()]);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            ops[0],
            EditOp::AppendItem { key, ref item } if key == "tags" && item == "llm"
        ));

        // Multiple tokens, some new, some present → only the new ones,
        // order preserved. (Existing = ["mTLS", "transformer"].)
        let existing = vec!["mTLS".into(), "transformer".into()];
        let ops = build_ops(EditField::Tags, "mTLS, llm, transformer, token-bucket", "", &existing);
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
        let ops = build_ops(EditField::Tags, "  llm  ", "", &[]);
        assert_eq!(ops.len(), 1);
        if let EditOp::AppendItem { ref item, key } = ops[0] {
            assert_eq!(key, "tags");
            assert_eq!(item, "llm");
        } else {
            panic!("expected AppendItem");
        }

        // All tokens in list → no AppendItem (empty buffer effect).
        let ops = build_ops(EditField::Tags, "mTLS, transformer", "", &["mTLS".into(), "transformer".into()]);
        assert!(ops.is_empty(), "all tokens already in the list → no-op");

        // Empty tokens (whitespace-only segments) are dropped.
        let ops = build_ops(EditField::Tags, "  , ,  ", "", &[]);
        assert!(ops.is_empty(), "whitespace-only tokens are dropped");
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
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_terms,
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
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_terms,
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
}
