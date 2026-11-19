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

use std::path::PathBuf;
use std::sync::OnceLock;

use eframe::egui;

use crate::data::{self, WorkerHandle};
use crate::state::{AppState, LaunchRoute, OpenPageState, ThemeChoice, ViewMode};

/// The top-level application (one per window).
pub struct App {
    state: AppState,
    /// The DB hint (`--db` or `None` → default location) captured at startup.
    db_hint: Option<PathBuf>,
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
        Self {
            state: AppState::new(launch, theme),
            db_hint,
            worker: None,
            last_warn: None,
            max_frames: 0,
            frames: 0,
        }
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
            });
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
}

/// Apply a built-in theme to the egui context. G0 supports three choices;
/// custom TOML themes + live switching land in G6 (PLAN-GUI §11).
pub fn apply_theme(cc: &eframe::CreationContext, theme: ThemeChoice) {
    let visuals = match theme {
        ThemeChoice::Light => egui::Visuals::light(),
        ThemeChoice::Dark | ThemeChoice::System => egui::Visuals::dark(),
    };
    cc.egui_ctx.set_visuals(visuals);
}

/// Global entry point so the `ui` modules can call `open_search` without
/// re-borrowing the `App` (the closure in `paint` already holds `&mut App`).
pub fn open_search(app: &mut App, initial_query: Option<String>) {
    app.open_search(initial_query);
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
            } => {
                // The corpus (and its collection facets + visibility counts)
                // just arrived. Store it all; the rail and the list read
                // straight off these fields.
                self.state.roots_known = roots_known;
                self.state.collections = collections;
                self.state.gated_count = gated_count;
                self.state.ungated_count = ungated_count;
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
                // page. Fold into `open_page`. If the selected page is
                // different (the user clicked another page before this body
                // landed), drop the stale body.
                if self.state.selected_page.as_deref() == Some(abs_path.as_str()) {
                    match self.state.open_page.as_mut() {
                        Some(slot) if slot.abs_path == abs_path => {
                            slot.body = Some(body);
                            slot.error = None;
                            slot.baseline = Some(baseline);
                        }
                        _ => {
                            self.state.open_page = Some(OpenPageState {
                                abs_path,
                                body: Some(body),
                                error: None,
                                baseline: Some(baseline),
                            });
                        }
                    }
                    // A fresh load resets the document to clean.
                    self.state.save_status = data::SaveStatus::default();
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
            data::Evt::Warn(msg) => {
                self.last_warn = Some(msg);
            }
            data::Evt::Fatal(msg) => {
                self.state.phase = crate::state::Phase::Error(msg);
            }
        }
    }
}
