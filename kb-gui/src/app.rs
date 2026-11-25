//! The eframe `App`: owns the render state, spawns the DB worker on first
//! paint, polls worker events each frame, and delegates painting to `ui`.
//!
//! G0 invariant (PLAN-GUI §3.4): the very first `update` call paints the
//! empty shell (chrome + skeleton panes) and does NO database work. The
//! worker is spawned at the end of that first `update`, and its
//! `LoadCorpus` command is *queued* — the DB open/migrate/corpus load happen
//! on the worker thread between frames, and the UI only reflects them when
//! an `Evt` arrives.
//!
//! G1 adds: `Cmd+K` search overlay (toggle + dispatch to the worker),
//! `Cmd+E` view toggle, click-to-open (list rows and the search overlay),
//! the `Evt::Page` fold (body arrives → `open_page`), and the rfd-driven
//! "Add folder as root" / "Open Markdown file" flows that the Welcome and
//! Home screens (and `☰`) use.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use eframe::egui;

use crate::data::{self, WorkerHandle};
use crate::state::{AppState, DocumentKind, LaunchRoute, OpenPageState, ThemeChoice, ViewMode};

/// The top-level application (one per window).
pub struct App {
    state: AppState,
    /// The DB hint (`--db` or `None` → default location) captured at startup.
    db_hint: Option<PathBuf>,
    /// The resolved theme tokens (PLAN-GUI §11.1) — the single source of
    /// truth the chrome consumes. Set at startup from prefs/CLI, updated on
    /// live switches (Settings → Appearance).
    theme: crate::theme::Theme,
    /// The user's persisted preferences (PLAN-GUI §11.3). Mutated by
    /// settings; saved (debounced) on switch/exit.
    prefs: crate::preferences::Preferences,
    /// Worker handle, owned here. `None` until the first `update` has
    /// painted the empty shell; `Some` after, for the lifetime of the app.
    worker: Option<WorkerHandle>,
    /// The most recent non-fatal worker warning (rendered in the status bar).
    last_warn: Option<String>,
    /// Quit after this many frames (0 = run until the window is closed). Used
    /// by the headless smoke test for a deterministic first-frame run.
    max_frames: u32,
    /// The frame counter for the `max_frames` quit.
    frames: u32,
}

impl App {
    pub fn new(db_hint: Option<PathBuf>, launch: LaunchRoute, theme: ThemeChoice) -> Self {
        let (theme, prefs) = resolve_theme(theme);
        let theme_choice = match theme.appearance {
            crate::theme::Appearance::Light => ThemeChoice::Light,
            crate::theme::Appearance::Dark => ThemeChoice::Dark,
        };
        Self {
            state: AppState::new(launch, theme_choice),
            db_hint,
            theme,
            prefs,
            worker: None,
            last_warn: None,
            max_frames: 0,
            frames: 0,
        }
    }

    /// The resolved theme tokens (for `main` to project onto the very first
    /// `CreationContext` so frame 1 matches exactly what the app will use).
    pub fn theme_for(choice: ThemeChoice) -> crate::theme::Theme {
        resolve_theme(choice).0
    }

    /// The app's preferences (for the General tab to read/write).
    pub fn prefs(&self) -> &crate::preferences::Preferences {
        &self.prefs
    }

    /// Mutable access to preferences (General tab mutations).
    pub fn prefs_mut(&mut self) -> &mut crate::preferences::Preferences {
        &mut self.prefs
    }

    /// The resolved theme tokens (the Appearance tab + G6-2 renderer read
    /// this). Also the source projected onto the egui context.
    pub fn theme(&self) -> &crate::theme::Theme {
        &self.theme
    }

    /// Every selectable theme: the three built-ins *plus* any custom TOML
    /// theme in `<config>/kb-gui/themes/` (PLAN-GUI §11.2). Custom themes
    /// that fail to parse are *shown* with a warning swatch in the
    /// Appearance tab (so the user can fix the file), but never become the
    /// `self.theme` while in use.
    pub fn all_themes(&self) -> Vec<(String, String, bool)> {
        // Each entry is `(name, display, valid)`. `name` is the value passed
        // to `set_theme` (a built-in's `Theme::name` or a custom TOML's
        // `name` field); `display` is the human-readable label shown in
        // the Settings tab (usually identical to `name` but with the
        // file path appended for invalid themes). `valid` is `false` for
        // a custom TOML that failed to parse (the user can still see it
        // in the list to learn *which* file is broken, but cannot click
        // it to select it).
        let mut out = Vec::new();
        for t in crate::theme::all_builtins() {
            let name = t.name.clone();
            out.push((name.clone(), name, true));
        }
        let dir = crate::theme::themes_dir();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().map(|e| e == "toml").unwrap_or(false) {
                    match crate::theme::from_toml_path(&p) {
                        Ok(t) => {
                            let name = t.name.clone();
                            out.push((name.clone(), name, true));
                        }
                        Err(_) => out.push((
                            p.file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            format!("{} (invalid)", p.display()),
                            false,
                        )),
                    }
                }
            }
        }
        out
    }

    /// Live theme switch (PLAN-GUI §11.3: "applies immediately, no
    /// restart"). Re-projects onto the egui context, updates the stored
    /// tokens, and persists the choice to prefs. Custom themes are
    /// resolved via `theme::themes_dir()` by matching the file stem.
    pub fn set_theme(&mut self, ctx: &egui::Context, name: &str) {
        // Built-ins have a unique `name`; custom themes live in TOML files
        // where the `name` field is the display key. Look up in the themes
        // directory first (covers both), then the built-in registry as a
        // fallback (so `--theme marknote-light` still wins).
        let dir = crate::theme::themes_dir();
        let mut candidate: Option<crate::theme::Theme> = None;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|s| s == "toml").unwrap_or(false) {
                    if let Ok(t) = crate::theme::from_toml_path(&p) {
                        if t.name == name {
                            candidate = Some(t);
                            break;
                        }
                    }
                }
            }
        }
        let Some(theme) = candidate.or_else(|| crate::theme::builtin(name)) else {
            return;
        };
        let choice = match theme.appearance {
            crate::theme::Appearance::Light => ThemeChoice::Light,
            crate::theme::Appearance::Dark => ThemeChoice::Dark,
        };
        self.theme = theme;
        self.state.theme = choice;
        self.prefs.theme_name = self.theme.name.clone();
        let _ = crate::preferences::save(&self.prefs);
        crate::theme::apply(ctx, &self.theme);
    }

    /// The prefs file on disk (for the General tab to *display* where the
    /// values land, and for a "reset to defaults" affordance).
    pub fn prefs_path(&self) -> std::path::PathBuf {
        crate::preferences::path()
    }

    /// Quit after this many painted frames (0 = run until the window closes).
    /// Used by the headless smoke test for a deterministic first-frame run.
    pub fn set_max_frames(&mut self, n: u32) {
        self.max_frames = n;
    }

    /// The mutable render state (used by the UI modules to mutate e.g. the
    /// rail selection, the note-list filter, or the `open_page` slot).
    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    /// The immutable render state.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The worker handle (for the Welcome/Home "Add root" / "Open file"
    /// flows to send a `Cmd` through).
    pub fn worker(&self) -> &Option<WorkerHandle> {
        &self.worker
    }

    /// The most recent non-fatal worker warning (for the status bar).
    pub fn last_warning(&self) -> &Option<String> {
        &self.last_warn
    }

    /// Toggle the `Cmd+K` search overlay. If it is already open, close it;
    /// otherwise open it and (for `initial_query = None`) keep the prior
    /// query + scope so "walk back into results" works (PLAN-GUI §6.8:
    /// "search results keep their state when navigating into a page and
    /// back").
    pub fn toggle_search(&mut self, initial_query: Option<String>) {
        let s = &mut self.state.search;
        if s.open {
            s.open = false;
            return;
        }
        s.open = true;
        // Hand the input focus on first paint (one-shot) so the user can type
        // immediately after Ctrl+K. See `SearchView::focus_next`.
        s.focus_next = true;
        if let Some(q) = initial_query {
            s.query = q;
            // Opening with a fresh query resets the scope chip to the
            // rail's current selection (the plan's "search in this
            // collection" action).
            s.scope = match &self.state.collection {
                crate::state::CollectionKey::Term(t) => Some(t.clone()),
                _ => None,
            };
            s.rows.clear();
            s.cursor = 0;
        }
    }

    /// Open the search overlay (idempotent — a second call while open is a
    /// no-op). `initial_query = Some(q)` replaces the current one.
    pub fn open_search(&mut self, initial_query: Option<String>) {
        if !self.state.search.open {
            self.toggle_search(initial_query);
        }
    }

    /// Run the current search query + scope through the worker. The
    /// results land back via `Evt::SearchResults` on a later frame. The
    /// overlay's text field is the source of truth for the query (the
    /// plan's "search results keep their state when navigating into a page
    /// and back"); the rail selection is the source of truth for the scope.
    pub fn run_search(&mut self) {
        let query = self.state.search.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        // Scope: a Term rail selection scopes the search (PLAN-GUI §2.1
        // rule 3); the Gated/Ungated/All keys are visibility filters, not
        // gate terms, so they leave the scope untouched here (the search
        // layer applies its own gate filter).
        let scope = match &self.state.collection {
            crate::state::CollectionKey::Term(t) => Some(t.clone()),
            _ => self.state.search.scope.clone(),
        };
        // Reflect the scope back into the shared query state so the overlay
        // chip + the status bar stay consistent with what we fire.
        self.state.search.scope = scope.clone();
        // Blank the previous result set so a stale cursor can't index into
        // the arriving rows (the arriving event still resets cursor to 0).
        self.state.search.rows.clear();
        self.state.search.cursor = 0;
        self.state.search.missing_tag = None;
        if let Some(worker) = &self.worker {
            worker.send(data::Cmd::Search { query, scope });
        }
    }

    /// Toggle Formatted / Raw (PLAN-GUI §6.2 / §6.5, `Cmd/Ctrl+E`).
    pub fn toggle_view_mode(&mut self) {
        match self.state.view_mode {
            ViewMode::Formatted => self.state.view_mode = ViewMode::Raw,
            ViewMode::Raw => self.state.view_mode = ViewMode::Formatted,
        }
        // Switching modes invalidates the WYSIWYG edit focus (a Raw view
        // has no splicable blocks to track).
        self.state.wysiwyg_editing = None;
    }

    /// `true` when the page at `abs_path` has a corpus row (PLAN-GUI §162–182
    /// "indexed in pages" — the distinguishing fact between a `KbPage` and an
    /// `UnindexedMarkdown`). Read for document classification in the
    /// `Evt::Page` fold.
    fn is_indexed(&self, abs_path: &str) -> bool {
        self.state.pages.iter().any(|p| p.abs_path == abs_path)
    }

    /// Select a page by abs_path. Triggers a body load through the worker;
    /// the body lands in `open_page` when the `Evt::Page` arrives.
    pub fn open_page(&mut self, abs_path: &str) {
        let is_new = self.state.selected_page.as_deref() != Some(abs_path);
        self.state.selected_page = Some(abs_path.to_string());
        // Reset the open-page slot (or seed it with an in-flight `None`
        // body so the preview renders "loading…").
        if is_new {
            self.state.open_page = Some(OpenPageState {
                abs_path: abs_path.to_string(),
                body: None,
                error: None,
                baseline: None,
                kind: DocumentKind::GenericMarkdown,
                banner_dismissed: false,
            });
            // A new page → all WYSIWYG edit-state is stale.
            self.state.wysiwyg_editing = None;
        }
        if let Some(worker) = &self.worker {
            worker.send(data::Cmd::OpenPage(abs_path.to_string()));
        }
    }

    /// Deselect the current page (return to browse). The page slot is
    /// dropped; the body is not re-fetched next time until the user clicks
    /// it again.
    pub fn close_page(&mut self) {
        self.state.selected_page = None;
        self.state.open_page = None;
        self.state.wysiwyg_editing = None;
    }

    /// Dismiss the open document's conversion/add-root banner for the
    /// session (PLAN-GUI §6.15 rule 6). Non-fatal: the action remains
    /// available from the `⋯` menu / Settings, so dismissal is not permanent.
    pub fn dismiss_banner(&mut self) {
        if let Some(slot) = self.state.open_page.as_mut() {
            slot.banner_dismissed = true;
        }
    }

    /// Open the conversion dialog for the open (generic/unindexed) document
    /// (PLAN-GUI §6.15 "Convert flow"; G3 wires the dialog + `Cmd::ConvertPage`).
    pub fn open_conversion(&mut self) {
        self.state.convert_open = true;
    }

    /// Open the New-note dialog (PLAN-GUI §6.11 `Cmd/Ctrl+N`, §6.10).
    /// Idempotent — a second call while it's already open is a no-op; the
    /// drafts (title / type / tags / collections) are preserved across a
    /// close→reopen so a cancelled attempt doesn't lose what was typed.
    pub fn open_create_note(&mut self) {
        self.state.create_open = true;
    }

    /// Open a Markdown file from disk (native picker → `open_page`). Mirrors
    /// the Welcome/Home "Open Markdown file…" action (PLAN-GUI §6.11 `O`).
    pub fn open_file_picker(&mut self) {
        if !crate::ui::welcome::picker_open_file(self) {
            // The picker was cancelled — nothing else to do.
        }
    }

    /// Refresh the corpus (PLAN-GUI §6.11 `Cmd/Ctrl+R`) — the same path the
    /// Settings → Data **Refresh** button and the rail "refresh" shortcut use.
    pub fn refresh_corpus(&mut self) {
        if let Some(worker) = &self.worker {
            worker.send(data::Cmd::Refresh);
        }
    }

    /// Open the conflict *Show diff* view: snapshot the file's current
    /// on-disk bytes (a best-effort read; on failure the diff window shows a
    /// placeholder) and flag the window. The editor's *current* buffer is the
    /// other pane, so the diff is "my uncommitted edits" vs "what's on disk
    /// now" — the §6.10 "Show diff" action.
    pub fn show_conflict_diff(&mut self) {
        let abs = match self.state.open_page.as_ref() {
            Some(s) => s.abs_path.clone(),
            None => return,
        };
        match std::fs::read_to_string(&abs) {
            Ok(text) => {
                self.state.conflict_diff_snapshot = Some(text);
            }
            Err(_) => {
                // The file was deleted / unreadable — the diff pane stays
                // empty (the window renders a "(disk content unavailable)"
                // hint so the user still gets a decision point).
                self.state.conflict_diff_snapshot = None;
            }
        }
        self.state.conflict_diff_open = true;
    }

    /// Close the conflict diff view (Esc, the window, or a Resolve action).
    pub fn close_conflict_diff(&mut self) {
        self.state.conflict_diff_open = false;
    }

    /// Add the open document's containing folder as a root, then re-index
    /// (PLAN-GUI §6.15 "UnindexedMarkdown" banner → **Add root**). The worker
    /// re-loads the corpus on success; the next `Evt::Page` re-fold
    /// re-classifies the now-indexed document as a `KbPage` and the banner
    /// disappears.
    pub fn add_root_for_open_page(&mut self) {
        let Some(path) = self.state.open_page.as_ref().map(|p| p.abs_path.clone()) else {
            return;
        };
        let parent = Path::new(&path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        if let Some(worker) = &self.worker {
            worker.send(data::Cmd::AddRoot(parent));
        }
    }

    /// Open (or re-open) the Settings modal (PLAN-GUI §6.12). `tab` is the
    /// tab to land on (`Roots` when `None` — corpus administration is the
    /// primary reason a user opens Settings). Re-fetch the admin tables on
    /// every open (the §6.12 rule "any admin action … reflected without
    /// restart" applies to *what's already shown*, which means the table is
    /// as fresh as the DB, not as fresh as a previous `Open` call).
    pub fn open_settings(&mut self, tab: Option<crate::state::SettingsTab>) {
        self.state.settings_open = true;
        if let Some(t) = tab {
            self.state.settings_tab = t;
        }
        // Re-fetch: a previous open's table is *not* a guarantee of today's
        // (a `kb add <dir>` / `kb drop-root <dir>` on the CLI side could
        // have happened since the last time this modal was open).
        self.refresh_settings_tables();
    }

    /// Close the Settings modal (Escape, `×`, `Cmd/Ctrl+.` on the window).
    pub fn close_settings(&mut self) {
        self.state.settings_open = false;
        // A stale "I confirmed the full rebuild" must not carry over to a
        // later open (the §6.12 rule "destructive actions show a dry-run
        // summary first" only holds when every open starts *unconfirmed*
        // — otherwise a one-off checkbox tick enables an unintended wipe).
        self.state.settings_confirm_rebuild = false;
    }

    /// Switch the active tab in the Settings modal. The tab strip reads
    /// `self.state.settings_tab` to style the active one.
    pub fn set_settings_tab(&mut self, tab: crate::state::SettingsTab) {
        self.state.settings_tab = tab;
    }

    /// Re-fetch the Roots / Aliases / Data tables (the §6.12 modal's
    /// admin views). Called on `open_settings` and on any mutation
    /// event that invalidates them (`RebuildDone`, `RootDropped`, …).
    /// Cheap: three `COUNT(*)` + two small `SELECT…ORDER BY` per
    /// round-trip; the worker does it off the UI thread.
    pub(crate) fn refresh_settings_tables(&mut self) {
        if let Some(worker) = &self.worker {
            worker.send(data::Cmd::ListRoots);
            worker.send(data::Cmd::ListAliases);
            worker.send(data::Cmd::Stats);
        }
    }

    /// The §6.12 Roots tab: register the picked folder as a root +
    /// re-index + reload the corpus (the *same* flow as the `Add root`
    /// button on the Welcome screen — this is the one the *tab* uses).
    pub fn add_root_settings(&mut self) {
        let Some(worker) = self.worker() else {
            return;
        };
        let folder = rfd::FileDialog::new()
            .set_title("Choose a folder to add as a root")
            .pick_folder();
        if let Some(path) = folder {
            worker.send(data::Cmd::AddRoot(path));
            // The AddRoot dispatch reloads the corpus (the worker does
            // that), but *our* tabs need the table re-fetch too — the
            // new row is in `roots` and the per-root page count is
            // what we want to show immediately.
            self.refresh_settings_tables();
        }
    }

    /// The §6.12 Roots tab: unregister the picked row (the §6.12
    /// "confirm; files untouched" rule). `abs` is the *root's* path,
    /// as listed in the Roots table (a `RootInfo.path`).
    pub fn drop_root_settings(&mut self, abs: &str) {
        if let Some(worker) = self.worker() {
            worker.send(data::Cmd::DropRoot(std::path::PathBuf::from(abs)));
            // The DropRoot dispatch reloads the corpus (the worker does
            // that) *and* runs the GC sweep the §6.12 rule calls out
            // ("the DB rows disappear on the next refresh"). Re-fetch
            // the tabs on the `RootDropped` event.
            self.refresh_settings_tables();
        }
    }

    /// The §6.12 Data tab: an incremental `refresh` (the `kb refresh`
    /// command). The worker runs `kb_db::refresh::refresh` + reloads
    /// the corpus + emits `RebuildDone`.
    pub fn run_refresh(&mut self) {
        if let Some(worker) = self.worker() {
            worker.send(data::Cmd::Refresh);
        }
    }

    /// The §6.12 Data tab: the *full* rebuild (`kb refresh --full`).
    /// The §6.12 rule "destructive actions show a dry-run summary first"
    /// is handled by the UI layer (a typed "rebuild" confirmation) — the
    /// worker itself does not gate, because the dry-run summary is
    /// *what this button does* (a wipe + re-walk from scratch, the full
    /// `kb_db::refresh::full_rebuild`).
    pub fn run_full_rebuild(&mut self) {
        if let Some(worker) = self.worker() {
            worker.send(data::Cmd::FullRebuild);
        }
    }

    /// The §6.12 "Tag aliases" tab: add or upsert an `alias → canonical`
    /// row. The §6.12 rule "alias ≠ canonical, canonical must exist or be
    /// explicitly allowed" is enforced in the *worker's `AddAlias`
    /// dispatch* (the "alias ≠ canonical" semantic check + the
    /// non-empty check), so a UI typo is a `Warn`, not a no-op add.
    pub fn add_alias(&mut self, alias: &str, canonical: &str) {
        if let Some(worker) = self.worker() {
            worker.send(data::Cmd::AddAlias {
                alias: alias.to_string(),
                canonical: canonical.to_string(),
            });
            // The `AliasChanged` event re-fetches the table (we trust
            // the worker's post-commit view) — no extra fetch here.
        }
    }

    /// The §6.12 "Tag aliases" tab: remove the picked `alias`.
    pub fn remove_alias(&mut self, alias: &str) {
        if let Some(worker) = self.worker() {
            worker.send(data::Cmd::RemoveAlias {
                alias: alias.to_string(),
            });
        }
    }

    /// Apply the one-shot launch route (`kb-gui <file>` /
    /// `kb-gui <file>#Section`) against the now-loaded corpus: resolve the
    /// abs path, select the page, and switch to the editable raw view. The
    /// route is consumed (replaced by `Home`) so a later corpus refresh
    /// (e.g. `kb refresh` while running) does not re-open the launch page
    /// over the user's navigation.
    fn apply_launch_route(&mut self) {
        let route = std::mem::replace(&mut self.state.launch, LaunchRoute::Home);
        let spec = match route {
            LaunchRoute::Home => return,
            // `OpenSection`'s section-anchor jump lands in G3 (the outline
            // can scroll the editor to the heading); for now open the page.
            LaunchRoute::OpenFile(spec) | LaunchRoute::OpenSection { file: spec, .. } => spec,
        };
        let spec = spec.as_str();
        // Resolve: an exact row abs_path, or the row whose display id or
        // title matches (the "uuid / slug / title" page-ref rules, §2.4).
        let mut resolved: Option<String> = self
            .state
            .pages
            .iter()
            .find(|p| p.abs_path == spec)
            .map(|p| p.abs_path.clone());
        if resolved.is_none() {
            resolved = self
                .state
                .pages
                .iter()
                .find(|p| p.id == spec || p.title.as_deref() == Some(spec))
                .map(|p| p.abs_path.clone());
        }
        if let Some(abs) = resolved {
            self.state.view_mode = ViewMode::Raw;
            self.open_page(&abs);
        } else if std::path::Path::new(spec).is_file() {
            // Not in the corpus (generic/outside-roots markdown), but a real
            // file on disk (PLAN-GUI §181: "a file path always opens; only
            // `kb` features are gated"). Open it directly — the `Evt::Page`
            // fold classifies it (banner + disabled features).
            let canonical =
                std::fs::canonicalize(spec).unwrap_or_else(|_| std::path::PathBuf::from(spec));
            let abs = canonical.to_string_lossy().into_owned();
            self.state.view_mode = ViewMode::Formatted;
            self.open_page(&abs);
        }
    }

    /// Mark the open document dirty (the editor text changed). The UI calls
    /// this when the raw/formatted buffer's text drifts from the baseline.
    pub fn mark_unsaved(&mut self) {
        if self.state.save_status == data::SaveStatus::Conflict {
            // A conflict is the strongest signal — do not downgrade it to
            // Unsaved (the user still owes a Reload / Keep-mine decision).
            return;
        }
        self.state.save_status = data::SaveStatus::Unsaved;
    }

    /// Save the open document (PLAN-GUI §3.3): the editor's current bytes go
    /// over the wire with the baseline captured at open; the worker does the
    /// conflict guard → atomic write → single-page reindex and answers with
    /// `Saved` / `Conflict` / `Warn`.
    pub fn save_page(&mut self) {
        self.save_page_with_force(false);
    }

    /// "Keep mine" resolve step (PLAN-GUI §6.10 Conflict): save the open
    /// document *over* a mid-session external change. Dispatches a `Save`
    /// with `force = true` (skips the conflict guard on the worker side).
    pub fn save_page_force(&mut self) {
        self.save_page_with_force(true);
    }

    fn save_page_with_force(&mut self, force: bool) {
        // Gather the save payload; skip the dispatch when the slot is not
        // ready to write (body missing or baseline uncaptured).
        let slot = match self.state.open_page.as_ref() {
            Some(s) if s.body.is_some() && s.baseline.is_some() => s,
            _ => return,
        };
        let abs_path = slot.abs_path.clone();
        let baseline = slot.baseline.clone().unwrap_or_default();
        let body = slot.body.clone().unwrap_or_default();
        let new_bytes = body.into_bytes();
        self.state.save_status = data::SaveStatus::Saving;
        if let Some(worker) = &self.worker {
            worker.send(data::Cmd::Save {
                abs_path,
                expected_disk: baseline,
                new_bytes,
                force,
            });
        } else {
            // No worker yet (first frame not sent). Fall back to clean rather
            // than wedging in `Saving` forever.
            self.state.save_status = data::SaveStatus::Unsaved;
        }
    }

    /// Create a new note in a directory. The worker scaffolds + writes +
    /// indexes and answers with `Created` (then auto-opens via `Cmd::OpenPage`)
    /// or `Warn` (a validation / IO failure).
    pub fn create_page(
        &mut self,
        directory: String,
        title: String,
        page_type: String,
        tags: Vec<String>,
        gates: Vec<String>,
    ) {
        if let Some(worker) = &self.worker {
            worker.send(data::Cmd::CreatePage {
                directory,
                title,
                page_type,
                tags,
                gates,
                description: None,
                language: None,
            });
        }
    }

    /// Convert the open (generic/unindexed) document into a full `KbPage`
    /// (PLAN-GUI §6.15 "Convert flow"). Sends `Cmd::ConvertPage` with the
    /// open page's baseline (conflict guard) + the dialog's `type:` /
    /// `title:` / `tags:` / `gates:`; the worker preserves the body + other
    /// frontmatter, sets/extends the frontmatter, adds a root if needed,
    /// writes atomically, reindexes, and answers with `Converted` /
    /// `Conflict` / `Warn`.
    pub fn convert_open_page(
        &mut self,
        page_type: String,
        title: String,
        tags: Vec<String>,
        gates: Vec<String>,
    ) {
        let slot = match self.state.open_page.as_ref() {
            Some(s) if s.body.is_some() && s.baseline.is_some() => s,
            _ => return,
        };
        let abs_path = slot.abs_path.clone();
        let baseline = slot.baseline.clone().unwrap_or_default();
        if let Some(worker) = &self.worker {
            worker.send(data::Cmd::ConvertPage {
                abs_path,
                expected_disk: baseline,
                page_type,
                title,
                tags,
                gates,
            });
        }
    }

    /// Seed the frontmatter inspector from the open page's on-disk frontmatter
    /// (PLAN-GUI §6.9). Captures the baseline (the values the op-diff is
    /// computed against) and seeds the drafts to the same values, so a
    /// freshly-opened panel is *clean* (no pending changes) until the user
    /// edits a field. Called when the panel first opens for a page.
    pub fn seed_inspector(&mut self) {
        let Some(slot) = self.state.open_page.as_ref() else {
            return;
        };
        let Some(body) = slot.body.as_ref() else {
            return;
        };
        let bytes = body.as_bytes();
        let fm = kb_core::frontmatter::parse(body);
        // `language:` is not a `Frontmatter` field — read it raw off the
        // frontmatter bytes (it is an *extra* scalar, per §6.9).
        let language = kb_core::frontmatter::scalar_value(bytes, "language");
        let tags = kb_core::frontmatter::list_values(bytes, "tags").unwrap_or_default();
        let collections =
            kb_core::frontmatter::list_values(bytes, "search_required_phrase").unwrap_or_default();
        let ins = &mut self.state.inspector;
        ins.abs_path = Some(slot.abs_path.clone());
        let type_val = fm.type_raw.clone().unwrap_or_default();
        ins.baseline_type = if type_val.is_empty() {
            None
        } else {
            Some(type_val.clone())
        };
        ins.baseline_title = fm.title.clone();
        ins.baseline_description = fm.description.clone();
        ins.baseline_language = language.clone();
        ins.baseline_tags = tags.clone();
        ins.baseline_collections = collections.clone();
        // Seed the drafts to the baseline (clean state on open).
        ins.draft_type = type_val;
        ins.draft_title = fm.title.clone().unwrap_or_default();
        ins.draft_description = fm.description.clone().unwrap_or_default();
        ins.draft_language = language.unwrap_or_default();
        ins.draft_tags = tags;
        ins.draft_collections = collections;
        ins.draft_tags_input.clear();
        ins.draft_collections_input.clear();
        ins.focus = crate::state::InspectorField::Type;
        ins.open = true;
    }

    /// Toggle the frontmatter inspector (PLAN-GUI §6.11, `Ctrl+Shift+F`).
    /// Opening seeds the drafts from the open page if unseeded (or the page
    /// changed); closing just flips the flag (drafts persist so a re-open is
    /// cheap).
    pub fn toggle_inspector(&mut self) {
        let ins = &mut self.state.inspector;
        if ins.open {
            ins.open = false;
            return;
        }
        if self.state.selected_page.is_none() {
            // No page open: still open the panel; `seed_inspector` is a no-op
            // and the panel renders its placeholder until a page opens.
            ins.open = true;
            return;
        }
        // Re-seed if the drafts are missing or stale (a different page is
        // now selected than the drafts were captured for).
        let stale = match (&ins.abs_path, self.state.open_page.as_ref()) {
            (Some(a), Some(s)) if a == &s.abs_path => {
                ins.baseline_type.is_none()
                    && ins.baseline_title.is_none()
                    && ins.draft_type.is_empty()
                    && ins.draft_title.is_empty()
                    && ins.draft_description.is_empty()
                    && ins.draft_tags.is_empty()
                    && ins.baseline_tags.is_empty()
                    && ins.baseline_collections.is_empty()
            }
            _ => true,
        };
        if stale {
            self.seed_inspector();
        } else {
            ins.open = true;
        }
    }

    /// Compute the pending `FmOp` batch by diffing the drafts against the
    /// baseline (PLAN-GUI §6.9: the op set *is* the commit). Empty = clean.
    ///
    /// The diff is order-stable: scalars first (type, title, description,
    /// language), then tags (removes then adds), then collections. This keeps
    /// the worker's apply-order deterministic and the warn message readable.
    pub fn inspector_ops(&self) -> Vec<data::FmOp> {
        let ins = &self.state.inspector;
        let mut ops = Vec::new();
        // Scalar fields: a draft change against the baseline ⇒ one op.
        // (baseline_value, key, draft_value) for the four scalar fields.
        let scalar: Vec<(Option<String>, &str, String)> = vec![
            (ins.baseline_type.clone(), "type", ins.draft_type.clone()),
            (ins.baseline_title.clone(), "title", ins.draft_title.clone()),
            (
                ins.baseline_description.clone(),
                "description",
                ins.draft_description.clone(),
            ),
            (
                ins.baseline_language.clone(),
                "language",
                ins.draft_language.clone(),
            ),
        ];
        for (base, key, draft) in scalar {
            // An empty draft means "remove the field" (the panel's clear-able
            // semantics); an empty baseline means "add the field".
            let draft_nonempty = !draft.trim().is_empty();
            let base_nonempty = base
                .as_deref()
                .map(|b| !b.trim().is_empty())
                .unwrap_or(false);
            let changed = base.as_deref() != Some(draft.as_str());
            match (base_nonempty, draft_nonempty, changed) {
                // Both present and the value differs → set.
                (true, true, true) => ops.push(data::FmOp::SetScalar {
                    key: key.to_string(),
                    value: draft,
                }),
                // Present in file, cleared in draft → remove the field.
                (true, false, _) => ops.push(data::FmOp::Remove {
                    key: key.to_string(),
                }),
                // Absent in file, added in draft → set.
                (false, true, _) => ops.push(data::FmOp::SetScalar {
                    key: key.to_string(),
                    value: draft,
                }),
                // Unchanged / both empty → nothing.
                (false, false, _) | (true, true, false) => {}
            }
        }
        // tags: diff the ordered lists (removes then adds, first-occurrence
        // order preserved).
        for t in &ins.baseline_tags {
            if !ins.draft_tags.iter().any(|d| d == t) {
                ops.push(data::FmOp::RemoveItem {
                    key: "tags".to_string(),
                    value: t.clone(),
                });
            }
        }
        for t in &ins.draft_tags {
            if !ins.baseline_tags.iter().any(|b| b == t) {
                ops.push(data::FmOp::AppendItem {
                    key: "tags".to_string(),
                    value: t.clone(),
                });
            }
        }
        for c in &ins.baseline_collections {
            if !ins.draft_collections.iter().any(|d| d == c) {
                ops.push(data::FmOp::RemoveItem {
                    key: "search_required_phrase".to_string(),
                    value: c.clone(),
                });
            }
        }
        for c in &ins.draft_collections {
            if !ins.baseline_collections.iter().any(|b| b == c) {
                ops.push(data::FmOp::AppendItem {
                    key: "search_required_phrase".to_string(),
                    value: c.clone(),
                });
            }
        }
        ops
    }

    /// The pending-changes summary (a short human line for the panel footer +
    /// the status bar). `None` = no changes (clean). The string mirrors the
    /// op order for scannability.
    pub fn inspector_pending_summary(&self) -> Option<String> {
        let ops = self.inspector_ops();
        if ops.is_empty() {
            return None;
        }
        let mut parts = Vec::with_capacity(ops.len());
        for op in &ops {
            parts.push(data_mod_summarize(op));
        }
        Some(parts.join(", "))
    }

    /// Commit the inspector's drafts: send the `FmOp` batch through the
    /// worker (conflict-guard → apply → write → reindex). No-op when clean.
    /// On `Saving` the panel disables its own inputs and Commit button until
    /// the worker's `Saved` / `Conflict` / `Warn` arrives.
    pub fn commit_inspector(&mut self) {
        let Some(slot) = self.state.open_page.as_ref() else {
            return;
        };
        let Some(baseline) = slot.baseline.clone() else {
            return;
        };
        let ins = &self.state.inspector;
        if ins.abs_path.as_deref() != Some(slot.abs_path.as_str()) {
            // Stale drafts (a different page is open than the seeds were for).
            return;
        }
        let ops = self.inspector_ops();
        if ops.is_empty() {
            // Clean: a commit is a no-op the worker would refuse anyway.
            return;
        }
        self.state.save_status = data::SaveStatus::Saving;
        self.state.inspector.saving = true;
        if let Some(worker) = &self.worker {
            worker.send(data::Cmd::SaveFrontmatter {
                abs_path: slot.abs_path.clone(),
                expected_disk: baseline,
                ops,
            });
        } else {
            self.state.save_status = data::SaveStatus::Unsaved;
            self.state.inspector.saving = false;
        }
    }

    /// Revert the inspector's drafts to the baseline (a `Revert` — undo all
    /// local edits without hitting disk). The panel's chips + inputs + scalar
    /// fields reset to the captured seeds.
    pub fn revert_inspector(&mut self) {
        let ins = &mut self.state.inspector;
        ins.saving = false;
        ins.draft_type = ins.baseline_type.clone().unwrap_or_default();
        ins.draft_title = ins.baseline_title.clone().unwrap_or_default();
        ins.draft_description = ins.baseline_description.clone().unwrap_or_default();
        ins.draft_language = ins.baseline_language.clone().unwrap_or_default();
        ins.draft_tags = ins.baseline_tags.clone();
        ins.draft_collections = ins.baseline_collections.clone();
        ins.draft_tags_input.clear();
        ins.draft_collections_input.clear();
        self.state.save_status = data::SaveStatus::Saved;
        self.state.conflict_diff_open = false;
        self.state.conflict_diff_snapshot = None;
    }

    /// Re-seed the inspector's baseline + drafts from the just-landed page
    /// body if the inspector is open for that page (the body arrived, so the
    /// on-disk frontmatter is now known). Safe to call when the inspector is
    /// not open for the page (a no-op). The drafts are discarded — the user
    /// must re-edit (a fresh open is the only consistent state).
    fn maybe_reseed_inspector_for(&mut self, abs_path: &str) {
        if self.state.inspector.abs_path.as_deref() == Some(abs_path)
            && self.state.inspector.open
            && self
                .state
                .open_page
                .as_ref()
                .is_some_and(|s| s.body.is_some())
        {
            self.seed_inspector();
        }
    }
}

/// A short human form of an `FmOp` for the pending-changes footer (mirrors
/// the worker-side `summarize_op`, kept in `app.rs` so the UI doesn't
/// depend on the data module's private helper).
fn data_mod_summarize(op: &data::FmOp) -> String {
    match op {
        data::FmOp::SetScalar { key, value } => format!("set {key}={value:?}"),
        data::FmOp::Remove { key } => format!("remove {key}"),
        data::FmOp::AppendItem { key, value } => format!("+{key} {value:?}"),
        data::FmOp::RemoveItem { key, value } => format!("-{key} {value:?}"),
    }
}

/// Global entry point so the `ui` modules can call `open_search` without
/// re-borrowing the `App` (the closure in `paint` already holds `&mut App`).
pub fn open_search(app: &mut App, initial_query: Option<String>) {
    app.open_search(initial_query);
}

/// Resolve a CLI/prefs theme choice into the concrete tokens + the prefs to
/// persist. A single source of truth shared by `App::new` and `main` so
/// frame 1 and the steady-state theme can never disagree.
///
/// Rule (PLAN-GUI §11.3): an explicit `--theme dark|light` wins for this
/// run; `System` honors the persisted `theme_name` (falling back to
/// marknote-dark when no prefs exist). A custom `theme_name` is resolved via
/// `theme::builtin` (G6-2 will extend this to the TOML loader).
fn resolve_theme(choice: ThemeChoice) -> (crate::theme::Theme, crate::preferences::Preferences) {
    let prefs = crate::preferences::load();
    let theme = match choice {
        ThemeChoice::Light => crate::theme::marknote_light(),
        ThemeChoice::Dark => crate::theme::marknote_dark(),
        ThemeChoice::System => match crate::theme::builtin(&prefs.theme_name) {
            Some(t) => t,
            None => crate::theme::marknote_dark(),
        },
    };
    (theme, prefs)
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // 1) Poll worker events (non-blocking; the empty case is normal).
        if let Some(worker) = &self.worker {
            for evt in worker.recv() {
                self.fold_event(evt);
            }
        }

        // 2) Process global keyboard shortcuts *before* the UI repaint so
        //    they take precedence over any per-pane focus handling.
        handle_g1_shortcuts(self, ui, &ctx);

        // 3) Paint the shell (Welcome / Home / browse / preview) + the
        //    search overlay on top (`search::paint_if_open` is a no-op
        //    when the overlay is closed).
        crate::ui::paint(self, ui);

        // 4) First-frame bookkeeping: on the very first `update` call,
        //    after the shell has been laid out, spawn the worker and kick
        //    off the startup load. This is the "spawned from the first
        //    `update` after laying out the empty shell" rule (PLAN-GUI
        //    §3.2 / §3.4).
        if self.worker.is_none() {
            let handle = data::spawn_worker();
            handle.send(data::Cmd::LoadCorpus {
                launch: self.state.launch.clone(),
                db: self.db_hint.clone(),
            });
            self.worker = Some(handle);

            // The first-frame clock: record it here so the test harness
            // (and `--startup-report`) can read it after the run.
            static FIRST_PAINT: OnceLock<u64> = OnceLock::new();
            if FIRST_PAINT.get().is_none() {
                crate::record_first_frame();
            }
        }

        // 5) Ask egui to repaint as soon as anything could have changed
        //    (worker events, input, …). G0's steady state is one paint per
        //    event loop tick; cheap because the shell is light.
        ctx.request_repaint();

        // 6) `--max-frames N`: quit after N painted frames (the headless
        //    smoke test drives a deterministic first-frame run + exit).
        if self.max_frames > 0 {
            self.frames += 1;
            if self.frames >= self.max_frames {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

/// G1 shortcuts (PLAN-GUI §6.11). The `ui` arg is the `Ui` that owns the
/// current frame; we read its `ctx` to query the `Modifiers` state.
fn handle_g1_shortcuts(app: &mut App, ui: &egui::Ui, ctx: &egui::Context) {
    if !ui.input(|i| i.key_pressed(egui::Key::Escape))
        && !ui.input(|i| i.key_pressed(egui::Key::Enter))
    {
        // Only the "open" / "toggle" keys are of interest here. (Esc /
        // Enter are handled by the focused widget, and also checked in the
        // overlay itself — we skip them here to avoid double-handling.)
    }

    let mods = ui.input(|i| i.modifiers);
    let ctrl_or_cmd = mods.ctrl || mods.command;

    if ctrl_or_cmd && ui.input(|i| i.key_pressed(egui::Key::K)) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        app.toggle_search(None);
    }
    if ctrl_or_cmd && !mods.shift && ui.input(|i| i.key_pressed(egui::Key::E)) {
        app.toggle_view_mode();
    }
    // `Ctrl+S` / `Cmd+S` — save the open document (the editor's own
    // `TextEdit` does not swallow a `Ctrl+S`; it only consumes single-key
    // events while it has focus). Fires only when a page is open.
    if ctrl_or_cmd
        && !mods.shift
        && ui.input(|i| i.key_pressed(egui::Key::S))
        && app.state().selected_page.is_some()
    {
        app.save_page();
    }
    // `Ctrl+Shift+F` / `Cmd+Shift+F` — toggle the frontmatter inspector
    // (PLAN-GUI §6.11). Shift-qualified so it doesn't collide with the
    // browser-style Ctrl+F find (which egui doesn't own, but the user's
    // muscle-memory says "F = find properties").
    if ctrl_or_cmd && mods.shift && ui.input(|i| i.key_pressed(egui::Key::F)) {
        app.toggle_inspector();
    }
    // `Ctrl+Enter` / `Cmd+Enter` — commit the inspector's drafts (PLAN-GUI
    // §6.9 keyboard: "Ctrl+Enter commits the inspector"). Fires only when a
    // page is open and the inspector has something to commit.
    if ctrl_or_cmd
        && ui.input(|i| i.key_pressed(egui::Key::Enter))
        && app.state().selected_page.is_some()
    {
        app.commit_inspector();
    }
    // `Ctrl+,` / `Cmd+,` — open/cycle the Settings modal (PLAN-GUI §6.12:
    // "an in-app modal/sheet with a left tab list … Cmd/Ctrl+,"). `Key::Comma`
    // is egui's token for the `,` key. Toggles open/closed (the
    // `open_settings` is the open path; `close_settings` is the
    // close path); a re-open lands on `Roots` (corpus admin is the
    // default tab), so no tab state to preserve across a toggle.
    if ctrl_or_cmd && !mods.shift && ui.input(|i| i.key_pressed(egui::Key::Comma)) {
        if app.state().settings_open {
            app.close_settings();
        } else {
            app.open_settings(None);
        }
    }
    // `Ctrl+N` / `Cmd+N` — open the New-note dialog (PLAN-GUI §6.11).
    // Shift-qualified keys below would shadow plain `N` on some layouts, but
    // §6.11 lists `N` with no modifier, so we keep it unqualified here.
    if ctrl_or_cmd && !mods.shift && ui.input(|i| i.key_pressed(egui::Key::N)) {
        app.open_create_note();
    }
    // `Ctrl+O` / `Cmd+O` — open a Markdown file (native picker → open_page).
    // The picker is modal, so no double-open risk (a cancelled picker is a no-op).
    if ctrl_or_cmd && !mods.shift && ui.input(|i| i.key_pressed(egui::Key::O)) {
        app.open_file_picker();
    }
    // `Ctrl+R` / `Cmd+R` — refresh the corpus (re-walk + reindex). No page
    // has to be open; the worker re-loads the corpus on success.
    if ctrl_or_cmd && !mods.shift && ui.input(|i| i.key_pressed(egui::Key::R)) {
        app.refresh_corpus();
    }
}

impl App {
    /// Merge one worker event into the render state. Pure state mutation —
    /// no I/O here, so the UI thread stays in control of its invariants.
    fn fold_event(&mut self, evt: data::Evt) {
        match evt {
            data::Evt::Corpus {
                pages,
                roots_known,
                collections,
                gated_count,
                ungated_count,
                distinct_languages,
            } => {
                // A *later* corpus load (e.g. an `AddRoot` / `Refresh`) changed
                // the indexed set — if the currently-open page is now in the
                // corpus, re-classify it (UnindexedMarkdown → KbPage) so the
                // banner drops and `kb` features enable (PLAN-GUI §6.15 step 4).
                if let Some(slot) = self.state.open_page.as_ref() {
                    if let Some(body) = &slot.body {
                        let new_kind =
                            crate::state::classify(body, self.is_indexed(&slot.abs_path));
                        if new_kind != slot.kind
                            && !slot.banner_dismissed
                            && new_kind == DocumentKind::KbPage
                        {
                            // The document is now a full `kb` page — drop the
                            // banner and mark it clean (PLAN-GUI §6.15 step 4).
                            // No re-open needed: the body is current on disk.
                            if let Some(s) = self.state.open_page.as_mut() {
                                s.kind = new_kind;
                                s.banner_dismissed = false;
                            }
                            self.last_warn = None;
                            self.state.save_status = data::SaveStatus::Saved;
                        }
                    }
                }
                // The corpus (and its collection facets + visibility counts)
                // just arrived. Store it all; the rail and the list read
                // straight off these fields.
                self.state.roots_known = roots_known;
                self.state.collections = collections;
                self.state.gated_count = gated_count;
                self.state.ungated_count = ungated_count;
                self.state.distinct_languages = distinct_languages;
                self.state.pages = pages;
                // Selection / open-page: keep the user's current selection
                // if it is still present in the new corpus; otherwise drop
                // it (the page may have been rebuilt without that row).
                if let Some(abs) = self.state.selected_page.clone() {
                    if !self.state.pages.iter().any(|p| p.abs_path == abs) {
                        self.state.selected_page = None;
                        self.state.open_page = None;
                    }
                }
                self.state.phase = crate::state::Phase::Ready;
                // Apply the one-shot launch route now that the corpus is in
                // hand (PLAN-GUI §3.4: `kb-gui <file>` opens that page once
                // the page list is ready). Applied *after* the selection
                // reconciliation above so a stale selection cannot win.
                self.apply_launch_route();
            }
            data::Evt::SearchResults {
                query,
                scope,
                rows,
                missing_tag,
            } => {
                // The search results arrived. Update the overlay's state
                // *only* if the overlay is still open — a stale result
                // from a closed overlay (e.g. the user opened it, typed a
                // query, navigated into a page, and came back) should not
                // clobber the in-progress rows.
                //
                // The rule per PLAN-GUI §6.8: "search results keep their
                // state when navigating into a page and back" — we keep
                // *our* state, which means the *next* time one lands we
                // overwrite it (the worker only sends one SearchResults per
                // Search command).
                let s = &mut self.state.search;
                s.query = query;
                s.scope = scope;
                s.rows = rows;
                s.missing_tag = missing_tag;
                s.cursor = 0;
            }
            data::Evt::Page {
                abs_path,
                body,
                baseline,
            } => {
                // The body (and its on-disk baseline) arrived for the selected
                // page. Clone the path up front so the match arms below can
                // move it into `OpenPageState` and we can still use it for
                // the inspector re-seed afterwards.
                let ap = abs_path.clone();
                if self.state.selected_page.as_deref() == Some(abs_path.as_str()) {
                    // Classify the loaded body against the corpus (PLAN-GUI
                    // §6.15 / §162–182): GenericMarkdown when no valid `type:`,
                    // else KbPage if indexed, else UnindexedMarkdown.
                    let kind = crate::state::classify(&body, self.is_indexed(&abs_path));
                    match self.state.open_page.as_mut() {
                        Some(slot) if slot.abs_path == abs_path => {
                            slot.body = Some(body);
                            slot.error = None;
                            slot.baseline = Some(baseline);
                            slot.kind = kind;
                            slot.banner_dismissed = false;
                        }
                        _ => {
                            self.state.open_page = Some(OpenPageState {
                                abs_path,
                                body: Some(body),
                                error: None,
                                baseline: Some(baseline),
                                kind,
                                banner_dismissed: false,
                            });
                        }
                    }
                    // A fresh load resets the document to clean.
                    self.state.save_status = data::SaveStatus::default();
                    // Re-seed the inspector from the fresh frontmatter so its
                    // drafts (and the op-diff baseline) reflect what just
                    // landed on disk.
                    self.maybe_reseed_inspector_for(&ap);
                }
            }
            data::Evt::Saved { abs_path } => {
                // The write + reindex landed. Mark the document clean.
                self.last_warn = None;
                if self
                    .state
                    .open_page
                    .as_ref()
                    .is_some_and(|p| p.abs_path == abs_path)
                {
                    // Re-baseline: the on-disk bytes are now *our* bytes, so
                    // the conflict guard's baseline is the edited content.
                    if let Some(slot) = self.state.open_page.as_mut() {
                        if let Some(body) = slot.body.as_ref() {
                            slot.baseline = Some(body.as_bytes().to_vec());
                        }
                    }
                    self.state.save_status = data::SaveStatus::Saved;
                    // The save is done — clear the inspector's `saving` flag
                    // and re-seed its baseline so "clean" again means the
                    // freshly-written bytes (the drafts are now the baseline).
                    self.state.inspector.saving = false;
                    if self.state.inspector.abs_path.as_deref() == Some(abs_path.as_str()) {
                        // Re-seed: the new on-disk values become the baseline
                        // AND the drafts (the commit is the source of truth
                        // now).
                        self.seed_inspector();
                    }
                }
            }
            data::Evt::Conflict { abs_path } => {
                // The file changed on disk under us — flag the conflict so the
                // editor can offer Reload / Keep-mine (PLAN-GUI §3.3).
                self.last_warn = Some(format!(
                    "{abs_path} changed on disk since this session — Reload or Keep-mine"
                ));
                if self
                    .state
                    .open_page
                    .as_ref()
                    .is_some_and(|p| p.abs_path == abs_path)
                {
                    self.state.save_status = data::SaveStatus::Conflict;
                }
            }
            data::Evt::Created { abs_path, .. } => {
                // A new note was written + indexed. Open it in the editor.
                if let Some(worker) = &self.worker {
                    worker.send(data::Cmd::OpenPage(abs_path));
                }
                self.state.save_status = data::SaveStatus::Saved;
            }
            data::Evt::Converted { abs_path, .. } => {
                // `ConvertPage` succeeded (PLAN-GUI §6.15 step 4): the page is
                // now indexed (a `KbPage`). Close the dialog, re-open the page
                // so the `Evt::Page` fold re-classifies it (the banner
                // disappears and all `kb` features enable), and mark it clean.
                self.last_warn = None;
                self.state.convert_open = false;
                if self
                    .state
                    .open_page
                    .as_ref()
                    .is_some_and(|p| p.abs_path == abs_path)
                {
                    self.state.save_status = data::SaveStatus::Saved;
                    if let Some(worker) = &self.worker {
                        worker.send(data::Cmd::OpenPage(abs_path));
                    }
                }
            }
            data::Evt::RebuildDone(summary) => {
                // A refresh / full-rebuild finished (the §6.12 "long
                // operations show progress" rule). Store the last result in
                // the Settings → Data tab state (it is read there on each
                // open), clear the stale last-warn, and *re-fetch* the
                // roots / stats tables so the corpus is current (the
                // `RebuildDone` event is only the *number* change; the
                // root/alias tables must be re-read against the fresh DB).
                self.last_warn = None;
                self.state.settings_last_rebuild = Some(summary);
                // The worker has already reloaded the corpus (every
                // mutation path calls `load_corpus_evt`), so this is just
                // the *refresh of the admin views* (the §6.12 rule that
                // "any admin action … reflected without restart" applies to
                // the Roots / Data tabs, not just the rail + list).
                if self.state.settings_open {
                    self.refresh_settings_tables();
                }
            }
            data::Evt::Roots(rows) => {
                self.state.settings_roots = Some(rows);
            }
            data::Evt::Aliases(rows) => {
                // The alias table landed. *Invalidate* the add-row draft if
                // it would have collided with a row that *just* appeared
                // (a concurrent add in another pane should not be silently
                // hidden by a stale draft here).
                self.state.settings_aliases = Some(rows.clone());
                let alias_d = self.state.settings_alias_new.0.clone();
                if !alias_d.is_empty() && rows.iter().any(|r| r.alias == alias_d) {
                    self.state.settings_alias_new = (String::new(), String::new());
                }
            }
            data::Evt::Stats(snapshot) => {
                self.state.settings_stats = Some(snapshot);
            }
            data::Evt::RootDropped { removed, summary } => {
                // A root was unregistered (the §6.12 "removing a root is
                // confirmed and explains that the DB rows disappear on the
                // next refresh" rule). Reflect the last result + the
                // dropped-row count the CLI reports; re-fetch the tables
                // (the root is gone, the per-root page count is gone, and
                // the corpus was GC'd by the worker's post-drop refresh).
                self.last_warn = Some(format!(
                    "dropped {removed} root(s); {} (the rows under the dropped
                     root were GC'd by the post-drop refresh)",
                    summary.note
                ));
                self.state.settings_last_rebuild = Some(summary);
                if self.state.settings_open {
                    self.refresh_settings_tables();
                }
            }
            data::Evt::AliasChanged {
                alias,
                canonical,
                added,
            } => {
                // An alias was added-upserted or removed (the §6.12
                // "add row with tag autocomplete; delete; validation"
                // loop). Update the local table *in place* without a
                // round-trip `ListAliases` (the §6.12 rule that
                // "any admin action … reflected without restart" applies
                // to the alias table too). Clear the add-row draft
                // if it just committed.
                let current = self.state.settings_aliases.clone().unwrap_or_default();
                let mut next = current;
                if added {
                    if let Some(c) = &canonical {
                        if let Some(slot) = next.iter_mut().find(|r| r.alias == alias) {
                            slot.canonical = c.clone();
                        } else {
                            next.push(data::AliasRow {
                                alias: alias.clone(),
                                canonical: c.clone(),
                            });
                        }
                        next.sort_by(|a, b| a.alias.cmp(&b.alias));
                    }
                } else {
                    next.retain(|r| r.alias != alias);
                }
                self.state.settings_aliases = Some(next);
                // The draft *just committed* (it equals the alias we sent).
                if self.state.settings_alias_new.0 == alias {
                    self.state.settings_alias_new = (String::new(), String::new());
                }
            }
            data::Evt::Warn(msg) => {
                self.last_warn = Some(msg);
            }
            data::Evt::Fatal(msg) => {
                self.state.phase = crate::state::Phase::Error(msg);
            }
        }
    }
}

#[cfg(test)]
mod g6_2_tests {
    use super::*;
    use crate::ui::create;

    /// `App` with a deterministic, worker-less start state (no DB, Home launch).
    fn fresh() -> App {
        App::new(None, LaunchRoute::Home, ThemeChoice::Light)
    }

    /// The New-note dialog's open flag is idempotent and preserves drafts
    /// across a close→reopen (PLAN-GUI §6.10: a cancelled attempt doesn't
    /// lose what was typed).
    #[test]
    fn open_create_note_sets_flag_and_preserves_drafts() {
        let mut app = fresh();
        assert!(!app.state().create_open, "starts closed");

        // Simulate the user typing a draft before opening.
        app.state_mut().create_title = String::from("My title");
        app.state_mut().create_type = String::from("note");
        app.state_mut().create_tags = vec!["alpha".to_string()];

        app.open_create_note();
        assert!(app.state().create_open, "open_create_note flips the flag");
        // Idempotent: a second call (a repeat Ctrl+N) is a no-op and the
        // drafts are untouched.
        app.open_create_note();
        assert!(app.state().create_open);
        assert_eq!(app.state().create_title, "My title");
        assert_eq!(app.state().create_tags, vec!["alpha".to_string()]);
    }

    /// The conflict-diff view: `show_conflict_diff` snapshots the *current*
    /// on-disk bytes (a best-effort read), `close_conflict_diff` clears the
    /// open flag but *keeps* the snapshot for the session (so a re-open is
    /// cheap). A missing file leaves the snapshot `None` (the pane's
    /// "(disk content unavailable)" hint).
    #[test]
    fn show_conflict_diff_snapshots_disk_and_close_preserves() {
        let mut app = fresh();
        assert!(!app.state().conflict_diff_open);
        assert!(app.state().conflict_diff_snapshot.is_none());

        // No page open ⇒ a no-op (the guard returns before reading disk).
        app.show_conflict_diff();
        assert!(
            !app.state().conflict_diff_open,
            "no open page ⇒ stays closed"
        );

        // Seed an open page with a real, readable file.
        let dir = std::env::temp_dir().join(format!(
            "kb-gui-diff-snap-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("page.md");
        let disk_text = "# T\n\ncurrent disk bytes\n";
        std::fs::write(&file, disk_text).unwrap();
        let abs = file.canonicalize().unwrap().to_string_lossy().into_owned();

        app.state_mut().open_page = Some(crate::state::OpenPageState {
            abs_path: abs.clone(),
            body: Some(String::from("# T\n\nmy unsaved edits")),
            error: None,
            baseline: None,
            kind: crate::state::DocumentKind::KbPage,
            banner_dismissed: false,
        });

        app.show_conflict_diff();
        assert!(
            app.state().conflict_diff_open,
            "show_conflict_diff opens it"
        );
        assert_eq!(
            app.state().conflict_diff_snapshot.as_deref(),
            Some(disk_text),
            "snapshot = current on-disk bytes (not the editor buffer)"
        );

        app.close_conflict_diff();
        assert!(!app.state().conflict_diff_open, "close clears the flag");
        assert!(
            app.state().conflict_diff_snapshot.as_deref() == Some(disk_text),
            "close keeps the snapshot (cheap re-open)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `show_conflict_diff` on a *missing* file leaves `None` (the diff pane
    /// renders its "(disk content unavailable)" hint) but still opens.
    #[test]
    fn show_conflict_diff_missing_file_yields_none_snapshot() {
        let mut app = fresh();
        let dir = std::env::temp_dir().join(format!(
            "kb-gui-diff-miss-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // A path that canonicalises but then is removed, so the read fails.
        let probe = dir.join("gone.md");
        std::fs::write(&probe, b"x").unwrap();
        let abs = probe.canonicalize().unwrap().to_string_lossy().into_owned();
        std::fs::remove_file(&probe).unwrap();

        app.state_mut().open_page = Some(crate::state::OpenPageState {
            abs_path: abs.clone(),
            body: Some(String::from("# T\n\nmine")),
            error: None,
            baseline: None,
            kind: crate::state::DocumentKind::KbPage,
            banner_dismissed: false,
        });

        app.show_conflict_diff();
        assert!(
            app.state().conflict_diff_open,
            "opens even when disk is gone"
        );
        assert!(
            app.state().conflict_diff_snapshot.is_none(),
            "unreadable file ⇒ None snapshot"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The New-note destination directory prefers the open page's root, then
    /// the first indexed root, then `.` (the `create::resolve_directory`
    /// contract, PLAN-GUI §6.10).
    #[test]
    fn create_directory_prefers_open_page_root_then_first() {
        // Empty corpus ⇒ the cwd fallback.
        let mut app = fresh();
        assert_eq!(
            create::resolve_directory(&app),
            ".",
            "no indexed root ⇒ cwd"
        );

        // One indexed page carrying a non-empty root ⇒ that root.
        app.state_mut().pages = vec![crate::state::PageRow {
            abs_path: "/r0/note.md".into(),
            id: "note:note".into(),
            title: Some("t".into()),
            type_: "note".into(),
            rel_path: "note.md".into(),
            root: String::from("/r0"),
            gates: vec![],
            tags: vec![],
            uuid: String::new(),
            updated_at: String::new(),
        }];
        assert_eq!(
            create::resolve_directory(&app),
            "/r0",
            "first non-empty root wins"
        );

        // A leading empty root is skipped; the next non-empty root wins.
        app.state_mut().pages = vec![
            crate::state::PageRow {
                abs_path: "/x/a.md".into(),
                id: "wiki:a".into(),
                title: None,
                type_: "wiki".into(),
                rel_path: "a.md".into(),
                root: String::new(),
                gates: vec![],
                tags: vec![],
                uuid: String::new(),
                updated_at: String::new(),
            },
            crate::state::PageRow {
                abs_path: "/r2/b.md".into(),
                id: "wiki:b".into(),
                title: None,
                type_: "wiki".into(),
                rel_path: "b.md".into(),
                root: String::from("/r2"),
                gates: vec![],
                tags: vec![],
                uuid: String::new(),
                updated_at: String::new(),
            },
        ];
        assert_eq!(
            create::resolve_directory(&app),
            "/r2",
            "empty root skipped, next non-empty wins"
        );
    }
}
