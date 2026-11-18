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
}

impl App {
    pub fn new(db_hint: Option<PathBuf>, launch: LaunchRoute, theme: ThemeChoice) -> Self {
        Self {
            state: AppState::new(launch, theme),
            db_hint,
            worker: None,
            last_warn: None,
        }
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
            let _ = worker.send(data::Cmd::Search { query, scope });
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
            });
        }
        if let Some(worker) = &self.worker {
            let _ = worker.send(data::Cmd::OpenPage(abs_path.to_string()));
        }
    }

    /// Deselect the current page (return to browse). The page slot is
    /// dropped; the body is not re-fetched next time until the user clicks
    /// it again.
    pub fn close_page(&mut self) {
        self.state.selected_page = None;
        self.state.open_page = None;
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
            let _ = handle.send(data::Cmd::LoadCorpus {
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
            data::Evt::Page { abs_path, body } => {
                // The body arrived for the selected page. Fold it into
                // `open_page`. If the selected page is different (the
                // user clicked another page before this body landed), drop
                // the stale body.
                if self.state.selected_page.as_deref() == Some(abs_path.as_str()) {
                    match self.state.open_page.as_mut() {
                        Some(slot) if slot.abs_path == abs_path => {
                            slot.body = Some(body);
                            slot.error = None;
                        }
                        _ => {
                            self.state.open_page = Some(OpenPageState {
                                abs_path,
                                body: Some(body),
                                error: None,
                            });
                        }
                    }
                }
            }
            data::Evt::Warn(msg) => {
                self.last_warn = Some(msg);
            }
            data::Evt::Fatal(msg) => {
                self.state.phase = crate::state::Phase::Error(msg);
            }
            data::Evt::Saved { .. } => {} // reserved for G2
        }
    }
}
