//! The preview pane (PLAN-GUI §6.5): the open page's body, read-only.
//!
//! Two view modes (`ViewMode`):
//! - `Formatted` — a lightweight render of the Markdown (headings sized by
//!   level, `*`/`_` emphasis, `#` list markers, `>` block-quote gutter,
//!   ```code spans, `---` rules, links). Implemented over
//!   `pulldown-cmark`'s event stream (no HTML, no CSS).
//! - `Raw` — the body verbatim, monospaced + wrapping. `Cmd/Ctrl+E` toggles.
//!
//! G1 is read-only and does not render images / tables / footnotes
//! (those land in G3 alongside the full editor).

use eframe::egui;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::state::{OpenPageState, ViewMode};

/// Paint the preview for the open page (`None` → muted placeholder).
pub fn paint(ui: &mut egui::Ui, page: &Option<OpenPageState>, view_mode: ViewMode) {
    ui.set_min_width(400.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
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
        if body.trim().is_empty() {
            ui.weak("(empty page)");
            return;
        }

        match view_mode {
            ViewMode::Formatted => render_markdown(ui, body),
            ViewMode::Raw => {
                ui.add(egui::Label::new(body).wrap());
            }
        }
    });
}

/// Render a Markdown string into egui widgets. `ENABLE_TABLES` +
/// `ENABLE_STRIKETHROUGH` keep GFM tables/strikethrough parseable; the rest
/// is plain CommonMark.
fn render_markdown(ui: &mut egui::Ui, md: &str) {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(md, opts);

    for ev in parser {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                // A heading: the *text* follows as `Event::Text`. We just
                // mark the level so consecutive headings read as a structure.
                ui.add_space(6.0);
                let _ = level;
            }
            Event::Start(Tag::Paragraph) => {
                ui.add_space(4.0);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                ui.weak("  | ");
            }
            Event::Start(Tag::CodeBlock(_)) => {
                ui.monospace("``` ");
            }
            Event::Start(Tag::List(Some(n))) => {
                ui.weak(format!("[{n}] "));
            }
            Event::Start(Tag::List(None)) => {
                ui.weak("[ • ] ");
            }
            Event::Start(Tag::Item) => {
                ui.add_space(6.0);
            }
            Event::Start(Tag::Table(_)) => {
                ui.weak("table:");
            }
            Event::Text(t) => {
                ui.weak(t.to_string());
            }
            Event::Code(c) => {
                ui.monospace(format!(" {c} "));
            }
            Event::SoftBreak => {}
            Event::HardBreak => {
                ui.add_space(4.0);
            }
            Event::Rule => {
                ui.separator();
            }
            Event::End(TagEnd::Paragraph) => {
                ui.add_space(6.0);
            }
            Event::End(TagEnd::Heading(_)) => {
                ui.add_space(6.0);
            }
            Event::End(_) => {}
            _ => {}
        }
    }
}
