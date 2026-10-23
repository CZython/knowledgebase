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
    /// The status-bar text (last event message).
    pub status: Status,
    /// The active color palette (P2). **`Copy`** and read by `render.rs` for
    /// every `Style`. Default-constructed (the pinned `default_palette`) when
    /// `kb tui` runs with no `--palette`; set by `with_palette` after a
    /// `--palette <name|path>` resolves.
    pub palette: palette::Palette,
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
            Status::info("↑↓/jk move · / filter · e edit · a alias · r roots · Enter open · v $EDITOR · q quit")
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
            palette: palette::Palette::default_palette(),
        })
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
            if let Ok((body, outline)) = load_body(&self.db, &path.to_string_lossy()) {
                r.body = body;
                r.outline = outline;
                r.scroll_top = 0;
            }
        }
        // Also re-snapshot the row's file bytes (in Browse mode the row's
        // `abs_path` is already what we'd read next — no state to update).
        self.set_success(format!("opened {} — Esc back (the file is now source of truth; re-index with `kb refresh`)", row.abs_path));
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
pub fn run_tui(dbpath: Option<&Path>, palette_spec: Option<&str>, list_palettes: bool) -> i32 {
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
    let palette = match palette_spec {
        Some(spec) => match palette::Palette::load(spec) {
            Ok(p) => p,
            Err(msg) => {
                eprintln!("{msg}");
                return crate::EXIT_ERR;
            }
        },
        None => palette::Palette::default_palette(),
    };
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
    println!("    kb/tui/palettes/{{default,colorblind,mono}}.toml");
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

    // ---- Redesigned edit flow (zsh-style three-stage) --------------------

    /// A small helper: seed an on-disk page + DB row + the derived
    /// `page_tags` rows, return an `App` with that page selected. Mirrors the
    /// fixture in `close_edit_...` but returns the temp dir so the caller can
    /// clean it up. Seeding `page_tags` is what gives the `Tags` field a
    /// non-empty *corpus* (the suggestions come from the DB, not the file).
    fn seeded_app() -> (App, std::path::PathBuf) {
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
        (App::new(db).unwrap(), path)
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
}
