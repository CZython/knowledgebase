//! Welcome (PLAN-GUI §6.14): the first-run screen shown when the app
//! launches with no path and the `roots` table is empty.
//!
//! G1 paints the short tutorial, the primary "Add folder as root…" action
//! (native directory picker → `Cmd::AddRoot` → corpus reload → Home), the
//! secondary "Open Markdown file…" (native file picker), and the
//! "Learn more" link (opens the in-app help; reserved — G5 wires it).

use eframe::egui;

use crate::app::App;
use crate::data::Cmd;

/// Paint the Welcome screen into the given Ui. The caller (`ui/mod.rs`)
/// has already painted the chrome.
pub fn paint(ui: &mut egui::Ui, app: &mut App) {
    ui.vertical_centered_justified(|ui| {
        ui.strong("Welcome!");
        ui.add_space(16.0);

        ui.weak("Add a folder as your first root so the pages in it become browsable.");
        ui.add_space(8.0);
        ui.weak("1. Pick a folder containing Markdown notes.");
        ui.weak("2. Add it as a root — kb indexes the pages it finds.");
        ui.weak("3. Remove it any time in Settings → Roots. Files are never deleted.");
        ui.add_space(12.0);
        ui.weak("Only Markdown with a valid `type:` becomes a page.");
        ui.weak("Other .md files can still be opened and converted later.");
        ui.add_space(20.0);

        ui.horizontal_centered(|ui| {
            if ui.button("Add folder as root…").clicked() {
                picker_add_root(app);
            }
            ui.add_space(8.0);
            if ui.button("Open a Markdown file…").clicked() {
                picker_open_file(app);
            }
        });

        ui.add_space(8.0);
        ui.horizontal_centered(|ui| {
            ui.weak("Files are never deleted.");
            ui.add_space(8.0);
            ui.weak("kb indexes them in your `kb.db` for search + link resolution.");
        });
    });
}

/// Native folder picker → `Cmd::AddRoot` through the worker (then a corpus
/// reload via the worker dispatch). `app` is borrowed mutably so the
/// worker handle can be sent. Returns true if a folder was chosen and
/// sent to the worker (the success/failure event arrives on the next
/// frame as `Evt::Corpus` or `Evt::Warn`).
pub fn picker_add_root(app: &mut App) -> bool {
    let Some(worker) = app.worker() else {
        return false;
    };
    let folder = rfd::FileDialog::new()
        .set_title("Choose a folder to add as a root")
        .pick_folder();
    match folder {
        Some(path) => {
            let _ = worker.send(Cmd::AddRoot(path));
            true
        }
        None => false,
    }
}

/// Native file picker → open the chosen Markdown file.
pub fn picker_open_file(app: &mut App) -> bool {
    let file = rfd::FileDialog::new()
        .set_title("Choose a Markdown file to open")
        .add_filter("Markdown", &["md", "markdown"])
        .pick_file();
    match file {
        Some(path) => {
            let abs = path.to_string_lossy().into_owned();
            app.open_page(&abs);
            true
        }
        None => false,
    }
}
