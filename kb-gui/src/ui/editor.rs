//! The raw Markdown editor (PLAN-GUI §6.5 / G2).
//!
//! A full-height `TextEdit::multiline` (code_editor / monospace) whose
//! line-number gutter is a **custom extended TextEdit** (G4 "LineNumberedEditor").
//! The gutter is toggleable at runtime via a "line numbers" checkbox in the
//! header. Persisted in `AppState::show_line_numbers`.
//!
//! The gutter is *guaranteed* to be row-aligned with the code by
//! construction: both use the same `font_id` (TextStyle::Monospace),
//! the same `line_height` (egui's TextEdit `row_height` + `extra_text_line_spacing`),
//! and the same wrap width (`INFINITY` ⇒ no wrap). The gutter is produced
//! by `LayoutJob::simple` over `1\n2\n3…N` so its `row[i].pos.y` is
//! structurally identical to the TextEdit galley's `row[i].pos.y`, and
//! the gutter is painted at the top-left of the allocation rect (same
//! vertical start as the TextEdit's galley).

use eframe::egui;
use eframe::egui::{Align2, FontId, Pos2, Sense, Vec2};

use crate::app::App;
use crate::data::SaveStatus;

/// Padding between the last digit and the vertical separator (px).
const GUTTER_RIGHT_PAD: f32 = 6.0;
/// Padding between the separator and the first character of the TextEdit.
const GUTTER_TO_TEXT_GAP: f32 = 6.0;
/// Min number of rows to show in the gutter (even an empty doc gets line 1).
const MIN_GUTTER_ROWS: usize = 1;

pub fn paint(app: &mut App, ui: &mut egui::Ui) {
    paint_header(app, ui);
    paint_conflict_banner(app, ui);
    if app.state().create_open {
        paint_create_form(app, ui);
        ui.separator();
    }

    let Some(body) = app.state().open_page.as_ref().and_then(|p| p.body.clone()) else {
        ui.weak("no page open yet — or the body is still loading");
        return;
    };
    let pre_text = body.clone();
    let mut text = pre_text.clone();
    let line_numbers_on = app.state().show_line_numbers;

    // Compute the font parameters *once* so the gutter and the TextEdit use
    // the EXACT same triple (`font_id`, `line_height`, wrap) the code
    // editor is configured with. This is what makes the gutter's row
    // alignment with the code correct by construction, regardless of
    // line count, content, or window size.
    let style = (*ui.style()).clone();
    let font_id: FontId = egui::TextStyle::Monospace.resolve(&style);
    let line_height: f32 =
        ui.fonts_mut(|f| f.row_height(&font_id) + style.spacing.extra_text_line_spacing);

    // Determine the number of *source* lines. The gutter labels source
    // lines, not display rows — with INFINITY wrap (no wrap), the one-to-one
    // mapping holds, and the gutter's `row.count()` equals line count.
    let line_count = text.lines().count().max(MIN_GUTTER_ROWS);

    // Build the gutter galley: exactly the same font / line_height /
    // INFINITY wrap / keep_trailing_whitespace as the TextEdit's internal
    // layouter (see `egui::widgets::text_edit::builder::default_layouter`).
    // `halign = Align::RIGHT` so multi-digit numbers hug our chosen right
    // edge (we position per row via `Painter::text(...RIGHT_CENTER...)`,
    // so this is belt-and-suspenders — the anchor param is authoritative).
    let gutter_galley = if line_numbers_on {
        let mut lines: Vec<String> = Vec::with_capacity(line_count);
        for i in 1..=line_count {
            lines.push(i.to_string());
        }
        let mut job = egui::text::LayoutJob::simple(
            lines.join("\n"),
            font_id.clone(),
            ui.visuals().weak_text_color(),
            f32::INFINITY,
        );
        job.keep_trailing_whitespace = true;
        job.halign = egui::Align::RIGHT;
        for s in job.sections.iter_mut() {
            s.format.line_height = Some(line_height);
        }
        Some(ui.ctx().fonts_mut(|f| f.layout_job(job)))
    } else {
        None
    };

    // The gutter column's width (in points): wide enough for the widest
    // number, plus padding on both sides, plus the separator stroke.
    let gutter_width = if let Some(g) = &gutter_galley {
        g.size().x + GUTTER_RIGHT_PAD + GUTTER_TO_TEXT_GAP
    } else {
        0.0
    };

    // Hoist the visuals (immutable values) BEFORE the closure, since inside
    // the closure `ui` is immutably borrowed and `&mut` borrows collide.
    let weak_text_color = ui.visuals().weak_text_color();
    let gutter_stroke = ui.visuals().widgets.noninteractive.bg_stroke;

    // The ONE vertical (and horizontal, for very long lines) scroll area.
    // `auto_shrink(false)` so it never "shrinks to fit" and hides lines.
    egui::ScrollArea::both()
        .id_salt("kb-editor-scroll")
        .auto_shrink(false)
        .show(ui, |ui_inner| {
            ui_inner.set_min_size(ui_inner.available_size());
            ui_inner.horizontal(|ui_row| {
                // ---- Gutter (if enabled) ---------------------------------
                // Allocate the gutter column's width + full height so
                // the TextEdit (the next widget in this horizontal) starts
                // exactly `gutter_width` points to its right and both
                // start at the SAME top. Then paint each number at its
                // row's own `pos.y` from the gutter galley (which is
                // row-identical to the code galley by construction).
                if let Some(galley) = &gutter_galley {
                    let total_h = line_count as f32 * line_height;
                    let gutter_rect = ui_row
                        .allocate_response(Vec2::new(gutter_width, total_h), Sense::hover())
                        .rect;

                    let painter = ui_row.painter();
                    for (i, row) in galley.rows.iter().enumerate() {
                        // Skip zero-height rows (e.g. trailing-empty newline).
                        if row.rect().height() <= 0.0 {
                            continue;
                        }
                        // Row i ↔ source line i+1 (because INFINITY wrap is
                        // no wrap — one `PlacedRow` per `\n` in the text).
                        let line_no = i + 1;
                        let y = gutter_rect.min.y + row.pos.y + row.rect().height() / 2.0;
                        let x_right = gutter_rect.max.x - GUTTER_RIGHT_PAD + 1.0;
                        painter.text(
                            Pos2::new(x_right, y),
                            Align2::RIGHT_CENTER,
                            line_no.to_string(),
                            font_id.clone(),
                            weak_text_color,
                        );
                    }

                    // Vertical separators (a single 1 px line down the right
                    // edge of the gutter column) — the classic split look.
                    painter.vline(
                        gutter_rect.max.x - 1.0,
                        gutter_rect.min.y..=gutter_rect.max.y,
                        gutter_stroke,
                    );

                    // Gap between separator and the first code character.
                    // Already accounted for in `gutter_width` so the TextEdit
                    // starts here; `add_space` makes it explicit and keeps
                    // `cursor().max.x` consistent.
                    // ui_row.add_space(GUTTER_TO_TEXT_GAP);
                }

                // ---- The TextEdit ----------------------------------------
                // `.code_editor()` = monospace + focus lock; `.desired_width(∞)`
                // is the "no wrap" knob (see TextEdit docs on `desired_width`):
                // the galley takes as many lines as the text has, not the
                // viewport's, so 1 galley row == 1 source line. The outer
                // `ScrollArea::both` handles the very-long-line case
                // (horizontal scroll).
                let resp = egui::TextEdit::multiline(&mut text)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(80)
                    .show(ui_row);
                // (Optional) hook to keep the cursor visible on scroll:
                let _ = resp; // response reserved for future "scroll to cursor"
            });
        });

    // Commit back + mark dirty (unchanged semantics from the legacy editor;
    // we still mutate `text` in place through `TextEdit`'s own buffer).
    if text != pre_text {
        if let Some(slot_mut) = app.state_mut().open_page.as_mut() {
            slot_mut.body = Some(text.clone());
        }
        app.mark_unsaved();
    }
}

fn paint_header(app: &mut App, ui: &mut egui::Ui) {
    let save_status = app.state().save_status;
    ui.horizontal(|ui| {
        let (label, color) = match save_status {
            SaveStatus::Saved => ("saved", egui::Color32::from_rgb(87, 167, 115)),
            SaveStatus::Unsaved => ("unsaved", egui::Color32::from_rgb(200, 160, 60)),
            SaveStatus::Saving => ("saving…", egui::Color32::from_rgb(120, 160, 220)),
            SaveStatus::Conflict => ("CONFLICT", egui::Color32::LIGHT_RED),
        };
        ui.colored_label(color, label);

        // The line-number toggle (user config knob; G4 directive).
        mut_checkbox_toggle(app, ui);

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
}

/// Small helper for the header checkbox so we can flip the flag.
#[allow(clippy::ptr_arg)]
fn mut_checkbox_toggle(app: &mut App, ui: &mut egui::Ui) {
    let want = app.state().show_line_numbers;
    let mut want2 = want;
    if ui.checkbox(&mut want2, "line numbers").changed() {
        app.state_mut().show_line_numbers = want2;
    }
}

fn paint_conflict_banner(app: &mut App, ui: &mut egui::Ui) {
    if app.state().save_status != SaveStatus::Conflict {
        return;
    }
    ui.horizontal(|ui| {
        ui.colored_label(
            egui::Color32::LIGHT_RED,
            "The file changed on disk since this editor session.",
        );
        ui.separator();
        if ui.button("Reload from disk").clicked() {
            if let Some(abs) = app.state().selected_page.clone() {
                app.open_page(&abs);
            }
        }
        if ui.button("Keep mine (write anyway)").clicked() {
            app.state_mut().save_status = SaveStatus::Unsaved;
            app.save_page();
        }
    });
    ui.separator();
}

/// A compact inline form for creating a new note (PLAN-GUI §6.5 "New note
/// dialog"; G2 keeps it inline rather than a modal window to avoid the
/// extra eframe viewport plumbing). Title + type. On Create it dispatches
/// `Cmd::CreatePage` via `App::create_page` and closes the form.
pub fn paint_create_form(app: &mut App, ui: &mut egui::Ui) {
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
