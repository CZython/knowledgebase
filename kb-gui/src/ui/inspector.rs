//! The frontmatter inspector (PLAN-GUI §6.9): a compact property panel,
//! painted as an `egui::Window` over the shell (like the search overlay),
//! open with `Ctrl/Cmd+Shift+F` or the `⚙ properties` top-bar button.
//!
//! Fields (per §6.9): `type` (combo: note/wiki/label), `title`,
//! `description` (≤255 + counter), `language`, `tags` (chip editor),
//! `search_required_phrase` (chip editor, rendered "collections"). Read-only
//! footer: display id, abs path, uuid short. Commit sends the
//! `data::FmOp` batch (the verified `kb_core::frontmatter` op kernel) to the
//! worker; Revert resets the drafts to the captured baseline without disk.
//!
//! Every field clone-outs its draft each frame (egui wants an owned
//! `&mut String`), writes it back on change, and marks the panel *dirty*
//! via `App::mark_unsaved` (the same `save_status` machinery the raw editor
//! uses, so the status bar / `Ctrl+S` semantics stay uniform).

use eframe::egui;
use eframe::egui::InnerResponse;

use crate::app::App;
use crate::state::InspectorField;

/// Paint the inspector `Window` when `inspector.open` (no-op otherwise).
/// Called last in the paint pass so it renders *over* the shell, exactly
/// like `search::paint_if_open`.
pub fn paint_if_open(app: &mut App, ui: &mut egui::Ui) {
    if !app.state().inspector.open {
        return;
    }
    let page = app
        .state()
        .open_page
        .as_ref()
        .is_some_and(|p| p.body.is_some());
    // Escape closes the panel (the plan's "Esc closes").
    if ui.input(|i| i.key_released(egui::Key::Escape)) {
        app.state_mut().inspector.open = false;
        return;
    }
    // `paint_if_open` is only called when `inspector.open == true`; start
    // from that and let the title-bar ✕ (egui `Window::open`) flip it back.
    let mut is_open = true;
    let show_res = egui::Window::new("Frontmatter")
        .collapsible(false)
        .resizable(true)
        .min_width(520.0)
        .max_width(760.0)
        .open(&mut is_open)
        .show(ui.ctx(), |ui| {
            paint_panel(ui, app, page);
        });
    // Destructure for ergonomics — we don't need `response` here.
    let _ = show_res.map(|InnerResponse { response: _, inner }| inner);
    if !is_open {
        app.state_mut().inspector.open = false;
    }
}

fn paint_panel(ui: &mut egui::Ui, app: &mut App, page: bool) {
    // Header: the page's read-only identity (§6.9 footer fields surfaced in
    // the panel).
    if let Some(abs) = &app.state().selected_page {
        if let Some(row) = app.state().pages.iter().find(|p| &p.abs_path == abs) {
            let title = row.title.clone().unwrap_or_else(|| row.id.clone());
            ui.horizontal(|ui| {
                ui.strong(title);
                ui.weak(format!(" · {}", row.rel_path));
            });
            if !row.uuid.is_empty() {
                // stable 16-hex token → show the last 8 (the "uuid short").
                let short = &row.uuid[row.uuid.len().saturating_sub(8)..];
                ui.weak(format!(" · id: {}", row.id));
                ui.weak(format!(" · uuid:…{short}"));
            }
        }
    } else {
        ui.weak("no page open — the inspector seeds once a page is open");
    }
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);

    if !page {
        ui.weak("open a page, then edit type / title / description / language / tags / collections here.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let _ = ui.add_enabled(false, egui::Button::new("Revert"));
            let _ = ui.add_enabled(false, egui::Button::new("Commit (Ctrl+Enter)"));
        });
        return;
    }

    // §6.15: the structured inspector is read-only for non-`kb` documents
    // (generic markdown) — conversion owns `type` / `title` / tags. Raw YAML
    // editing in the raw view still works. Detect by the open page's
    // `DocumentKind`: only `KbPage` (full features) and `UnindexedMarkdown`
    // (valid `type:`, just not indexed yet) have full feature parity.
    let full_kb_features = matches!(
        app.state().open_page.as_ref().map(|p| p.kind),
        Some(crate::state::DocumentKind::KbPage)
            | Some(crate::state::DocumentKind::UnindexedMarkdown)
    );
    if !full_kb_features {
        ui.colored_label(
            egui::Color32::from_rgb(200, 160, 60),
            "Inspector is read-only until this document is converted to a
             valid `kb` page (use the Convert banner above).",
        );
        ui.separator();
        ui.add_space(6.0);
    }

    let saving = app.state().inspector.saving;

    // The stable-widget-id for each §6.9 field (so Tab / Shift+Tab can move
    // `egui` focus onto the next field — the plan's "keyboard traversal"
    // rule). `make_persistent_id` salts the id against this Ui's scope, so
    // it is stable per paint pass and unique to this window.
    fn field_id(ui: &egui::Ui, salt: &str) -> egui::Id {
        ui.make_persistent_id(salt)
    }
    let id_of = |f: InspectorField| match f {
        InspectorField::Type => "kb.insp.type",
        InspectorField::Title => "kb.insp.title",
        InspectorField::Description => "kb.insp.description",
        InspectorField::Language => "kb.insp.language",
        InspectorField::Tags => "kb.insp.tags",
        InspectorField::Collections => "kb.insp.collections",
    };
    let id_type = field_id(ui, id_of(InspectorField::Type));
    let id_title = field_id(ui, id_of(InspectorField::Title));
    let id_desc = field_id(ui, id_of(InspectorField::Description));
    let id_lang = field_id(ui, id_of(InspectorField::Language));
    let id_tags = field_id(ui, id_of(InspectorField::Tags));
    let id_coll = field_id(ui, id_of(InspectorField::Collections));

    // ---- type (combo: note / wiki / label / unset) ----
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("type"));
        let mut t = app.state().inspector.draft_type.clone();
        let display = if t.is_empty() { "—" } else { t.as_str() };
        egui::ComboBox::from_id_salt("kb.insp.type")
            .selected_text(display)
            .show_ui(ui, |ui| {
                for v in ["note", "wiki", "label"] {
                    if ui.selectable_label(t == v, v).clicked() {
                        t = v.to_string();
                    }
                }
                if ui.selectable_label(t.is_empty(), "— (unset)").clicked() {
                    t.clear();
                }
            });
        if t != app.state().inspector.draft_type {
            app.state_mut().inspector.draft_type = t;
            app.mark_unsaved();
        }
    });
    let _ = id_type;

    // ---- title ----
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("title"));
        let mut v = app.state().inspector.draft_title.clone();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut v)
                .id(id_title)
                .hint_text("(optional)")
                .desired_width(360.0),
        );
        if resp.changed() || v != app.state().inspector.draft_title {
            app.state_mut().inspector.draft_title = v.clone();
            app.mark_unsaved();
        }
    });

    // ---- description (≤255 + counter) ----
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("description"));
        let mut v = app.state().inspector.draft_description.clone();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut v)
                .id(id_desc)
                .hint_text("(one line)")
                .desired_width(300.0),
        );
        if resp.changed() || v != app.state().inspector.draft_description {
            app.state_mut().inspector.draft_description = v.clone();
            app.mark_unsaved();
        }
        let len = v.chars().count();
        let color = if len > 255 {
            egui::Color32::LIGHT_RED
        } else if len == 255 {
            egui::Color32::from_rgb(200, 160, 60)
        } else {
            ui.visuals().weak_text_color()
        };
        ui.colored_label(color, format!("{len}/255"));
    });

    // ---- language (+ known-values hint) ----
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("language"));
        let mut v = app.state().inspector.draft_language.clone();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut v)
                .id(id_lang)
                .hint_text("(optional)")
                .desired_width(300.0),
        );
        if resp.changed() || v != app.state().inspector.draft_language {
            app.state_mut().inspector.draft_language = v.clone();
            app.mark_unsaved();
        }
    });
    let known = &app.state().distinct_languages;
    if !known.is_empty() {
        let preview: Vec<&str> = known.iter().take(6).map(|s| s.as_str()).collect();
        ui.weak(format!("known: {}…", preview.join(", ")));
    }

    // ---- tags (chip editor) ----
    ui.add_space(4.0);
    paint_chip_editor(ui, app, /* is_tags = */ true, id_tags);

    // ---- collections (search_required_phrase chip editor) ----
    ui.add_space(4.0);
    paint_chip_editor(ui, app, /* is_tags = */ false, id_coll);

    // ---- Tab / Shift+Tab traversal (§6.9 keyboard: "Tab / Shift+Tab
    // across fields"). `focus` tracks the *application-level* field order;
    // we request egui focus onto the next field's stable widget id. Guard:
    // only fire when no widget has focus *or* the focused widget is one of
    // the inspector's own inputs (a Tab inside an unrelated window should
    // not steal the traversal).
    let mods = ui.input(|i| i.modifiers);
    let tab_pressed = ui.input(|i| i.key_pressed(egui::Key::Tab));
    let shift = mods.shift;
    if tab_pressed && !mods.ctrl && !mods.command {
        let from = app.state().inspector.focus;
        let next = if shift { from.prev() } else { from.next() };
        app.state_mut().inspector.focus = next;
        ui.memory_mut(|mem| mem.request_focus(field_id(ui, id_of(next))));
    }

    // ---- footer: pending summary + Revert + Commit ----
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);
    let pending = app.inspector_pending_summary();
    {
        let _ = match &pending {
            Some(line) => ui.colored_label(
                egui::Color32::from_rgb(200, 160, 60),
                format!("pending: {line}"),
            ),
            None => ui.weak("no pending changes"),
        };
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!saving && full_kb_features, egui::Button::new("Revert"))
            .clicked()
        {
            app.revert_inspector();
        }
        if ui
            .add_enabled(
                !saving && pending.is_some() && full_kb_features,
                egui::Button::new("Commit (Ctrl+Enter)"),
            )
            .clicked()
        {
            app.commit_inspector();
        }
        if saving {
            ui.weak("saving…");
        }
    });
}

/// A chip list + add-input for a token field (`tags` or
/// `search_required_phrase`). Each existing chip renders a `×` button that
/// removes it; the trailing input commits a token on Enter/comma. Drafts are
/// held on `InspectorState` selected by `is_tags`.
#[allow(clippy::too_many_lines)]
fn paint_chip_editor(ui: &mut egui::Ui, app: &mut App, is_tags: bool, field_id: egui::Id) {
    // Snapshot the drafts + input so the closures below can both read and
    // write back without an immutable borrow held across a `&mut` call.
    let label: &'static str = if is_tags { "tags" } else { "collections" };
    let mut drafts: Vec<String> = if is_tags {
        app.state().inspector.draft_tags.clone()
    } else {
        app.state().inspector.draft_collections.clone()
    };
    let mut input: String = if is_tags {
        app.state().inspector.draft_tags_input.clone()
    } else {
        app.state().inspector.draft_collections_input.clone()
    };

    // Chips.
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

        // Add input.
        let resp = ui.add(
            egui::TextEdit::singleline(&mut input)
                .id(field_id)
                .hint_text("add token, then Enter or ,")
                .desired_width(ui.available_width()),
        );
        if resp.changed() {
            // Live-edit the input buffer (write-back below); do not commit a
            // token on every keystroke.
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
                app.state_mut().inspector.draft_tags = drafts.clone();
                app.state_mut().inspector.draft_tags_input = input.clone();
            } else {
                app.state_mut().inspector.draft_collections = drafts.clone();
                app.state_mut().inspector.draft_collections_input = input.clone();
            }
            app.mark_unsaved();
        }
    }
}
