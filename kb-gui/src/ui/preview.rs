//! The preview pane (PLAN-GUI §6.5, §7): the open page's body, read-only.
//!
//! Two view modes (`ViewMode`):
//! - `Formatted` — the Markdown body (frontmatter stripped) rendered as
//!   rich text via `md::parse` → `render::paint_doc`: sized headings,
//!   bold/italic/strikethrough, links, code blocks, task lists, block
//!   quotes, and GFM tables.
//! - `Raw` — the file verbatim (frontmatter included), monospaced + wrapping.
//!
//! `Cmd/Ctrl+E` toggles between the two.

use std::path::Path;

use eframe::egui;

use super::{md, render};
use crate::app::App;
use crate::state::{DocumentKind, OpenPageState, ViewMode};

/// Paint the preview for the open page (`None` → muted placeholder).
///
/// Takes `&mut App` (not just the page) so the §6.15 conversion/add-root
/// banner — painted above the document — can dispatch its actions through
/// the `App` (`dismiss_banner` / `add_root_for_open_page` /
/// `open_conversion`), mirroring the way `editor::paint` takes `app`.
pub fn paint(app: &mut App, ui: &mut egui::Ui) {
    ui.set_min_width(400.0);
    // The §6.15 banner (if this document is not a full `kb` page) is a
    // non-blocking strip *above* the document — painted in the pane's outer
    // (horizontal) context, then the scrollable body below it.
    if let Some(slot) = app.state().open_page.as_ref() {
        if slot.body.is_some() && !slot.banner_dismissed && !slot.kind.is_kb_page() {
            paint_banner(app, ui, slot.kind);
        }
    }
    let page = app.state().open_page.clone();
    let view_mode = app.state().view_mode;
    // The pane lives inside a `horizontal_top` row (ui/mod.rs), so the
    // surrounding `ui` is horizontal. The formatted renderer relies on a
    // *vertical* layout (`ui.indent` for block quotes / nested lists panics
    // otherwise), so wrap the scrollable content in an explicit vertical
    // scope.
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.vertical(|ui| {
            paint_page(ui, &page, view_mode);
        });
    });
}

/// The §6.15 non-blocking banner strip (not a modal). Two variants:
/// * `GenericMarkdown` → "Would you like to convert this markdown file into
///   a valid page?" with **Convert** + **Dismiss**.
/// * `UnindexedMarkdown` → "Not indexed — add the containing folder as a
///   root …?" with **Add root** + **Dismiss** (+ Convert as a secondary).
fn paint_banner(app: &mut App, ui: &mut egui::Ui, kind: DocumentKind) {
    ui.horizontal_wrapped(|ui| {
        let (heading, color) = match kind {
            DocumentKind::GenericMarkdown => (
                "⚠ This is generic Markdown (no valid `type:` frontmatter).",
                egui::Color32::from_rgb(200, 160, 60),
            ),
            DocumentKind::UnindexedMarkdown => (
                "⚠ Valid `type:`, but this file is not indexed (outside every root).",
                egui::Color32::from_rgb(120, 160, 220),
            ),
            DocumentKind::KbPage => return, // no banner for a full kb page
        };
        ui.colored_label(color, heading);
        ui.separator();
        match kind {
            DocumentKind::GenericMarkdown => {
                ui.label("Convert it into a valid page to enable search, links, and collections:");
                if ui.button("Convert").clicked() {
                    seed_conversion(app);
                    app.open_conversion();
                }
            }
            _ => {
                ui.label(
                    "Add the containing folder as a root to enable search, links, and collections:",
                );
                if ui.button("Add root").clicked() {
                    app.add_root_for_open_page();
                }
            }
        }
        if ui.button("Dismiss").clicked() {
            app.dismiss_banner();
        }
    });
    ui.separator();
}

/// Seed the conversion dialog's fields from the open document (PLAN-GUI
/// §6.15 step 1): `type` defaults to `note`, `title` is prefilled from the
/// frontmatter title or the filename stem, tags/collections start empty
/// (free-form, added by the user).
fn seed_conversion(app: &mut App) {
    let mut abs = String::new();
    let mut body_opt: Option<String> = None;
    if let Some(slot) = app.state().open_page.as_ref() {
        abs = slot.abs_path.clone();
        body_opt = slot.body.clone();
    }
    let mut title = String::new();
    if let Some(body) = &body_opt {
        let fm = kb_core::frontmatter::parse(body);
        // `title:` from frontmatter, else the first H1.
        title = fm.title.unwrap_or_default();
        if title.trim().is_empty() {
            title = extract_h1(body);
        }
    }
    if title.trim().is_empty() {
        // Filename stem as the last-resort prefill (§6.15 step 1).
        let stem = Path::new(&abs)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned());
        if let Some(stem) = stem {
            if !stem.trim().is_empty() {
                title = stem;
            }
        }
    }
    app.state_mut().convert_title = title;
    app.state_mut().convert_type = "note".to_string();
    app.state_mut().convert_tags = Vec::new();
    app.state_mut().convert_collections = Vec::new();
}

/// The first `# ` heading in a Markdown body (fallback title source for the
/// conversion dialog, §6.15 step 1). `None` when there is no H1.
fn extract_h1(body: &str) -> String {
    let text = &kb_core::frontmatter::parse(body).body;
    for line in text.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("# ") {
            return rest.trim().trim_end_matches('#').trim().to_string();
        }
    }
    String::new()
}

fn paint_page(ui: &mut egui::Ui, page: &Option<OpenPageState>, view_mode: ViewMode) {
    let Some(page) = page else {
        ui.weak("no page open — pick a row in the note list");
        return;
    };
    if let Some(err) = &page.error {
        ui.colored_label(egui::Color32::LIGHT_RED, format!("⚠ {err}"));
        return;
    }
    let Some(body) = &page.body else {
        ui.weak("loading body…");
        return;
    };

    match view_mode {
        ViewMode::Formatted => render_formatted(ui, body),
        ViewMode::Raw => {
            ui.add(egui::Label::new(body.to_string()).wrap());
        }
    }
}

/// `Formatted` mode: strip the frontmatter and paint the Markdown body as
/// rich text. An empty body renders a muted placeholder.
fn render_formatted(ui: &mut egui::Ui, body: &str) {
    if body.trim().is_empty() {
        ui.weak("(empty page)");
        return;
    }
    let text = &kb_core::frontmatter::parse(body).body;
    if text.trim().is_empty() {
        ui.weak("(no body — frontmatter only)");
        return;
    }
    let doc = md::parse(text);
    render::paint_doc(ui, &doc);
}
