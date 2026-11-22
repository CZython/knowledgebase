//! Top-level application state for `kb-gui`.
//!
//! `AppState` is owned by the eframe `App` (the single egui thread). It
//! reflects what the UI should paint right now: the current phase (loading /
//! ready / error), the live corpus/page/search data as it arrives from the DB
//! worker, the active theme, and the launch routing derived from argv.
//!
//! G1 adds the navigation model on top of G0's phase machine: the rail's
//! collection selection (with its search scope), the note-list grouping and
//! filter, and which of the launch states — Welcome / Home / browse — is live.
//!
//! The state NEVER touches the database directly — it only holds data the
//! worker emitted (PLAN-GUI §3.2: the worker thread is the only thread that
//! does disk/SQL). This module is also the home of the `StartupReport` first-
//! frame budget helper (PLAN-GUI §13: the 500 ms budget is a testable value,
//! not prose).

use std::time::Duration;

/// The hard first-frame budget (PLAN-GUI §3.4): from process spawn to the
/// first painted chrome with empty/skeleton content. Expressed in ms so the
/// budget check stays an integer comparison, no float drift.
pub const FIRST_FRAME_BUDGET_MS: u64 = 500;

/// A measured first-frame time, judged against [`FIRST_FRAME_BUDGET_MS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupReport {
    /// Elapsed spawn-to-first-paint in whole milliseconds.
    pub total: Duration,
}

impl StartupReport {
    /// Record a first-frame time in milliseconds.
    pub fn new(ms: u64) -> Self {
        Self {
            total: Duration::from_millis(ms),
        }
    }

    /// Whole-milliseconds view of the recorded time.
    pub fn ms(&self) -> u64 {
        self.total.as_millis() as u64
    }

    /// `true` when the recorded time is strictly inside the budget.
    pub fn ok(&self) -> bool {
        self.ms() < FIRST_FRAME_BUDGET_MS
    }

    /// `true` when the recorded time breaches the budget.
    #[allow(dead_code)] // exercised by the startup test (G0 acceptance) before G6 wires the UI verdict
    pub fn breached(&self) -> bool {
        !self.ok()
    }

    /// A one-line human report, for `--startup-report` and the startup test.
    pub fn line(&self) -> String {
        if self.ok() {
            format!(
                "first frame {} ms — within budget ({} ms)",
                self.ms(),
                FIRST_FRAME_BUDGET_MS
            )
        } else {
            format!(
                "first frame {} ms — BREACHED budget ({} ms)",
                self.ms(),
                FIRST_FRAME_BUDGET_MS
            )
        }
    }
}

/// Which theme the app is running under. Built-ins are compiled in; `System`
/// defers to the OS appearance (G6 wires live switching; G1 carries a cached
/// value so frame 1 is never blocked on a system query — PLAN-GUI §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    Dark,
    Light,
    System,
}

/// Launch routing derived from argv (PLAN-GUI §3.4 / G0 "launch routing
/// skeleton"). The shell always paints first; this only says *where to go*
/// once the worker is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchRoute {
    /// No path/ref was given: show Home (or Welcome if there are no roots).
    Home,
    /// `kb-gui <file>`: open that page once the corpus is loaded.
    OpenFile(String),
    /// `kb-gui <file>#Section`: open the page and jump to the heading.
    OpenSection { file: String, section: String },
}

impl LaunchRoute {
    /// `true` when the app should land on Home (no file was requested).
    pub fn is_home(&self) -> bool {
        matches!(self, LaunchRoute::Home)
    }
}

/// A single page in the corpus: the `pages` row plus the collection
/// membership and tag rows the GUI needs to render list rows and rail
/// counts without extra round-trips (PLAN-GUI §2.1: the membership is a
/// many-to-many derived from `page_gates`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRow {
    pub abs_path: String,
    /// Display id (`type:slug`).
    pub id: String,
    pub title: Option<String>,
    pub type_: String,
    pub rel_path: String,
    pub root: String,
    /// Gate terms (`page_gates.term`, sorted) — the page's collection
    /// membership. Empty for an un-gated page.
    pub gates: Vec<String>,
    /// Tags (`page_tags.tag`, sorted) — rendered as chips on list rows.
    pub tags: Vec<String>,
    /// Stable 16-hex identity token (may be empty pre-backfill).
    pub uuid: String,
    /// ISO-8601 update time (for Home's "Recent" list; may be empty).
    pub updated_at: String,
}

impl PageRow {
    /// `true` when the page belongs to at least one collection.
    pub fn gated(&self) -> bool {
        !self.gates.is_empty()
    }
}

/// A collection (a `page_gates` term) or one of the two pseudo-collections.
/// The pseudo-collections are pinned tiles (PLAN-GUI §6.3): `All` is browse
/// parity, `Gated`/`Ungated` are the visibility split.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CollectionKey {
    /// All indexed pages — browse parity, never filtered. The default
    /// selection; it also clears the search scope (PLAN-GUI §2.1 rule 1).
    All,
    /// Pages that carry at least one gate term.
    Gated,
    /// Pages that carry no gate term.
    Ungated,
    /// A specific gate term (case-insensitively matched; the label is the
    /// corpus's canonical spelling).
    Term(String),
}

impl CollectionKey {
    /// The tile's visible label.
    #[allow(dead_code)] // exercised by the rail tile (G1 UI — ui::rail)
    pub fn label(&self) -> String {
        match self {
            Self::All => "All pages".into(),
            Self::Gated => "Gated".into(),
            Self::Ungated => "Ungated".into(),
            Self::Term(t) => t.clone(),
        }
    }

    /// `true` when the key is a real collection (not All). Used to decide
    /// whether the selection implies a search scope (PLAN-GUI §2.1 rule 3:
    /// selecting a collection *scopes* search).
    #[allow(dead_code)] // consumed by the search-scope derivation (G1 ui::app::run_search) when a non-Term selection should scope
    pub fn scoped(&self) -> bool {
        !matches!(self, Self::All)
    }
}

/// How the note list arranges rows (PLAN-GUI §6.4: `None · Collection ·
/// Visibility`). `Collection` duplicates a multi-gate page under every gate
/// it belongs to — rows repeat by design — and keeps gateless pages under
/// `Ungated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupBy {
    /// A flat row per page (default in `All pages` — browse parity).
    #[default]
    None,
    /// One section per collection; a page appears under every gate it has.
    Collection,
    /// Two sections: `Open` (un-gated) and `Gated`.
    Visibility,
}

/// The search-results state (the `Cmd+K` overlay; PLAN-GUI §6.8). The overlay
/// keeps its state when navigating into a page and back: the worker only
/// refills the rows when a new query lands.
#[derive(Debug, Clone, Default)]
pub struct SearchView {
    /// The query as last sent to the worker.
    pub query: String,
    /// The scope as last sent (`None` = unscoped, strict CLI gate rules).
    pub scope: Option<String>,
    pub rows: Vec<SearchHit>,
    /// Set when a tag filter matched no corpus tag (the CLI's specific
    /// message, rendered verbatim).
    pub missing_tag: Option<String>,
    /// `true` from the frame the overlay was opened to the frame it was
    /// closed; the last result is kept for the "walk back into results" case.
    pub open: bool,
    /// Keyboard-cursor row inside `rows` (for `↑`/`↓` + `Enter`).
    pub cursor: usize,
}

/// One search result (PLAN-GUI §6.8 — same columns as `kb search --json`).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub abs_path: String,
    pub id: String,
    pub type_: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub rel_path: String,
    pub gates: String,
    pub uuid: String,
    pub uuid_short: String,
    pub score: Option<f64>,
}

/// Overall UI phase. The worker's events drive this; the UI only reads it.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    /// Shell is up, worker is opening the DB / loading the corpus.
    Loading,
    /// Corpus is loaded; the note list and search are live.
    Ready,
    /// A fatal startup error (bad `--db`, migration failure, …).
    Error(String),
}

/// The open-document state (PLAN-GUI §6.5 editor host). In G1 the body is
/// read-only (Formatted renderer + a Raw read view); editing lands in G2.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenPageState {
    pub abs_path: String,
    /// The loaded body; `None` while the worker is fetching it.
    pub body: Option<String>,
    /// A load error (bad path, read failure). Rendered in the host.
    pub error: Option<String>,
    /// The byte baseline captured when the body was opened (`expected_disk`
    /// for the `Cmd::Save` conflict guard, PLAN-GUI §3.3). `None` until the
    /// body has actually loaded.
    pub baseline: Option<Vec<u8>>,
    /// The document kind (PLAN-GUI §2 / §6.15), recomputed in the `Evt::Page`
    /// fold from the loaded body + corpus membership. `GenericMarkdown` while
    /// the body is still `None` (the banner appears once it classifies).
    pub kind: DocumentKind,
    /// `true` once the user dismissed the conversion/add-root banner for the
    /// session (PLAN-GUI §6.15 rule 6: "Dismiss hides the banner for the
    /// session"; the action remains in the `⋯` menu / Settings so dismissal is
    /// not permanent). Reset to `false` on a fresh load / page switch.
    pub banner_dismissed: bool,
}

/// The frontmatter inspector's working state (PLAN-GUI §6.9). Holds the
/// *draft* values the user is editing, keyed off the open page's *on-disk*
/// values captured when the panel seeded (the baseline). The UI computes the
/// dirty op-set by diffing draft vs. baseline; `Commit` sends it to the
/// worker, `Revert` resets the drafts to the baseline.
///
/// The panel is `open`-aware: a `Cmd/Ctrl+Shift+F` or `Properties` button
/// toggle flips `open` (and seeds `baseline` from the open page's parsed
/// frontmatter on first open, so a `Revert` has a real target).
#[derive(Debug, Clone, Default)]
pub struct InspectorState {
    /// Panel visible (the §6.9 toggle).
    pub open: bool,
    /// The absolute path the drafts correspond to (`None` when no page open
    /// or the drafts are stale — the panel renders a placeholder).
    pub abs_path: Option<String>,
    /// The on-disk (baseline) raw scalar values, captured at seed time.
    pub baseline_type: Option<String>,
    pub baseline_title: Option<String>,
    pub baseline_description: Option<String>,
    pub baseline_language: Option<String>,
    pub baseline_tags: Vec<String>,
    pub baseline_collections: Vec<String>,
    /// The live draft values the panel paints (seeded from the baseline).
    pub draft_type: String,
    pub draft_title: String,
    pub draft_description: String,
    pub draft_language: String,
    pub draft_tags: Vec<String>,
    pub draft_tags_input: String,
    pub draft_collections: Vec<String>,
    pub draft_collections_input: String,
    /// The field order for Tab / Shift+Tab traversal (PLAN-GUI §6.9):
    /// `type, title, description, language, tags, collections` (indices 0..=5).
    pub focus: InspectorField,
    /// Set when a `SaveFrontmatter` is in flight (the panel disables Commit /
    /// reflects "saving…" in the panel header).
    pub saving: bool,
}

/// A focusable field in the inspector (the Tab traversal order — §6.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InspectorField {
    #[default]
    Type,
    Title,
    Description,
    Language,
    Tags,
    Collections,
}

impl InspectorField {
    /// The next field in the Tab order (wraps to `Type`).
    pub fn next(&self) -> Self {
        match self {
            Self::Type => Self::Title,
            Self::Title => Self::Description,
            Self::Description => Self::Language,
            Self::Language => Self::Tags,
            Self::Tags => Self::Collections,
            Self::Collections => Self::Type,
        }
    }
    /// The previous field in the Shift+Tab order (wraps to `Collections`).
    pub fn prev(&self) -> Self {
        match self {
            Self::Type => Self::Collections,
            Self::Title => Self::Type,
            Self::Description => Self::Title,
            Self::Language => Self::Description,
            Self::Tags => Self::Language,
            Self::Collections => Self::Tags,
        }
    }
}

/// The document kind of the *open* page (PLAN-GUI §2 / §6.15, lines 162–182).
///
/// A property of the open document, not the file format. It decides which
/// `kb` features are enabled and whether the conversion banner shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    /// Valid `type:` *and* indexed in the corpus (`kb browse` row present).
    /// Full `kb` features; no banner.
    KbPage,
    /// Valid `type:` but outside every registered root / not yet in the
    /// corpus. Full editing; search/links/collections disabled until indexed.
    /// Gets the “Not indexed — add the containing folder as a root?” banner.
    UnindexedMarkdown,
    /// Missing or unknown `type:` (or unparseable frontmatter). `kb`
    /// features disabled; the conversion banner (“Would you like to convert
    /// this markdown file into a valid page?”) is shown.
    GenericMarkdown,
}

impl DocumentKind {
    /// `true` when the document is a full `kb` page (no banner, features on).
    pub fn is_kb_page(&self) -> bool {
        matches!(self, Self::KbPage)
    }
}

/// Classify a loaded page body (PLAN-GUI §162–182, §727–749).
///
/// `GenericMarkdown` when `frontmatter::parse` yields no known `type:`
/// (missing, unknown, or unparseable). Otherwise the page has a *valid*
/// `type:`: if it is indexed in the corpus it is a `KbPage`, else an
/// `UnindexedMarkdown` (valid type, but outside every root / not in `pages`).
///
/// Pure and cheap enough to run on the UI thread in the `Evt::Page` fold.
pub fn classify(body: &str, indexed: bool) -> DocumentKind {
    let fm = kb_core::frontmatter::parse(body);
    if !fm.is_indexable() {
        DocumentKind::GenericMarkdown
    } else if indexed {
        DocumentKind::KbPage
    } else {
        DocumentKind::UnindexedMarkdown
    }
}

/// Which body the open document shows (PLAN-GUI §6.2 top-bar toggle,
/// `Cmd/Ctrl+E`). G1 renders both read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// Read-only formatted render via pulldown-cmark (default).
    #[default]
    Formatted,
    /// Raw source, monospace (read-only in G1; the editor is G2).
    Raw,
}

/// Everything the `App` paints. Plain data + the derived view state; no
/// channels, no handles (those live in `data::WorkerHandle`).
#[derive(Debug)]
pub struct AppState {
    pub phase: Phase,
    /// The corpus (every indexed page, `kb browse` parity including gated
    /// pages), with membership + tags attached.
    pub pages: Vec<PageRow>,
    /// Derived from `pages`: the rail's collection tiles (Term keys in
    /// first-appearance order, already case-deduped) and their counts.
    /// `Gated`/`Ungated` counts are recomputed by the UI from `pages`.
    pub collections: Vec<(String, usize)>,
    /// The two visibility pseudo-collection counts (computed by the worker
    /// so the rail and list can never disagree).
    pub gated_count: usize,
    pub ungated_count: usize,
    /// Distinct non-empty `language:` values, for the inspector autocomplete.
    pub distinct_languages: Vec<String>,
    /// At least one root is registered (drives the Welcome vs Home split —
    /// PLAN-GUI §6.13/§6.14).
    pub roots_known: bool,
    pub launch: LaunchRoute,
    /// Read for theme application at startup; G6 repaints on live switches.
    #[allow(dead_code)] // read by the G6 live theme-switch pass (PLAN-GUI §11)
    pub theme: ThemeChoice,
    /// The rail's active tile. A non-`All` selection filters the list and
    /// scopes search (PLAN-GUI §2.1 rule 3).
    pub collection: CollectionKey,
    /// The note-list arrangement (default `None` — browse parity; PLAN-GUI
    /// §6.4).
    pub group_by: GroupBy,
    /// The inline filter in the note-list header (case-insensitive substring
    /// over title/path; empty = no filter).
    pub list_filter: String,
    /// The list's selected row (by abs_path; `None` = nothing selected /
    /// nothing open yet). The G1 preview host keys off this.
    pub selected_page: Option<String>,
    /// The open document (PLAN-GUI §6.5) — the body + error; `None` when no
    /// page is open (Home/Welcome shows instead).
    pub open_page: Option<OpenPageState>,
    /// The inline "new note" form is shown (G2; PLAN-GUI §6.5).
    pub create_open: bool,
    /// The new-note form's title input (local to the form; not persisted).
    pub create_title: String,
    /// The new-note form's type input (`wiki` / `note` / `label`).
    pub create_type: String,
    /// The "convert to page" dialog is shown (G3; PLAN-GUI §6.15 "Convert
    /// flow"). Seeded from the open document's frontmatter on open.
    pub convert_open: bool,
    /// The conversion dialog's `type:` input (default `note`, §6.15 step 1).
    pub convert_type: String,
    /// The conversion dialog's `title:` input (prefilled from H1/filename).
    pub convert_title: String,
    /// The conversion dialog's free-form tags (optional, §6.15 step 1).
    pub convert_tags: Vec<String>,
    /// The conversion dialog's free-form collection terms (optional).
    pub convert_collections: Vec<String>,
    /// Formatted vs Raw body (the top-bar toggle, `Cmd/Ctrl+E`).
    pub view_mode: ViewMode,
    /// The `Cmd+K` overlay's results + query + scope. Kept across close/open
    /// so navigating back into results does not refetch.
    pub search: SearchView,
    /// The open-document save state (PLAN-GUI §6.5 footer). `Saved` (default)
    /// = clean; `Unsaved` = dirty; `Saving` = in flight; `Conflict` = the file
    /// changed on disk under us and a Reload / Keep-mine choice is offered.
    #[allow(dead_code)] // read by the G2 editor footer (ui::chrome status bar) once wired
    pub save_status: crate::data::SaveStatus,
    /// The frontmatter inspector's drafts + open flag (PLAN-GUI §6.9).
    /// `open = false` by default; `Cmd/Ctrl+Shift+F` or the Properties
    /// button toggles it.
    pub inspector: InspectorState,
    /// WYSIWYG: the index (into `doc.root`) of the block currently in
    /// edit mode, or `None` when reading. Clicking a block in read mode
    /// switches it to edit mode; clicking a different block switches the
    /// focus. Resets to `None` on page switch.
    pub wysiwyg_editing: Option<usize>,
}

impl AppState {
    /// The initial state a fresh app starts in: skeleton content, no data,
    /// the rail pinned to `All pages` with no list group-by (browse parity).
    pub fn new(launch: LaunchRoute, theme: ThemeChoice) -> Self {
        Self {
            phase: Phase::Loading,
            pages: Vec::new(),
            collections: Vec::new(),
            gated_count: 0,
            ungated_count: 0,
            roots_known: false,
            launch,
            theme,
            distinct_languages: Vec::new(),
            collection: CollectionKey::All,
            group_by: GroupBy::None,
            list_filter: String::new(),
            selected_page: None,
            open_page: None,
            create_open: false,
            create_title: String::new(),
            create_type: "wiki".to_string(),
            convert_open: false,
            convert_type: "note".to_string(),
            convert_title: String::new(),
            convert_tags: Vec::new(),
            convert_collections: Vec::new(),
            view_mode: ViewMode::default(),
            search: SearchView::default(),
            save_status: crate::data::SaveStatus::default(),
            inspector: InspectorState::default(),
            wysiwyg_editing: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_when_no_frontmatter() {
        // No `---` frontmatter at all ⇒ no `type:` ⇒ GenericMarkdown
        // regardless of corpus membership.
        assert_eq!(
            classify("# Hello\n\nbody", true),
            DocumentKind::GenericMarkdown
        );
        assert_eq!(
            classify("# Hello\n\nbody", false),
            DocumentKind::GenericMarkdown
        );
    }

    #[test]
    fn generic_when_type_missing() {
        // Frontmatter present but `type:` absent ⇒ not indexable ⇒ Generic.
        let body = "---\ntitle: untitled\ntags:\n  - x\n---\n\n# T\n";
        assert_eq!(classify(body, true), DocumentKind::GenericMarkdown);
        assert_eq!(classify(body, false), DocumentKind::GenericMarkdown);
    }

    #[test]
    fn generic_when_type_unknown() {
        // `type:` present but not one of wiki/note/label ⇒ not indexable.
        let body = "---\ntype: memo\n---\n\n# T\n";
        assert_eq!(classify(body, true), DocumentKind::GenericMarkdown);
        assert_eq!(classify(body, false), DocumentKind::GenericMarkdown);
    }

    #[test]
    fn kb_page_when_valid_type_and_indexed() {
        let body = "---\ntype: note\ntitle: T\n---\n\n# T\n";
        assert_eq!(classify(body, true), DocumentKind::KbPage);
    }

    #[test]
    fn unindexed_when_valid_type_but_not_indexed() {
        // Valid `type:` but the page is not in the corpus (outside every
        // root) ⇒ UnindexedMarkdown (the "add folder as root" banner).
        let body = "---\ntype: wiki\ntitle: T\n---\n\n# T\n";
        assert_eq!(classify(body, false), DocumentKind::UnindexedMarkdown);
    }

    #[test]
    fn body_is_whole_file_not_just_markdown() {
        // Classification runs on the *whole* file (frontmatter + markdown);
        // a known `type:` + body parses to an indexable frontmatter.
        let body = "---\ntype: label\n---\n\ntext\n";
        assert!(kb_core::frontmatter::parse(body).is_indexable());
        assert_eq!(classify(body, true), DocumentKind::KbPage);
    }
}
