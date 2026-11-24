//! The UI paint pass for `kb-gui` (PLAN-GUI §6 shell layout).
//!
//! `paint` dispatches on the app's `Phase` + launch-route + selection,
//! painting the chrome + panes with proper egui panels:
//!
//! - `Panel::top` → `chrome::top_bar`
//! - `Panel::bottom` → `chrome::status_bar`
//! - `Panel::left("kb.left")` → a *single* tabbed panel hosting either the
//!   collections rail (`rail::paint`) or the note list (`list::paint`),
//!   switched via a two-tab bar; only the active tab is visible
//!   (every Ready state — Welcome/Home show zero-count empty states)
//! - `Panel::right("kb.outline")` → `outline::paint` (only when a page is open)
//! - `CentralPanel` → Welcome / Home / `preview::paint` / `editor::paint`
//!
//! The panel-based layout gives a deterministic width partition: each
//! panel claims its size from the window, leaving whatever remains to the
//! `CentralPanel`. This replaces the previous `ui.horizontal_top` approach
//! (which left dead columns and overlapping panes because `horizontal_top`
//! does not claim width deterministically).
//!
//! The `Cmd+K` overlay, the frontmatter inspector, and the convert dialog
//! are `egui::Window`s that paint over the shell — their `paint_if_open`
//! no-ops when the feature is closed.

pub(crate) mod block_edit;
pub(crate) mod chrome;
pub(crate) mod convert;
pub(crate) mod editor;
pub(crate) mod home;
pub(crate) mod inspector;
pub(crate) mod list;
pub(crate) mod md;
pub(crate) mod outline;
pub(crate) mod preview;
pub(crate) mod rail;
pub(crate) mod render;
pub(crate) mod search;
pub(crate) mod settings;
pub(crate) mod splice;
pub(crate) mod welcome;

use eframe::egui;

use crate::app::App;
use crate::state::Phase;
use egui::containers::{CentralPanel, Panel};

/// The single consolidated left pane: a two-tab bar (`Collections` /
/// `Notes`) above whichever panel is active. Clicking the inactive tab
/// switches it. This replaces the earlier side-by-side rail + list panels.
///
/// The active pane claims the remainder of the panel's height: `list::paint`
/// wraps its rows in its own `ScrollArea` (header stays pinned, rows scroll)
/// while `rail::paint` is a short plain stack — so we do *not* add a second,
/// enclosing scroll area here (that would nest scrollbars and pin the list
/// header away from the panel top).
fn left_pane(app: &mut App, ui: &mut egui::Ui) {
    let active = app.state().active_left_pane;
    ui.horizontal(|ui| {
        if ui
            .selectable_label(active == crate::state::LeftPaneKind::Rail, "Collections")
            .clicked()
            && active != crate::state::LeftPaneKind::Rail
        {
            app.state_mut().active_left_pane = crate::state::LeftPaneKind::Rail;
        }
        if ui
            .selectable_label(active == crate::state::LeftPaneKind::List, "Notes")
            .clicked()
            && active != crate::state::LeftPaneKind::List
        {
            app.state_mut().active_left_pane = crate::state::LeftPaneKind::List;
        }
    });
    ui.separator();
    match active {
        crate::state::LeftPaneKind::Rail => rail::paint(ui, app),
        crate::state::LeftPaneKind::List => list::paint(ui, app),
    }
}

/// The main paint entry point (called from `App::ui` each frame).
pub fn paint(app: &mut App, ui: &mut egui::Ui) {
    let phase = app.state().phase.clone();
    match &phase {
        Phase::Loading => {
            // Panels always paint so the window has stable chrome even
            // mid-load (the 500 ms first-frame budget check).
            Panel::top("kb.top").show(ui, |ui| chrome::top_bar(ui, app));
            Panel::bottom("kb.bottom").show(ui, |ui| chrome::status_bar(ui, app));
            CentralPanel::default().show(ui, |ui| {
                ui.strong("kb");
                ui.add_space(8.0);
                ui.weak("loading corpus…");
            });
        }

        Phase::Ready => {
            let has_selection = app.state().selected_page.is_some();

            // Top bar always paints (the app's identity is stable across
            // Welcome/Home/browse states).
            Panel::top("kb.top").show(ui, |ui| chrome::top_bar(ui, app));

            // Side panels — a single tabbed left panel (collections rail OR
            // note list, switched via a small tab bar). Painted for every
            // Ready state; in Welcome/Home both tabs show empty-state
            // content (no roots / no pages yet). Only the active tab is
            // visible (the two panels used to paint side-by-side and
            // consumed too much width).
            Panel::left("kb.left")
                .default_size(300.0)
                .min_size(200.0)
                .max_size(460.0)
                .show(ui, |ui| {
                    left_pane(app, ui);
                });
            // Outline: only meaningful once a page is open (a Welcome or
            // unselected Home view has no body to outline).
            if has_selection {
                let page = app.state().open_page.clone();
                let body = page.as_ref().and_then(|p| p.body.clone());
                Panel::right("kb.outline")
                    .default_size(220.0)
                    .min_size(140.0)
                    .max_size(340.0)
                    .show(ui, |ui| outline::paint(ui, &page, &body));
            }

            // Bottom status bar paints for every Ready state (the app's
            // save-state + row-count line is part of the stable chrome).
            Panel::bottom("kb.bottom").show(ui, |ui| chrome::status_bar(ui, app));

            // Central area: Welcome / Home / preview / editor / browse.
            CentralPanel::default().show(ui, |ui| {
                let has_sel = app.state().selected_page.is_some();
                let launch_home = app.state().launch.is_home();
                let has_roots = app.state().roots_known;

                if !has_sel && launch_home && !has_roots {
                    welcome::paint(ui, app);
                } else if !has_sel && launch_home {
                    home::paint(ui, app);
                } else {
                    let mode = app.state().view_mode;
                    if mode == crate::state::ViewMode::Raw {
                        // G2: the raw view is the editable editor (§6.15 banner
                        // is painted by the preview path; the editor carries its
                        // own Conflict banner).
                        editor::paint(app, ui);
                    } else {
                        // G3: takes `app` (not just `ui`) so the §6.15
                        // conversion/add-root banner can dispatch its actions.
                        preview::paint(app, ui);
                    }
                }
            });

            // Overlays — paint last so they sit above the shell. The
            // `Window`s paint on their own layer (no need to reserve space
            // in the panels above).
            search::paint_if_open(app, ui);
            inspector::paint_if_open(app, ui);
            convert::paint_if_open(app, ui);
            // The Settings modal (PLAN-GUI §6.12) — last in the paint
            // order so it sits above every other overlay (search,
            // inspector, convert). Its `paint_if_open` is a no-op when
            // the modal is closed.
            settings::paint_if_open(app, ui);
        }

        Phase::Error(msg) => {
            Panel::top("kb.top").show(ui, |ui| chrome::top_bar(ui, app));
            Panel::bottom("kb.bottom").show(ui, |ui| chrome::status_bar(ui, app));
            CentralPanel::default().show(ui, |ui| {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("fatal: {msg}"));
                ui.add_space(8.0);
                ui.weak("the corpus could not be loaded (DB open / migration failed)");
            });
        }
    }
}
