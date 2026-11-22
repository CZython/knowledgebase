//! The UI paint pass for `kb-gui` (PLAN-GUI §6 shell layout).
//!
//! `paint` dispatches on the app's `Phase` + launch-route + selection,
//! painting the chrome + panes with proper egui panels:
//!
//! - `Panel::top` → `chrome::top_bar`
//! - `Panel::bottom` → `chrome::status_bar`
//! - `Panel::left("kb.rail")` → `rail::paint` (only in browse/page)
//! - `Panel::left("kb.list")` → `list::paint` (only in browse/page)
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
pub(crate) mod splice;
pub(crate) mod welcome;

use eframe::egui;

use crate::app::App;
use crate::state::Phase;
use egui::containers::{CentralPanel, Panel};

/// `true` when the shell should paint the browse panes (rail + list).
/// Welcome has no roots and no selection — it owns the full central rect.
/// Home (roots but no selection) and Browse (selection) both paint panes.
fn browseable(app: &App) -> bool {
    app.state().selected_page.is_some() || app.state().roots_known
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
            let browse = browseable(app);
            let has_selection = app.state().selected_page.is_some();

            // Top bar always paints (the app's identity is stable across
            // Welcome/Home/browse states).
            Panel::top("kb.top").show(ui, |ui| chrome::top_bar(ui, app));

            // Browse panes (rail + list) — skip in Welcome/Home: those two
            // states need the full central rect for their layout.
            if browse {
                Panel::left("kb.rail")
                    .default_size(220.0)
                    .min_size(160.0)
                    .max_size(340.0)
                    .show(ui, |ui| rail::paint(ui, app));
                Panel::left("kb.list")
                    .default_size(340.0)
                    .min_size(260.0)
                    .max_size(500.0)
                    .show(ui, |ui| list::paint(ui, app));
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
