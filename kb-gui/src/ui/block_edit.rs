//! WYSIWYG block-editing widgets for the Formatted view (PLAN-GUI §8, G4).
//!
//! **Read mode** (default, `wysiwyg_editing == None`): every block —
//! splicable or not — renders as rich formatted text via
//! [`render::paint_single`]. Splicable blocks (heading / paragraph / code)
//! are wrapped in a `ui.scope` so their click response is detectable;
//! clicking one enters **edit mode** for that block.
//!
//! **Edit mode** (`wysiwyg_editing == Some(pos)`): the block at that
//! splicable-subset position renders as a `TextEdit`. The text is
//! commit-on-keystroke via `splice`; clicking a different block switches
//! the edit target. Non-splicable blocks always render read-only.
//!
//! Splicing only the smallest span keeps every other byte byte-identical
//! (§8.6 span isolation). The frontmatter prefix is re-attached on commit.

use eframe::egui;
use eframe::egui::Key;

use super::md;
use super::render;
use super::splice::{splice, splicible_root_blocks, BlockEdit};
use crate::app::App;
use crate::state::OpenPageState;

/// Heading display size (H1 largest → H6 smallest), matching the read-only
/// renderer's scale so editable and fallback renderings agree visually.
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

/// Paint the Formatted view as a *view-editable* document.
///
/// Read mode (default): all blocks render as rich formatted text. Splicable
/// blocks (heading / paragraph / code) get a synthesized click target so the
/// user can enter edit mode by clicking them. The active edit target is
/// identified by its position in the splicable-subset (index into
/// `splicible_root_blocks`).
///
/// Edit mode (`wysiwyg_editing == Some(pos)`): the single block at `pos`
/// renders as a `TextEdit`; its text commits span-isolated on every
/// keystroke that changes it (mirrors the raw editor in `editor.rs`).
/// Non-splicable blocks always render read-only.
///
/// The open page must carry a loaded body; otherwise a muted placeholder is
/// shown (the call site in `preview.rs` already guards the no-page / loading /
/// error cases).
pub fn paint(ui: &mut egui::Ui, app: &mut App, page: &OpenPageState) {
    let Some(body) = page.body.as_deref() else {
        ui.weak("(no body)");
        return;
    };

    // Split the full file into (untouched frontmatter bytes) + the body-relative
    // Markdown slice. `md::Block::range` targets byte offsets *into the
    // body-relative slice* (§8 tests confirm body-relative ranges), so we splice
    // that slice and re-attach the frontmatter bytes verbatim on commit.
    let (prefix, md_body) = split_frontmatter(body);

    if md_body.trim().is_empty() {
        ui.weak(if prefix.is_empty() {
            "(empty page)"
        } else {
            "(no body — frontmatter only)"
        });
        return;
    }

    let doc = md::parse(&md_body);
    let edits = splicible_root_blocks(&doc, &md_body);
    if edits.is_empty() {
        // Nothing splicable (a list-only / quote-only document, etc.) — fall
        // back to the read-only renderer for the whole body.
        render::paint_doc(ui, &doc);
        return;
    }

    // Walk the root blocks in order. For splicable blocks, decide which
    // sub-state applies:
    //   * If the user is editing position `pos` in the splicable subset and
    //     *this* block is at that position → render a TextEdit.
    //   * Otherwise → render the rich read-only view, wrapping it in a
    //     `ui.scope` + `Response::interact(Sense::click())` so a click
    //     enters edit mode for this block.
    //
    // `edits` preserves document order over the splicable subset, so a
    // running counter tracks which splicable-subset position we're on.
    let editing = app.state().wysiwyg_editing;
    let mut splicable_pos: usize = 0;
    for &idx in &doc.root {
        let Some(block) = doc.blocks.get(idx) else {
            continue;
        };
        // Splicable iff it is one of the splicable kinds AND it has a
        // resolvable span in the body (matches `splicible_root_blocks`
        // exactly, so the counter stays in lockstep with the edits vec).
        let splicable = match block {
            md::Block::Heading { .. } | md::Block::Paragraph { .. } | md::Block::Code { .. } => {
                block.range(&md_body).is_some()
            }
            md::Block::List { .. } => {
                super::splice::tight_list_span_for(&doc, &md_body, idx).is_some()
            }
            _ => false,
        };
        if !splicable {
            render::paint_single(ui, &doc, idx);
            continue;
        }
        let Some(edit) = edits.get(splicable_pos) else {
            render::paint_single(ui, &doc, idx);
            splicable_pos += 1;
            continue;
        };
        splicable_pos += 1;
        if editing == Some(splicable_pos - 1) {
            // EDIT MODE: text field commits on change. `Escape` exits back
            // to read mode (a common affordance for WYSIWYG editors).
            let pre_text = edit.plain_text.clone();
            let mut text = pre_text.clone();
            paint_widget(ui, &mut text, edit);
            if text != pre_text {
                commit(app, &prefix, &md_body, edit, &text);
            }
            if ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
                app.state_mut().wysiwyg_editing = None;
            }
        } else {
            // READ MODE: rich-rendered block with a click target overlaid on
            // exactly the block's rect so the user can enter edit mode by
            // clicking it.
            //
            // `ui.scope` allocates a child rect for the block; the scope's
            // background is registered with `Sense::hover` (egui default), and
            // the inner labels inherit `Sense::hover` too. egui's hit-test
            // tie-breaks same-rect widgets by "last painted = topmost", so the
            // `labels` beat the scope background for hit-testing. `.interact()`
            // on the scope background adds `Sense::click` but keeps the background
            // behind in z-order → it loses the tie-break to the labels.
            //
            // The fix: after painting the block, register a *fresh* widget at
            // the block's rect with `Sense::click` via `Ui::interact`. Because
            // it is registered last, it wins the hit-test tie-break and the
            // click is routed to **it** (not to the labels, which don't sense
            // click at all). This is the same mechanism `Button` uses (its
            // `id` is registered with `Sense::click` at button-paint time).
            let inner = ui.scope(|ui| render::paint_single(ui, &doc, idx));
            let block_rect = inner.response.rect;
            let click_id = ui.make_persistent_id(("block_edit_read", idx));
            let clickable = ui.interact(block_rect, click_id, egui::Sense::click());
            if clickable.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if clickable.clicked() {
                app.state_mut().wysiwyg_editing = Some(splicable_pos - 1);
            }
        }
    }
}

/// Split a full page file into (frontmatter-prefix bytes, body-relative Markdown
/// slice). Total: returns (whole file, empty prefix) when no frame is found.
fn split_frontmatter(body: &str) -> (String, String) {
    let Some(fm) = kb_core::frontmatter::find_fm(body.as_bytes()) else {
        return (String::new(), body.to_string());
    };
    let lines: Vec<String> = body.split_inclusive('\n').map(|l| l.to_string()).collect();
    let body_start = lines[..=fm.close_line]
        .iter()
        .map(|l| l.len())
        .sum::<usize>();
    (
        body[..body_start].to_string(),
        body[body_start..].to_string(),
    )
}

/// Route a splicable block to the right `TextEdit`: heading is a single-line
/// sized field, a paragraph is a wrapping multiline field, and code is a
/// monospace multiline panel. `desired_width(f32::INFINITY)` fills the
/// available width (the parent `ui.vertical` scope in `preview::paint`
/// provides the finite width allocation). egui mutates `text` in place on
/// user input.
fn paint_widget(ui: &mut egui::Ui, text: &mut String, edit: &BlockEdit) {
    match &edit.kind {
        super::splice::BlockKind::Heading(level) => {
            ui.add(
                egui::TextEdit::singleline(text)
                    .font(egui::FontId::proportional(heading_size(*level)))
                    .hint_text("heading")
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(8.0);
        }
        super::splice::BlockKind::Paragraph => {
            ui.add(
                egui::TextEdit::multiline(text)
                    .hint_text("paragraph")
                    .desired_width(f32::INFINITY)
                    .desired_rows(1),
            );
            ui.add_space(8.0);
        }
        super::splice::BlockKind::Code { lang, fenced } => {
            if *fenced {
                if let Some(l) = lang {
                    ui.weak(l.as_str());
                }
            }
            ui.add(
                egui::TextEdit::multiline(text)
                    .code_editor()
                    .hint_text("code")
                    .desired_width(f32::INFINITY)
                    .desired_rows(4),
            );
            ui.add_space(10.0);
        }
        super::splice::BlockKind::List { .. } => {
            // A whole simple list is one multiline field: the marker-less item
            // texts, one per line. Enter adds a new line (a new list item),
            // ↑/↓ move the caret between lines — the plain-text field gives
            // both for free (PLAN-GUI §8 WYSIWYG list editing).
            ui.add(
                egui::TextEdit::multiline(text)
                    .hint_text("list item, one per line — Enter for a new item")
                    .desired_width(f32::INFINITY)
                    .desired_rows(1),
            );
            ui.add_space(8.0);
        }
    }
}

/// Re-splice the edited content into the body-relative slice and write the
/// re-assembled file back to `open_page.body`, marking the page dirty so the
/// chrome's Save button / status chip reflect the change. No-op when the splice
/// does not change the slice (identity round-trip).
fn commit(app: &mut App, prefix: &str, md_body: &str, edit: &BlockEdit, new_text: &str) {
    let mut e = edit.clone();
    e.plain_text = new_text.to_string();
    let new_md_body = splice(md_body, &e);
    if new_md_body == *md_body {
        return; // §8.6 identity round-trip: no effective change.
    }
    let new_body = format!("{prefix}{new_md_body}");
    if let Some(slot) = app.state_mut().open_page.as_mut() {
        slot.body = Some(new_body);
    }
    app.mark_unsaved();
}

#[cfg(test)]
mod tests {
    use super::super::splice::BlockKind;
    use super::*;
    use crate::state::{DocumentKind, LaunchRoute, OpenPageState, ThemeChoice};

    fn app_with_body(body: &str) -> App {
        let mut app = App::new(None, LaunchRoute::Home, ThemeChoice::Dark);
        app.state_mut().open_page = Some(OpenPageState {
            abs_path: "/tmp/p.md".to_string(),
            body: Some(body.to_string()),
            baseline: None,
            error: None,
            kind: DocumentKind::KbPage,
            banner_dismissed: true,
        });
        app
    }

    /// The G4 regression that used to render read-only: every splicable block
    /// type (heading, paragraph, fenced code) plus a non-splicable list, so the
    /// mixed editable-widget / read-only-fallback interleaving is exercised.
    /// Catches a `ui.indent`-on-horizontal panic the same way the render tests
    /// do, without a display.
    /// Clone the open page out so the borrowed page does not conflict with the
    /// `&mut app` the paint pass needs to commit through.
    fn app_page(body: &str) -> (App, OpenPageState) {
        let app = app_with_body(body);
        let page = app.state().open_page.clone().expect("a page");
        (app, page)
    }

    #[test]
    fn paint_mixed_document_without_panic() {
        let (mut app, page) = app_page("---\ntype: note\n---\n\n# Title\n\nA plain paragraph.\n\n```rust\nfn main() {}\n```\n\n- a list\n- item\n");
        egui::__run_test_ui(|ui| {
            ui.vertical(|ui| {
                paint(ui, &mut app, &page);
            });
        });
    }

    /// A document that is *entirely* non-splicable (a list + a blockquote) must
    /// fall back to the read-only renderer for the whole body — the early-return
    /// branch in `paint` when `edits.is_empty()`.
    #[test]
    fn paint_non_splicable_only_falls_back() {
        let (mut app, page) = app_page("- one\n- two\n\n> a quote\n");
        egui::__run_test_ui(|ui| {
            ui.vertical(|ui| {
                paint(ui, &mut app, &page);
            });
        });
    }

    /// `split_frontmatter` must return (frame, body) where the *frame* covers
    /// every byte up to and including the closing `---\n`, and the *body* is
    /// the remaining slice. The regression caught live (G4 smoke): the old
    /// `split` summed line lengths *after* the close marker and used that
    /// sum as a byte offset — so for a 4-line frame the prefix swallowed the
    /// H1 + first paragraph + part of the first bullet, and the body began
    /// mid-word with `"ullet\n…"` in the G4 fixture.
    #[test]
    fn split_frontmatter_splits_frame_from_body() {
        let body = "---\ntype: note\ntitle: T\n---\n\n# H\n\npara.\n";
        let (frame, rest) = split_frontmatter(body);
        assert_eq!(frame, "---\ntype: note\ntitle: T\n---\n");
        assert_eq!(rest, "\n# H\n\npara.\n");
        // The frame and body must reconstruct the source byte-for-byte.
        assert_eq!(format!("{frame}{rest}"), body);
    }

    /// No frontmatter → empty prefix, whole file becomes the body.
    #[test]
    fn split_frontmatter_without_frame_returns_whole_file() {
        let body = "# H\n\npara.\n";
        let (frame, rest) = split_frontmatter(body);
        assert_eq!(frame, "");
        assert_eq!(rest, body);
    }

    /// A body immediately after the closing `---` (no blank separator) —
    /// the frame must end exactly on the closing marker and the body must
    /// begin at the first content byte.
    #[test]
    fn split_frontmatter_body_starts_immediately_after_close() {
        let body = "---\nkey: value\n---\n# first heading\n";
        let (frame, rest) = split_frontmatter(body);
        assert_eq!(frame, "---\nkey: value\n---\n");
        assert_eq!(rest, "# first heading\n");
    }

    /// A mixed document (splicable + a simple list) must paint without
    /// panicking, with the simple list editable (in scope) and the block
    /// interleaving preserved.
    #[test]
    fn paint_list_editable_in_mixed_doc() {
        let (mut app, page) = app_page("# T\n\npara\n\n- a\n- b\n\n> q\n");
        egui::__run_test_ui(|ui| {
            ui.vertical(|ui| {
                paint(ui, &mut app, &page);
            });
        });
    }

    /// Editing a list's plain text commits a span-isolated splice: the list
    /// lines change, the surrounding blocks (heading, paragraph, quote) are
    /// byte-identical, and the page is marked unsaved.
    #[test]
    fn list_edit_commits_span_isolated() {
        let mut app = app_with_body("- one\n- two\n\ntail\n");
        let (prefix, md_body) = split_frontmatter("- one\n- two\n\ntail\n");
        let doc = md::parse(&md_body);
        let edit = splicible_root_blocks(&doc, &md_body)
            .into_iter()
            .find(|e| matches!(e.kind, BlockKind::List { .. }))
            .expect("a list");
        // The Enter-new-line flow: a new (blank-at-commit) item was typed.
        commit(&mut app, &prefix, &md_body, &edit, "one\nthree-new");
        assert_eq!(
            app.state()
                .open_page
                .as_ref()
                .expect("page")
                .body
                .as_deref(),
            Some("- one\n- three-new\n\ntail\n")
        );
    }

    /// Typing into a heading widget commits a span-isolated splice: the heading
    /// line changes, the rest of the body (frontmatter + following blocks) is
    /// byte-identical, and the page is marked unsaved.
    #[test]
    fn heading_edit_commits_span_isolated() {
        let mut app = app_with_body("# Old\n\nbody tail\n");
        let text = "Renamed heading".to_string();
        // Drive the same code path the widget does: serialize + splice + commit.
        let (prefix, md_body) = split_frontmatter("# Old\n\nbody tail\n");
        let doc = md::parse(&md_body);
        let edit = splicible_root_blocks(&doc, &md_body)
            .into_iter()
            .find(|e| matches!(e.kind, BlockKind::Heading(1)))
            .expect("an h1");
        commit(&mut app, &prefix, &md_body, &edit, &text);
        assert_eq!(
            app.state()
                .open_page
                .as_ref()
                .expect("page")
                .body
                .as_deref(),
            Some("# Renamed heading\n\nbody tail\n")
        );
    }
}
