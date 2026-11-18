//! The UI paint pass for `kb-gui` (PLAN-GUI §6 shell layout).
//!
//! `paint` dispatches on the app's `Phase` + launch-route + selection:
//!
//! | launch / selection  | pane(s) painted                       |
//! |----------------------|----------------------------------------|
//! | Home + no roots      | Welcome (§6.14)                        |
//! | Home + roots         | Home (§6.13)                           |
//! | Open* or list click  | browse shell (rail + list) + preview  |
//!
//! The top bar and status bar paint for *every* Ready state so the app's
//! identity is stable even on the Welcome/Home screens (`chrome`).
//!
//! ## Why `vertical_centered_justified` + `horizontal_top`
//!
//! In egui 0.37, `ui.horizontal` allocates a row the fixed height of
//! `style.spacing.interact_size.y` (≈15 px) — it assumes a single row and
//! does not grow to the parent's height; `ui.vertical` is shrink-to-fit.
//! So `vertical` → `horizontal` leaves ~640 px of a 1200×800 window blank.
//!
//! `vertical_centered_justified` and `horizontal_top` are the two 0.37
//! primitives that claim the full available rect ("the new layout will take
//! up all available space"):
//!
//! - `vertical_centered_justified` = top-to-bottom, children stretched to
//!   the full cross-axis width (and the parent claims all height). The
//!   bands stack full-width.
//! - `horizontal_top` = left-to-right, children top-aligned, but the row
//!   itself claims all available height — so the panes' ScrollAreas get the
//!   full vertical space they need to actually scroll.

pub(crate) mod chrome;
pub(crate) mod home;
pub(crate) mod list;
pub(crate) mod outline;
pub(crate) mod preview;
pub(crate) mod rail;
pub(crate) mod search;
pub(crate) mod welcome;

use eframe::egui;

use crate::app::App;
use crate::state::Phase;

/// The main paint entry point (called from `App::ui` each frame).
pub fn paint(app: &mut App, ui: &mut egui::Ui) {
    // Read the phase into an owned local *before* painting so the match
    // scrutinee does not hold an immutable borrow on `app` across the arms
    // (the arms call `chrome::top_bar(ui, app)`, which needs `&mut app`).
    let phase = app.state().phase.clone();
    match &phase {
        Phase::Loading => {
            // Skeleton (chrome + placeholder) so the 500 ms first-frame
            // budget (PLAN-GUI §13) has something painted even if the worker
            // is slow. The Welcome/Home split is unknown yet (`roots_known`
            // is still the initial `false`).
            chrome::top_bar(ui, app);
            ui.separator();
            ui.vertical_centered_justified(|ui| {
                ui.strong("kb");
                ui.add_space(8.0);
                ui.weak("loading corpus…");
            });
            ui.separator();
            chrome::status_bar(ui, app);
        }

        Phase::Ready => {
            let launch_home = app.state().launch.is_home();
            let has_selection = app.state().selected_page.is_some();
            let has_roots = app.state().roots_known;

            chrome::top_bar(ui, app);
            ui.separator();

            if launch_home && !has_roots {
                welcome::paint(ui, app);
            } else if launch_home && !has_selection {
                home::paint(ui, app);
            } else {
                ui.horizontal_top(|ui| {
                    rail::paint(ui, app);
                    ui.separator();
                    list::paint(ui, app);
                    ui.separator();
                    // The open page (if any) renders in the preview + outline
                    // panes. When nothing is selected the panes paint their
                    // own placeholder, so the shell layout stays stable.
                    let page = app.state().open_page.clone();
                    let body = page.as_ref().and_then(|p| p.body.clone());
                    let mode = app.state().view_mode;
                    preview::paint(ui, &page, mode);
                    ui.separator();
                    outline::paint(ui, &page, &body);
                });
            }

            ui.separator();
            chrome::status_bar(ui, app);

            // The `Cmd+K` overlay paints on top of the shell above (no-op
            // when closed — `SearchView::open` is the toggle's source of
            // truth).
            search::paint_if_open(app, ui);
        }

        Phase::Error(msg) => {
            chrome::top_bar(ui, app);
            ui.separator();
            ui.vertical_centered_justified(|ui| {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("fatal: {msg}"));
                ui.add_space(8.0);
                ui.weak("the corpus could not be loaded (DB open / migration failed)");
            });
            ui.separator();
            chrome::status_bar(ui, app);
        }
    }
}
