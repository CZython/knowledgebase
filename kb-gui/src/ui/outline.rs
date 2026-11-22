//! The outline panel (PLAN-GUI §6.6): the page's ATX section tree, derived
//! from `kb_core::section::Doc`. Read-only in G1 (clicking a heading to
//! scroll the preview to it lands in G3 — the scroll position is the
//! preview's to own).
//!
//! Layout: a narrow right-hand column (~200 px) listing `ref` + title,
//! indented by level. Painted into the browse shell next to the preview.

use eframe::egui;

use crate::state::OpenPageState;

/// Parse the page body into its ATX section tree.
fn sections(body: &str) -> Vec<kb_core::section::Section> {
    kb_core::section::Doc::new(body.as_bytes().to_vec()).sections
}

/// Paint the outline for the open page (or a muted placeholder when there
/// is no page, no body yet, or the body has no headings). The panel width
/// is claimed by the `SidePanel::right("kb.outline")` in `ui/mod.rs`.
pub fn paint(ui: &mut egui::Ui, page: &Option<OpenPageState>, body: &Option<String>) {
    ui.vertical(|ui| {
        ui.strong("Outline");
        ui.add_space(4.0);

        let Some(page) = page else {
            ui.weak("no page open");
            return;
        };
        let Some(body) = body else {
            ui.weak("loading body…");
            return;
        };
        if body.trim().is_empty() {
            ui.weak("empty page");
            return;
        }

        let secs = sections(body);
        if secs.is_empty() {
            ui.weak("no headings");
            return;
        }
        for s in &secs {
            // Indent by heading level so the tree reads top-down.
            let indent = (s.level.saturating_sub(1)) as f32 * 10.0;
            ui.add_space(indent);
            let label = if s.ref_ == "*" {
                s.title.clone()
            } else {
                format!("{}  {}", s.ref_, s.title)
            };
            // G1 is read-only; G3 wires click → scroll the preview to the
            // section's `start_line`.
            let _ = ui.weak(label);
        }
        let _ = page; // page is used for its abs_path in G3 (scroll target id).
    });
}
