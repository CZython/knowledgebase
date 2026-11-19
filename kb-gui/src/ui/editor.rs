//! The raw Markdown editor (PLAN-GUI §6.5 / G2).
//!
//! A full-height, full-width `TextEdit::multiline` with line numbers in the
//! gutter. The text buffer is the *source of truth* for the open page's body;
//! every keystroke that mutates the text sets `save_status = Unsaved` via
//! `App::mark_unsaved`. A `Save` button (and `Ctrl/Cmd+S`) calls
//! `App::save_page`; the worker's `Saved` / `Conflict` event flips the state
//! and the status bar updates accordingly.
//!
//! A **Create** button in the editor header opens a small inline form
//! (title + type) that calls `App::create_page`; the worker's `Created`
//! event auto-opens the new file in the editor.
//!
//! A **Conflict** banner is painted above the editor when `save_status` is
//! `Conflict`, with `Reload` (`Cmd::OpenPage`) and `Keep-mine`
//! (force `save_page`) buttons.

use eframe::egui;

use crate::app::App;
use crate::data::SaveStatus;

/// Paint the editor pane (replaces `preview::paint` when the view is `Raw`
/// or when the user explicitly entered the editor).
pub fn paint(app: &mut App, ui: &mut egui::Ui) {
    ui.set_min_width(400.0);
    ui.set_height(f32::INFINITY);

    // Header bar: save-state indicator + Save / Create buttons.
    let save_status = app.state().save_status;
    ui.horizontal(|ui| {
        // Save-state chip.
        let (label, color) = match save_status {
            SaveStatus::Saved => ("saved", egui::Color32::from_rgb(87, 167, 115)),
            SaveStatus::Unsaved => ("unsaved", egui::Color32::from_rgb(200, 160, 60)),
            SaveStatus::Saving => ("saving…", egui::Color32::from_rgb(120, 160, 220)),
            SaveStatus::Conflict => ("CONFLICT", egui::Color32::LIGHT_RED),
        };
        ui.colored_label(color, label);

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            if ui.button("Create").clicked() {
                app.state_mut().create_open = true;
            }
            if ui.button("Save  (Ctrl+S)").clicked() && save_status == SaveStatus::Unsaved {
                app.save_page();
            }
        });
    });

    // Conflict banner.
    if save_status == SaveStatus::Conflict {
        ui.horizontal(|ui| {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                "The file changed on disk since this editor session.",
            );
            ui.separator();
            if ui.button("Reload from disk").clicked() {
                // Re-fetch the page from the worker.
                if let Some(abs) = app.state().selected_page.clone() {
                    app.open_page(&abs);
                }
            }
            if ui.button("Keep mine (write anyway)").clicked() {
                // Force a save: clear the conflict and re-dispatch.
                app.state_mut().save_status = SaveStatus::Unsaved;
                app.save_page();
            }
        });
        ui.separator();
    }

    // The "create new page" inline form.
    if app.state().create_open {
        paint_create_form(app, ui);
        ui.separator();
    }

    // The editor buffer. Clone the data out of `app` *before* the closure
    // below, which needs `&mut app` (to write the edit back); holding a
    // borrow of `app.state()` across the closure is a borrow-checker error.
    let Some(body) = app.state().open_page.as_ref().and_then(|s| s.body.clone()) else {
        ui.weak("no page open yet — or the body is still loading");
        return;
    };

    // Track the pre-edit text to detect mutations.
    let pre_text = body.clone();

    // Build a line-numbered layout: a narrow gutter `Column` + the `TextEdit`.
    ui.horizontal(|ui| {
        ui.set_min_height(600.0);
        // Line-number gutter.
        let line_count = body.lines().count().max(1);
        egui::ScrollArea::vertical()
            .id_salt("gutter")
            .show(ui, |ui| {
                ui.set_min_width(40.0);
                for i in 1..=line_count {
                    ui.label(i.to_string());
                }
            });
        ui.separator();
        // The editor. `TextEdit` mutates `text` (a `String`, which implements
        // `TextBuffer`) in place every frame as the user types; we commit back
        // to the slot (and mark dirty) only when the text actually changed
        // since the last frame.
        let mut text = body.to_string();
        egui::TextEdit::multiline(&mut text)
            .code_editor()
            .desired_width(f32::INFINITY)
            .desired_rows(40)
            .show(ui);
        // Commit: if the text changed, mark dirty + write back to the slot.
        if text != pre_text {
            if let Some(slot_mut) = app.state_mut().open_page.as_mut() {
                slot_mut.body = Some(text);
            }
            app.mark_unsaved();
        }
    });
}

/// A compact inline form for creating a new note (PLAN-GUI §6.5 "New note
/// dialog"; G2 keeps it inline rather than a modal window to avoid the extra
/// eframe viewport plumbing). Title + type. On Create it dispatches
/// `Cmd::CreatePage` via `App::create_page` and closes the form; the worker's
/// `Created` event auto-opens the new file in the editor.
fn paint_create_form(app: &mut App, ui: &mut egui::Ui) {
    ui.strong("New note");
    ui.horizontal(|ui| {
        ui.label("title:");
        let mut title = app.state().create_title.clone();
        ui.add(
            egui::TextEdit::singleline(&mut title)
                .hint_text("e.g. Energy in electronics")
                .desired_width(320.0),
        );
        app.state_mut().create_title = title;

        ui.label("type:");
        let mut page_type = app.state().create_type.clone();
        egui::ComboBox::from_id_salt("create-type")
            .selected_text(page_type.clone())
            .show_ui(ui, |ui| {
                for candidate in ["wiki", "note", "label"] {
                    if ui
                        .selectable_label(page_type == candidate, candidate)
                        .clicked()
                    {
                        page_type = candidate.into();
                    }
                }
            });
        app.state_mut().create_type = page_type;

        if ui.button("Create").clicked() && !app.state().create_title.trim().is_empty() {
            // Pick the default root directory (the first registered root;
            // G5 lets the user pick which root explicitly).
            let directory = app
                .state()
                .pages
                .iter()
                .find(|p| !p.root.is_empty())
                .map(|p| p.root.clone())
                .unwrap_or_else(|| ".".to_string());
            let title = app.state().create_title.trim().to_string();
            let page_type = app.state().create_type.clone();
            app.create_page(directory, title, page_type, Vec::new(), Vec::new());
            app.state_mut().create_open = false;
            app.state_mut().create_title = String::new();
        }
    });
}
