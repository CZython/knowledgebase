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
///
/// `vertical_centered` is top-down with each child centered horizontally as
/// a unit, and — unlike `horizontal_centered` — it never reserves the
/// remaining panel height, so the content stays directly under the heading
/// instead of being vertically stranded mid-screen.
pub fn paint(ui: &mut egui::Ui, app: &mut App) {
    ui.vertical_centered(|ui| {
        ui.add_space(24.0);
        ui.strong("Welcome!");
        ui.add_space(12.0);

        ui.weak("Add a folder as your first root so the pages in it become browsable.");
        ui.add_space(8.0);
        ui.weak("1. Pick a folder containing Markdown notes.");
        ui.weak("2. Add it as a root — kb indexes the pages it finds.");
        ui.weak("3. Remove it any time in Settings → Roots. Files are never deleted.");
        ui.add_space(12.0);
        ui.weak("Only Markdown with a valid `type:` becomes a page.");
        ui.weak("Other .md files can still be opened and converted later.");

        ui.add_space(24.0);
        let (r1, r2) = centered_row(
            ui,
            || egui::Button::new("Add folder as root…"),
            || egui::Button::new("Open a Markdown file…"),
        );
        if r1.clicked() {
            picker_add_root(app);
        }
        if r2.clicked() {
            picker_open_file(app);
        }

        ui.add_space(12.0);
        centered_row(
            ui,
            || egui::Label::new(egui::RichText::new("Files are never deleted.").weak()),
            || {
                egui::Label::new(
                    egui::RichText::new(
                        "kb indexes them in your `kb.db` for search + link resolution.",
                    )
                    .weak(),
                )
            },
        );
    });
}

/// Lay out a two-item row (left-to-right) with the *group* centered
/// horizontally in the surrounding container, at natural height.
///
/// egui has no "center a *group* of widgets along the main axis" helper —
/// a scope's children always start from the container's left edge (which is
/// why single-widget lines center but a visible `ui.horizontal` scope does
/// not) — so we measure each item's size in an invisible probe scope
/// (invisible so the probe paints nothing; a visible probe would
/// double-paint) and then `ui.place` the real widgets into hand-computed,
/// centered rects. The row's slot is claimed in the parent so the cursor
/// advances past exactly the row height — no leftover space reserved.
fn centered_row<W: egui::Widget>(
    ui: &mut egui::Ui,
    make_first: impl Fn() -> W,
    make_second: impl Fn() -> W,
) -> (egui::Response, egui::Response) {
    let (s1, s2) = ui
        .scope_builder(
            egui::UiBuilder::new()
                .layout(egui::Layout::top_down(egui::Align::Min))
                .invisible(),
            |probe| {
                let r1 = probe.add(make_first()).rect;
                let r2 = probe.add(make_second()).rect;
                (r1.size(), r2.size())
            },
        )
        .inner;
    let gap = ui.spacing().item_spacing.x;
    let row_w = s1.x + gap + s2.x;
    let slot_left = ui.cursor().left() + (ui.available_width() - row_w).max(0.0) / 2.0;
    let slot_top = ui.cursor().top();
    let slot_h = s1.y.max(s2.y);
    // Claim the row's bounding box in the parent so the cursor advances
    // past exactly the row height — no leftover space reserved.
    ui.advance_cursor_after_rect(egui::Rect::from_min_size(
        egui::pos2(ui.cursor().left(), slot_top),
        egui::vec2(ui.available_width(), slot_h),
    ));
    // Left-anchor each item so item 2 starts exactly after item 1 + gap;
    // center the row vertically around the slot mid-line so taller/shorter
    // items align cleanly.
    let cy = slot_top + slot_h / 2.0;
    let rect1 = egui::Rect::from_min_size(egui::pos2(slot_left, cy - s1.y / 2.0), s1);
    let rect2 = egui::Rect::from_min_size(egui::pos2(slot_left + s1.x + gap, cy - s2.y / 2.0), s2);
    (
        ui.place(rect1, make_first()),
        ui.place(rect2, make_second()),
    )
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
            worker.send(Cmd::AddRoot(path));
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
