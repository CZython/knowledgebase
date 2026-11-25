//! The conflict **Show diff** view (PLAN-GUI §6.10 "Conflict").
//!
//! A two-pane `egui::Window` (left = my uncommitted edits, right = the file's
//! *current on-disk* content captured at "Show diff" time). This is a
//! **line-by-line parallel** view, not a true diff algorithm: both panes are
//! the full text, side by side, with monospace + a line-number gutter so the
//! user can visually track where the content diverged. (A full LCS diff is
//! out of scope for a conflict banner; the goal is a decision point —
//! Reload, Keep-mine, or inspect — not a merge tool.)
//!
//! The left pane reads `open_page.body` (the editor's live buffer), so it
//! always reflects what "Keep mine" would write. The right pane reads
//! `conflict_diff_snapshot` (frozen at click time). Esc closes.

use eframe::egui;

use crate::app::App;

/// Paint the diff `Window` when `conflict_diff_open` (no-op otherwise).
pub fn paint_if_open(app: &mut App, ui: &mut egui::Ui) {
    if !app.state().conflict_diff_open {
        return;
    }
    if ui.input(|i| i.key_released(egui::Key::Escape)) {
        app.close_conflict_diff();
        return;
    }
    egui::Window::new("Conflict — showing diff")
        .collapsible(false)
        .resizable(true)
        .default_width(1100.0)
        .default_height(640.0)
        .min_width(700.0)
        .show(ui.ctx(), |ui| {
            paint_panel(ui, app);
        });
}

fn paint_panel(ui: &mut egui::Ui, app: &mut App) {
    let mine = app
        .state()
        .open_page
        .as_ref()
        .and_then(|p| p.body.clone())
        .unwrap_or_default();
    let disk = app.state().conflict_diff_snapshot.clone();

    ui.colored_label(
        egui::Color32::from_rgb(200, 160, 60),
        "Side-by-side: your uncommitted edits (left) vs the file on disk now (right).",
    );
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.vertical_centered_justified(|ui| {
            ui.strong("mine (unsaved)")
                .on_hover_text("What 'Keep mine' would write to disk");
            ui.label(egui::RichText::new("left").weak().small());
        });
        ui.add_space(16.0);
        ui.vertical_centered_justified(|ui| {
            ui.strong("disk (current)")
                .on_hover_text("Snapshot taken when 'Show diff' was clicked");
            ui.label(egui::RichText::new("right").weak().small());
        });
    });
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.set_height(ui.available_height());
        ui.set_min_width(ui.available_width() * 0.5);
        paint_pane(ui, "kb.diff.mine", &mine);
        ui.separator();
        ui.set_height(ui.available_height());
        ui.set_min_width(ui.available_width() * 0.5);
        if let Some(text) = &disk {
            paint_pane(ui, "kb.diff.disk", text);
        } else {
            ui.add_space(8.0);
            ui.colored_label(
                egui::Color32::from_rgb(180, 120, 60),
                "(no disk snapshot — the file was unreadable or deleted)",
            );
        }
    });

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("Close").clicked() {
            app.close_conflict_diff();
        }
        if ui
            .button("Reload from disk")
            .on_hover_text("Discard my edits and re-load the file")
            .clicked()
        {
            if let Some(abs) = app.state().selected_page.clone() {
                app.open_page(&abs);
            }
            app.state_mut().save_status = crate::data::SaveStatus::Unsaved;
            app.close_conflict_diff();
        }
        if ui
            .button("Keep mine")
            .on_hover_text("Overwrite the disk with my buffer (writes anyway)")
            .clicked()
        {
            app.state_mut().save_status = crate::data::SaveStatus::Unsaved;
            app.save_page_force();
            app.close_conflict_diff();
        }
    });
}

fn paint_pane(ui: &mut egui::Ui, salt: &str, body: &str) {
    let mut text = body.to_string();
    egui::ScrollArea::both()
        .id_salt(salt)
        .auto_shrink(false)
        .show(ui, |ui_inner| {
            ui_inner.set_min_size(ui_inner.available_size());
            ui_inner.add(
                egui::TextEdit::multiline(&mut text)
                    .code_editor()
                    .font(egui::TextStyle::Monospace)
                    .interactive(false)
                    .desired_width(f32::INFINITY)
                    .desired_rows(80),
            );
        });
}
