//! The "convert to page" dialog (PLAN-GUI §6.15 "Convert flow"): a compact
//! `egui::Window` over the shell (like the inspector), reached from the §6.15
//! banner's **Convert** button.
//!
//! Fields (per §6.15 step 1): `type` (required, default `note`), `title`
//! (prefilled from H1/filename), optional `tags`, optional `collections`
//! terms. The dialog previews the exact frontmatter that will be *added /
//! set* (never a full rewrite); **Convert and save** dispatches
//! `Cmd::ConvertPage` via `App::convert_open_page`. The worker preserves the
//! body + existing frontmatter, adds/sets `type:` / `title:` / `tags:` /
//! `search_required_phrase:` only, then reindexes.
//!
//! If the open document is an `UnindexedMarkdown` (valid `type:` but outside
//! every root), the dialog shows a note that the containing folder will be
//! added as a root as part of the conversion (step 2 "Add containing folder
//! as root & convert"). **Cancel** closes without touching the file.

use eframe::egui;

use crate::app::App;
use crate::state::DocumentKind;

/// Paint the conversion `Window` when `convert_open` (no-op otherwise).
/// Called near the end of the paint pass so it renders *over* the shell,
/// exactly like `inspector::paint_if_open`.
pub fn paint_if_open(app: &mut App, ui: &mut egui::Ui) {
    if !app.state().convert_open {
        return;
    }
    // Escape closes the dialog (the plan's "cancel without touching the file").
    if ui.input(|i| i.key_released(egui::Key::Escape)) {
        app.state_mut().convert_open = false;
        return;
    }
    egui::Window::new("Convert to page")
        .collapsible(false)
        .resizable(true)
        .min_width(520.0)
        .max_width(720.0)
        .show(ui.ctx(), |ui| {
            paint_panel(ui, app);
        });
}

fn paint_panel(ui: &mut egui::Ui, app: &mut App) {
    let kind = app
        .state()
        .open_page
        .as_ref()
        .map(|p| p.kind)
        .unwrap_or(DocumentKind::GenericMarkdown);

    // Header: a short description of what conversion does.
    ui.colored_label(
        egui::Color32::from_rgb(120, 160, 220),
        "Convert this document into a valid `kb` page. The body and all other
         frontmatter are preserved — only `type:` / `title:` / `tags:` /
         `collections` are added or set.",
    );
    if kind == DocumentKind::UnindexedMarkdown {
        ui.colored_label(
            egui::Color32::from_rgb(200, 160, 60),
            "This file is outside every root — its containing folder will be added as a root
         as part of the conversion, so the page is indexed and appears in search / links /
         collections.",
        );
    }
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);

    // ---- type (combo: note / wiki / label) ----
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("type").strong());
        let mut t = app.state().convert_type.clone();
        egui::ComboBox::from_id_salt("kb.convert.type")
            .selected_text(t.clone())
            .show_ui(ui, |ui| {
                for v in ["note", "wiki", "label"] {
                    if ui.selectable_label(t == v, v).clicked() {
                        t = v.to_string();
                    }
                }
            });
        if t != app.state().convert_type {
            app.state_mut().convert_type = t.clone();
        }
    });

    // ---- title ----
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("title"));
        let mut v = app.state().convert_title.clone();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut v)
                .id_salt("kb.convert.title")
                .hint_text("(prefilled from H1 / filename)")
                .desired_width(360.0),
        );
        if resp.changed() || v != app.state().convert_title {
            app.state_mut().convert_title = v.clone();
        }
    });

    // ---- tags (chip editor) ----
    ui.add_space(4.0);
    paint_chip_editor(ui, app, /* is_tags = */ true);

    // ---- collections (search_required_phrase chip editor) ----
    ui.add_space(4.0);
    paint_chip_editor(ui, app, /* is_tags = */ false);

    // ---- footer: Cancel + Convert and save ----
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("Cancel").clicked() {
            app.state_mut().convert_open = false;
        }
        if ui
            .button("Convert and save")
            .on_hover_text("Convert this document into a valid `kb` page and save")
            .clicked()
            && !app.state().convert_type.is_empty()
        {
            let page_type = app.state().convert_type.clone();
            let title = app.state().convert_title.clone();
            let tags = app.state().convert_tags.clone();
            let collections = app.state().convert_collections.clone();
            app.convert_open_page(page_type, title, tags, collections);
            app.state_mut().convert_open = false;
        }
    });
}

/// A chip list + add-input for a token field (`tags` or `collections`).
/// Each existing chip renders a `×` button that removes it; the trailing
/// input commits a token on Enter/comma. Drafts are held on `AppState`
/// (`convert_tags` / `convert_collections`) selected by `is_tags`.
fn paint_chip_editor(ui: &mut egui::Ui, app: &mut App, is_tags: bool) {
    let label: &'static str = if is_tags { "tags" } else { "collections" };
    let mut drafts: Vec<String> = if is_tags {
        app.state().convert_tags.clone()
    } else {
        app.state().convert_collections.clone()
    };
    let mut input: String = String::new();

    {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(label));
            for (i, token) in drafts.iter().enumerate() {
                let removed = ui
                    .button(format!("{token}  ×"))
                    .on_hover_text("remove this token")
                    .clicked();
                if removed {
                    drafts.remove(i);
                    changed = true;
                    break;
                }
            }
            if drafts.is_empty() {
                ui.weak("— none —");
            }
        });

        let resp = ui.add(
            egui::TextEdit::singleline(&mut input)
                .id_salt(if is_tags {
                    "kb.convert.tags"
                } else {
                    "kb.convert.collections"
                })
                .hint_text("add token, then Enter or ,")
                .desired_width(ui.available_width()),
        );
        if resp.changed() {
            changed = true;
        }
        if resp.lost_focus()
            || ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Comma))
        {
            let token = input.trim().to_string();
            if !token.is_empty() && !drafts.contains(&token) {
                drafts.push(token);
                input.clear();
                changed = true;
            }
        }
        if changed {
            if is_tags {
                app.state_mut().convert_tags = drafts.clone();
            } else {
                app.state_mut().convert_collections = drafts.clone();
            }
        }
    }
}
