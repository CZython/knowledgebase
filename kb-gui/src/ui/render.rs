//! Formatted Markdown render pass (PLAN-GUI §7, G3).
//!
//! Walks the IR produced by `md::parse` and emits egui widgets:
//!
//! | IR block        | Rendered as                                |
//! |-----------------|--------------------------------------------|
//! | `Heading`       | Sized rich text (H1 largest → H6 smallest) |
//! | `Paragraph`     | Rich text, wraps at pane width             |
//! | `CodeBlock`     | Framed monospace panel, language caption   |
//! | `List`/`Item`   | Bullet / number + inline text, indented    |
//! | `BlockQuote`    | Left bar + indented children               |
//! | `Table`         | `egui::Grid`, header row bold              |
//! | `Rule`          | Horizontal separator                       |
//! | `Html`          | Weak monospace `[html] …`                  |
//!
//! Inline rich text is built directly into a `LayoutJob` by recursing the
//! inline tree (so nested wrappers compose). Styling maps to `TextFormat`:
//! strong → `strong_text_color`, emphasis → italics, strikethrough →
//! strikethrough stroke, link → `hyperlink_color` + underline, code →
//! monospace + `code_bg_color`, image → weak placeholder glyph (real
//! textures need a registered image loader — tracked as a follow-up).
//!
//! The pass is *pure*: `&Doc` + `&mut egui::Ui` in, pixels out. The live
//! call site is `preview.rs` → `paint_doc`.

use eframe::egui;
use eframe::egui::text::LayoutJob;
use std::sync::Arc;

use super::md::{self, Align, Block, Doc, Inline};

/// Paint the whole document (its `root` blocks) in order.
pub fn paint_doc(ui: &mut egui::Ui, doc: &Doc) {
    for &idx in &doc.root {
        paint_block(ui, doc, idx);
    }
}

/// Paint a single arena block (and recurse into containers). Exposed for the
/// WYSIWYG pass (G4), which paints splicable blocks as editable widgets and
/// falls back to this for the non-splicable ones (list / quote / table / …).
pub fn paint_single(ui: &mut egui::Ui, doc: &Doc, idx: usize) {
    paint_block(ui, doc, idx);
}

/// Paint a single arena block (and recurse into containers).
fn paint_block(ui: &mut egui::Ui, doc: &Doc, idx: usize) {
    let Some(block) = doc.blocks.get(idx) else {
        return;
    };
    match block {
        Block::Heading { level, inlines } => {
            paint_spacing_by_level(ui, *level);
            paint_inline_row(ui, inlines, heading_size(*level));
            ui.add_space(6.0);
        }
        Block::Paragraph { inlines } => {
            paint_inline_row(ui, inlines, 0.0);
            ui.add_space(8.0);
        }
        Block::Code { lang, text } => paint_code_block(ui, lang, text),
        Block::List {
            items,
            ordered,
            start,
        } => paint_list(ui, doc, *ordered, *start, items),
        Block::ListItem { .. } => {
            // Only reachable if a list item appears at top level; paint it as
            // an unnumbered item in defensive isolation.
            paint_list_item(ui, doc, idx, None);
        }
        Block::Quote { children } => {
            // Children render in a *vertical* indented scope (children may
            // contain lists / indented blocks, which require a vertical
            // layout — `ui.indent` panics on a horizontal one).
            ui.indent("kb-blockquote", |ui| {
                for &c in children {
                    paint_block(ui, doc, c);
                }
            });
            ui.add_space(8.0);
        }
        Block::Table {
            aligns,
            header,
            rows,
        } => paint_table(ui, aligns, header, rows),
        Block::Rule => {
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
        }
        Block::Html { text } => {
            ui.add(
                egui::Label::new(egui::RichText::new(format!("[html] {}", text.trim())).weak())
                    .wrap(),
            );
            ui.add_space(6.0);
        }
    }
}

fn paint_spacing_by_level(ui: &mut egui::Ui, level: u8) {
    let before = match level {
        1 => 10.0,
        2 => 8.0,
        _ => 6.0,
    };
    ui.add_space(before);
}

fn heading_size(level: u8) -> f32 {
    match level {
        1 => 30.0,
        2 => 25.0,
        3 => 21.0,
        4 => 19.0,
        5 => 16.5,
        _ => 15.0,
    }
}

// ---------------------------------------------------------------------------
// Inline rich text
// ---------------------------------------------------------------------------

/// A per-call styling palette derived from the active `Visuals`.
#[derive(Clone)]
struct Ink {
    base_font: egui::FontId,
    color: egui::Color32,
    strong: egui::Color32,
    link: egui::Color32,
    code_bg: egui::Color32,
    weak: egui::Color32,
}

impl Ink {
    fn new(ui: &egui::Ui, size: f32) -> Self {
        let visuals = ui.visuals();
        let base_font = if size > 0.0 {
            egui::FontId::proportional(size)
        } else {
            egui::FontSelection::Default.resolve(ui.style())
        };
        Self {
            base_font,
            color: visuals.text_color(),
            strong: visuals.strong_text_color(),
            link: visuals.hyperlink_color,
            code_bg: visuals.code_bg_color,
            weak: visuals.weak_text_color(),
        }
    }
}

/// Build a wrapping, multi-line `LayoutJob` for an inline list and label it.
fn paint_inline_row(ui: &mut egui::Ui, inlines: &[Inline], size: f32) {
    if inlines.is_empty() {
        return;
    }
    let ink = Ink::new(ui, size);
    let mut job = LayoutJob {
        // Default already sets `max_width: INFINITY`; the Label widget
        // constrains it to the pane width at layout time.
        wrap: Default::default(),
        break_on_newline: true,
        ..Default::default()
    };
    append_inlines(
        &mut job,
        inlines,
        &ink,
        Wrap {
            bold: false,
            italics: false,
            strike: false,
        },
    );
    if job.text.is_empty() {
        return;
    }
    ui.add(egui::Label::new(egui::WidgetText::LayoutJob(Arc::new(job))).wrap());
}

/// Flags for wrapper styling to apply to the leaves inside it. Simple
/// copyable flags so the recursion below doesn't juggle closure params.
#[derive(Clone, Copy)]
struct Wrap {
    bold: bool,
    italics: bool,
    strike: bool,
}

/// Recursively append inline fragments to `job`. Any `wr` flags active on
/// entry (from enclosing wrappers) are OR'd into every leaf's format.
fn append_inlines(job: &mut LayoutJob, inlines: &[Inline], ink: &Ink, wr: Wrap) {
    for i in inlines {
        match i {
            Inline::Text(t) => {
                let mut f = egui::TextFormat::simple(ink.base_font.clone(), ink.color);
                apply(&mut f, ink, wr);
                job.append(t, 0.0, f);
            }
            Inline::Code(c) => {
                let mut f = egui::TextFormat {
                    font_id: egui::FontId::monospace(ink.base_font.size.max(12.0)),
                    color: ink.color,
                    background: ink.code_bg,
                    expand_bg: 1.0,
                    ..Default::default()
                };
                apply(&mut f, ink, wr);
                job.append(&format!(" {c} "), 0.0, f);
            }
            Inline::Link { label, .. } => {
                let color = ink.link;
                let font = ink.base_font.clone();
                for l in label {
                    let mut f = egui::TextFormat::simple(font.clone(), color);
                    f.underline = egui::Stroke::new(1.0, color);
                    apply(&mut f, ink, wr);
                    job.append(&md::flat_text(std::slice::from_ref(l)), 0.0, f);
                }
            }
            Inline::Image { alt, .. } => {
                let mut f = egui::TextFormat::simple(ink.base_font.clone(), ink.weak);
                apply(&mut f, ink, wr);
                job.append(&format!(" [img: {alt}] "), 0.0, f);
            }
            Inline::Strong(inner) => append_inlines(job, inner, ink, with(wr, true, false, false)),
            Inline::Emphasis(inner) => {
                append_inlines(job, inner, ink, with(wr, false, true, false))
            }
            Inline::Strikethrough(inner) => {
                append_inlines(job, inner, ink, with(wr, false, false, true))
            }
            Inline::Break(hard) => {
                let text = if *hard { "\n" } else { " " };
                let mut f = egui::TextFormat::simple(ink.base_font.clone(), ink.color);
                apply(&mut f, ink, wr);
                job.append(text, 0.0, f);
            }
        }
    }
}

fn with(wr: Wrap, bold: bool, italics: bool, strike: bool) -> Wrap {
    Wrap {
        bold: wr.bold || bold,
        italics: wr.italics || italics,
        strike: wr.strike || strike,
    }
}

/// Compose wrapper styling into a leaf's format. `bold` recolors to the
/// strong color; the other flags are font attributes.
fn apply(f: &mut egui::TextFormat, ink: &Ink, wr: Wrap) {
    if wr.bold {
        f.color = ink.strong;
    }
    if wr.italics {
        f.italics = true;
    }
    if wr.strike {
        f.strikethrough = egui::Stroke::new(1.0, f.color);
    }
}

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

fn paint_code_block(ui: &mut egui::Ui, lang: &Option<String>, text: &str) {
    let visuals = ui.visuals();
    let (code_bg, text_color, weak_color) = (
        visuals.code_bg_color,
        visuals.text_color(),
        visuals.weak_text_color(),
    );
    if let Some(lang) = lang {
        ui.add(egui::Label::new(
            egui::RichText::new(lang.clone()).color(weak_color),
        ));
        ui.add_space(2.0);
    }
    let frame = egui::Frame::new()
        .fill(code_bg)
        .inner_margin(egui::Margin::same(6))
        .corner_radius(4);
    frame.show(ui, |ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(text.to_string())
                    .monospace()
                    .color(text_color),
            )
            .wrap(),
        );
    });
    ui.add_space(10.0);
}

fn paint_list(ui: &mut egui::Ui, doc: &Doc, ordered: bool, start: u64, items: &[usize]) {
    for (i, &item_idx) in items.iter().enumerate() {
        paint_list_item(
            ui,
            doc,
            item_idx,
            if ordered {
                Some(start + i as u64)
            } else {
                None
            },
        );
    }
}

fn paint_list_item(ui: &mut egui::Ui, doc: &Doc, idx: usize, number: Option<u64>) {
    let Some(block) = doc.blocks.get(idx) else {
        return;
    };
    let Block::ListItem {
        task,
        inlines,
        children,
    } = block
    else {
        return;
    };
    let marker = match (task, number) {
        (Some(true), _) => "☑".to_string(),
        (Some(false), _) => "☐".to_string(),
        (None, Some(n)) => format!("{n}. "),
        (None, None) => "• ".to_string(),
    };
    ui.horizontal_wrapped(|ui| {
        ui.add(egui::Label::new(egui::RichText::new(marker).weak()));
        // Tight list items carry their inline text directly; loose items put
        // it in the first Paragraph child.
        if !inlines.is_empty() {
            paint_inline_row(ui, inlines, 0.0);
        } else if let Some(first_para) = children.iter().find_map(|&c| match &doc.blocks[c] {
            Block::Paragraph { inlines } => Some(inlines),
            _ => None,
        }) {
            paint_inline_row(ui, first_para, 0.0);
        }
    });
    ui.add_space(2.0);
    // Non-paragraph children (nested lists, code, …) get indented.
    for &c in children {
        if matches!(doc.blocks[c], Block::Paragraph { .. }) {
            continue;
        }
        ui.indent("kb-list-item-nested", |ui| {
            paint_block(ui, doc, c);
        });
    }
    ui.add_space(4.0);
}

fn paint_table(
    ui: &mut egui::Ui,
    aligns: &[Align],
    header: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
) {
    egui::Grid::new("kb-table")
        .striped(true)
        .spacing([14.0, 4.0])
        .show(ui, |ui| {
            for (ci, cell) in header.iter().enumerate() {
                align_cell(ui, aligns, ci, cell, true);
            }
            ui.end_row();
            for row in rows {
                for (ci, cell) in row.iter().enumerate() {
                    align_cell(ui, aligns, ci, cell, false);
                }
                ui.end_row();
            }
        });
    ui.add_space(10.0);
}

fn align_cell(ui: &mut egui::Ui, aligns: &[Align], ci: usize, cell: &[Inline], header: bool) {
    let flat = md::flat_text(cell);
    let visuals = ui.visuals();
    let rt = if header {
        egui::RichText::new(flat)
            .strong()
            .color(visuals.strong_text_color())
    } else {
        egui::RichText::new(flat).color(visuals.text_color())
    };
    // No `.wrap()`: inside the Grid, wrapping makes each label's min-width
    // tiny, so the grid's first sizing pass remembers a narrow column width
    // (stored in `prev_state`) and every subsequent frame wraps mid-word.
    // Let the cell expand to content width instead.
    match aligns.get(ci).copied().unwrap_or(Align::None) {
        Align::None | Align::Left => {
            ui.add(egui::Label::new(rt));
        }
        Align::Center => {
            ui.add(egui::Label::new(rt).halign(egui::Align::Center));
        }
        Align::Right => {
            ui.add(egui::Label::new(rt).halign(egui::Align::Max));
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::md;

    /// Paint a full-document fixture exercising every block type in the IR
    /// (headings, paragraphs, code, lists with tight/loose items and nesting,
    /// quotes, tables, rules, html). Catches the `ui.indent`-on-horizontal and
    /// the tight-item / orphan-emphasis panics without a display.
    #[test]
    fn paint_full_fixture_does_not_panic() {
        let body = "# top\n\npara **bold** and `code` and [link](https://x)\n\n> quote line 1\n> quote line 2\n\n- tight a\n- *emphasis* item\n\n1. ordered one\n2. ordered two\n   - nested under two\n\n```rust\nfn main() {}\n```\n\n| c1 | c2 |\n|---|---:|\n| a | 9 |\n\n---\n\n`<raw html>`\n";
        let doc = md::parse(body);
        egui::__run_test_ui(|ui| {
            ui.vertical(|ui| {
                paint_doc(ui, &doc);
            });
        });
    }

    /// The exact regression that crashed the app: a top-level list whose items
    /// contain emphasis / code inlines (tight items land inlines directly on
    /// the item), followed by a nested (indented) sub-list. This drives both
    /// the tight-item inline path and the `ui.indent` path inside
    /// `paint_list_item`.
    #[test]
    fn paint_list_with_nested_items() {
        let doc = md::parse("- an *em* and `code`\n- second\n  - nested");
        egui::__run_test_ui(|ui| {
            ui.vertical(|ui| {
                paint_doc(ui, &doc);
            });
        });
    }

    /// A blockquote containing a nested list exercises `paint_block(Quote)` →
    /// vertical `ui.indent` → `paint_list` → `paint_list_item` → `ui.indent`:
    /// the deepest nesting of vertical scopes in the renderer.
    #[test]
    fn paint_quote_with_nested_list() {
        // Nested sub-list inside a blockquote item: `paint_block(Quote)` →
        // vertical `ui.indent` → `paint_list` → `paint_list_item` → `ui.indent`
        // — the deepest chain of vertical scopes in the renderer.
        let doc = md::parse("> - item one\n> - item two\n>   - sub under two\n");
        egui::__run_test_ui(|ui| {
            ui.vertical(|ui| {
                paint_doc(ui, &doc);
            });
        });
    }
}
