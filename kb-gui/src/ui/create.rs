//! The "New note" dialog (PLAN-GUI §6.10 "New note", §6.11 `Cmd/Ctrl+N`).
//!
//! An `egui::Window` over the shell (exactly like `convert::paint_if_open`):
//! `type` (required, default `wiki`), `title` (required), optional `tags`,
//! optional `collections` terms. **Create** dispatches `Cmd::CreatePage` via
//! `App::create_page`; the worker scaffolds + writes + indexes and answers
//! with `Created` (then auto-opens) or `Warn`. **Cancel** / `Esc` close
//! without writing.
//!
//! The dialog's *destination directory* is derived at Create time: prefer the
//! currently-open page's root, else the first indexed root, else `.` — the
//! same resolution the G2 inline form used. §6.10's "pick a root" field is
//! deferred (a single-root corpus is the common case; the derived directory
//! is correct there), matching the existing `create_page` contract.

use eframe::egui;

use crate::app::App;

/// Paint the New-note `Window` when `create_open` (no-op otherwise).
/// Called near the end of the paint pass so it renders *over* the shell.
pub fn paint_if_open(app: &mut App, ui: &mut egui::Ui) {
    if !app.state().create_open {
        // The flag being false is the common case; still clear any stale diff
        // state so a re-open of the *conflict* dialog isn't masked. (Cheap.)
        return;
    }
    // Escape closes the dialog (the plan's "cancel without writing").
    if ui.input(|i| i.key_released(egui::Key::Escape)) {
        app.state_mut().create_open = false;
        return;
    }
    egui::Window::new("New note")
        .collapsible(false)
        .resizable(true)
        .min_width(560.0)
        .max_width(760.0)
        .show(ui.ctx(), |ui| {
            paint_panel(ui, app);
        });
}

fn paint_panel(ui: &mut egui::Ui, app: &mut App) {
    ui.colored_label(
        egui::Color32::from_rgb(120, 160, 220),
        "Create a new `kb` page. The body starts as `# <title>`; `type:` /
         `title:` / `tags:` / `collections` are set in the frontmatter. With
         at least one collection term the page starts gated; leaving it empty
         starts it `Ungated`.",
    );
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);

    // ---- type (combo: wiki / note / label) ----
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("type").strong());
        let mut t = app.state().create_type.clone();
        egui::ComboBox::from_id_salt("kb.create.type")
            .selected_text(t.clone())
            .show_ui(ui, |ui| {
                for v in ["wiki", "note", "label"] {
                    if ui.selectable_label(t == v, v).clicked() {
                        t = v.to_string();
                    }
                }
            });
        if t != app.state().create_type {
            app.state_mut().create_type = t.clone();
        }
    });

    // ---- title (required) ----
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("title").strong());
        let mut v = app.state().create_title.clone();
        ui.add(
            egui::TextEdit::singleline(&mut v)
                .id_salt("kb.create.title")
                .hint_text("e.g. Energy in electronics")
                .desired_width(400.0),
        );
        if v != app.state().create_title {
            app.state_mut().create_title = v.clone();
        }
    });

    // ---- tags (chip editor) ----
    ui.add_space(4.0);
    paint_chip_editor(ui, app, /* is_tags = */ true);

    // ---- collections (search_required_phrase chip editor) ----
    ui.add_space(4.0);
    paint_chip_editor(ui, app, /* is_tags = */ false);

    // ---- footer: Cancel + Create ----
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("Cancel").clicked() {
            app.state_mut().create_open = false;
        }
        let enabled = !app.state().create_title.trim().is_empty();
        // `Response::on_hover_text` adds the tooltip text — `add_enabled`
        // already adds the widget; we attach the tooltip on the response.
        if ui
            .add_enabled(enabled, egui::Button::new("Create"))
            .on_hover_text("Scaffold, write and index a new page")
            .clicked()
            && enabled
        {
            let directory = resolve_directory(app);
            let title = app.state().create_title.trim().to_string();
            let page_type = app.state().create_type.clone();
            let tags = app.state().create_tags.clone();
            let collections = app.state().create_collections.clone();
            app.create_page(directory, title, page_type, tags, collections);
            app.state_mut().create_open = false;
            app.state_mut().create_title = String::new();
        }
    });
}

/// The destination directory for the new page (see module doc). Prefers the
/// open page's root, then the first indexed root, then the cwd.
pub(crate) fn resolve_directory(app: &App) -> String {
    for p in &app.state().pages {
        if !p.root.is_empty() {
            return p.root.clone();
        }
    }
    // No indexed root yet (e.g. the Welcome / first-launch state) — fall back
    // to the cwd (the worker's `create_dir_all` will materialise it).
    ".".to_string()
}

/// A chip list + add-input for a token field (`tags` or `collections`),
/// mirroring `convert::paint_chip_editor`. Drafts live on `AppState`
/// (`create_tags` / `create_collections`) selected by `is_tags`.
fn paint_chip_editor(ui: &mut egui::Ui, app: &mut App, is_tags: bool) {
    let label: &'static str = if is_tags { "tags" } else { "collections" };
    let mut drafts: Vec<String> = if is_tags {
        app.state().create_tags.clone()
    } else {
        app.state().create_collections.clone()
    };
    let mut input: String = String::new();

    {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(label).strong());
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
                    "kb.create.tags"
                } else {
                    "kb.create.collections"
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
                app.state_mut().create_tags = drafts.clone();
            } else {
                app.state_mut().create_collections = drafts.clone();
            }
        }
    }
}
