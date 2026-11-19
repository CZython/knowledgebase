//! Chrome: the top bar and status bar (PLAN-GUI §§6.2 / 6.7).
//!
//! G1 carries the functional surface: app menu button, current-page title
//! (or `no page open`), an active-collection scope chip (§6.2), the search
//! shortcut button, the Formatted/Raw toggle, and the status-bar phase /
//! save line + selected-path / row-count line.
//!
//! The menu (`☰`) is wired to the App menu in G1 (New note, Add root,
//! Refresh, Full rebuild, Theme ▸, Aliases, Settings, Quit); in G1 it is
//! a button that triggers `Add root` when clicked. The full menu + custom
//! theme switching land in G5 / G6 (PLAN-GUI §6.12 / §11).

use eframe::egui;

use crate::app::App;
use crate::state::{Phase, ViewMode};

/// The top bar (height ~40 px, PLAN-GUI §6.2).
///
/// Layout: left = `☰` + `kb` + (scope chip); center = current page title
/// (or `no page open` in italics); right = search shortcut +
/// Formatted/Raw toggle.
pub fn top_bar(ui: &mut egui::Ui, app: &mut App) {
    ui.set_min_height(40.0);
    ui.horizontal(|ui| {
        // Left: menu + app name + active-collection scope chip.
        if ui.button("☰").clicked() {
            // G1 wires the menu to `Add root` (the one action available
            // without a page open). G5/G6 adds the full App menu.
            let _ = crate::ui::welcome::picker_add_root(app);
        }
        ui.strong("kb");

        // Back button: visible only when a page is open; returns the shell
        // to the browse list (resets the selection + open-page slot).
        let has_selection = app.state().selected_page.is_some();
        if has_selection && ui.button("← back").clicked() {
            app.close_page();
        }

        // Active-collection chip sits next to the app name when a non-All
        // rail key is selected (PLAN-GUI §6.2: "when a collection is
        // active, a scope chip (in: Electronics) sits next to it").
        let collection = app.state().collection.clone();
        if !matches!(&collection, crate::state::CollectionKey::All) {
            ui.separator();
            if ui
                .selectable_label(false, format!("in: {}", collection.label()))
                .clicked()
            {
                // Clicking the scope chip opens the search scoped to that
                // collection (the "Search in this collection" rail context
                // action, §6.3).
                crate::app::open_search(app, None);
            }
        }

        // Middle: the current page title (or the no-page hint). The title
        // is `ui.strong` when something is selected; otherwise a muted
        // `no page open` so the chrome has a stable structure.
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            let selected = app
                .state()
                .selected_page
                .as_ref()
                .and_then(|abs| app.state().pages.iter().find(|p| &p.abs_path == abs));
            match selected {
                Some(row) => {
                    let title = row.title.clone().unwrap_or_else(|| row.abs_path.clone());
                    ui.strong(title);
                    ui.weak(format!(" — {}", row.rel_path));
                }
                None => {
                    ui.weak("no page open");
                }
            }
        });

        // Right: Properties + search shortcut + Formatted/Raw toggle.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Properties button (G2, §6.9): toggles the frontmatter
            // inspector. Visible only when a page is open.
            if has_selection {
                let pending =
                    app.state().inspector.open && app.inspector_pending_summary().is_some();
                let label = if pending {
                    "⚙ properties ●"
                } else {
                    "⚙ properties"
                };
                if ui.button(label).clicked() {
                    app.toggle_inspector();
                }
                ui.separator();
            }
            // Toggle Formatted/Raw. G1 reads `app.state().view_mode`; the
            // toggle flips it (the preview re-renders next frame).
            let mode = app.state().view_mode;
            let formatted_ok = ui
                .selectable_label(mode == ViewMode::Formatted, "Formatted")
                .clicked();
            let raw_ok = ui.selectable_label(mode == ViewMode::Raw, "Raw").clicked();
            if formatted_ok {
                app.state_mut().view_mode = ViewMode::Formatted;
            }
            if raw_ok {
                app.state_mut().view_mode = ViewMode::Raw;
            }
            ui.separator();
            if ui.button("🔎 search").clicked() {
                crate::app::open_search(app, None);
            }
        });
    });
}

/// The status bar (~24 px). Phase + row count + selected-page path / recent
/// error / last warning.
///
/// In `Ready` + browse, the status bar shows the row count and, when a
/// collection is active, `in <collection>`; it appends the selected page's
/// rel path. In Welcome/Home it shows the corpus count; in `Loading` it
/// shows `loading…`; in `Error` it shows the message.
pub fn status_bar(ui: &mut egui::Ui, app: &App) {
    ui.set_min_height(24.0);
    ui.horizontal(|ui| {
        match &app.state().phase {
            Phase::Loading => {
                ui.weak(format!(
                    "loading… {} page(s) so far",
                    app.state().pages.len()
                ));
            }
            Phase::Ready => {
                // Count + scope chip (PLAN-GUI §6.4 footer: "N notes" /
                // "N in <collection>").
                let pages_len = app.state().pages.len();
                let filtered_len = crate::ui::list::visible_count(app);
                ui.strong(format!("{filtered_len} / {pages_len} page(s)"));
                let collection = &app.state().collection;
                if !matches!(collection, crate::state::CollectionKey::All) {
                    ui.weak(format!(" · in {}", collection.label()));
                }
                if !app.state().roots_known {
                    ui.weak(" · no roots registered");
                }

                // Save-state chip (G2 editor footer): only meaningful when an
                // open document exists. `Saved` is rendered in the
                // editor header, so the status bar surfaces the three
                // transient states (Unsaved / Saving / Conflict).
                if app.state().open_page.is_some() {
                    let save_status = app.state().save_status;
                    match save_status {
                        crate::data::SaveStatus::Saved => {}
                        crate::data::SaveStatus::Unsaved => {
                            ui.colored_label(egui::Color32::from_rgb(200, 160, 60), " · unsaved");
                        }
                        crate::data::SaveStatus::Saving => {
                            ui.colored_label(egui::Color32::from_rgb(120, 160, 220), " · saving…");
                        }
                        crate::data::SaveStatus::Conflict => {
                            ui.colored_label(egui::Color32::LIGHT_RED, " · CONFLICT");
                        }
                    }
                }

                // Right side: the selected page's path + any last warning.
                if let Some(abs) = &app.state().selected_page {
                    if let Some(row) = app.state().pages.iter().find(|p| &p.abs_path == abs) {
                        ui.weak(format!(" · {}", row.rel_path));
                    }
                }
            }
            Phase::Error(msg) => {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("error: {msg}"));
            }
        }

        // Most-recent warning (worker `Evt::Warn`) — rendered on the
        // right, in muted red so it does not compete with the count.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(warning) = &app.last_warning() {
                ui.colored_label(egui::Color32::LIGHT_RED, format!(" · {warning}"));
            }
        });
    });
}
