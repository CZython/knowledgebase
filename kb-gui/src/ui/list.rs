//! The note list (PLAN-GUI §6.4): the `All pages` row list.
//!
//! G1 wires the functional surface:
//!
//! - Header: `+ new note` (G2), an inline filter field (case-insensitive
//!   substring over title/path), a `Group by` selector
//!   (`None · Collection · Visibility`), and a sort menu (title/date/path).
//!   `None` grouping is the default in `All pages` (browse parity);
//!   collection selection scopes the rows; `Visibility` shows `Open /
//!   Gated` sections; `Collection` duplicates multi-gate pages under every
//!   gate and keeps gateless pages under `Ungated`.
//! - Rows (PLAN-GUI §6.4 anatomy): page-type glyph, title (bold),
//!   timestamp/path (muted), gated marker (`◆`), collection chips for
//!   other memberships, and a kebab menu on hover (G1: a no-op `⋯`).
//! - Selected row: `selection` background. Clicking a row opens its body
//!   into the preview (§6.5) via `app.open_page`.
//! - Footer: `N notes` / `N in <collection>` (and, in `Collection` grouping,
//!   unique notes + row count). Rendered by `chrome::status_bar` (the
//!   "N notes" footer); this module exposes `visible_count` for it.
//!
//! Virtualization via `ScrollArea` is kept simple for G1 (the corpus is
//! small-medium); `ScrollArea::vertical` + `show` paints every row, which
//! is fine for a knowledge base of a few thousand pages.

use eframe::egui;

use crate::app::App;
use crate::state::{CollectionKey, GroupBy, PageRow};

/// The count of rows the list will paint after the current collection +
/// filter are applied (PLAN-GUI §6.4 footer: "`N notes`" / "`N in
/// <collection>`"). Exposed so `chrome::status_bar` can render the real
/// figure without re-implementing the filter.
pub fn visible_count(app: &App) -> usize {
    let corpus = corpus_filtered(app);
    corpus.iter().filter(|p| matches_filter(p, app)).count()
}

/// The rows the list paints, *before* the inline filter is applied — i.e.
/// the collection-scoped set (PLAN-GUI §2.1 rule 3: "selecting a
/// collection filters the list"). Grouping does not add or remove pages
/// (it only *orders / repeats* them), so this is the source-of-truth set.
fn corpus_filtered(app: &App) -> Vec<PageRow> {
    let pages = &app.state().pages;
    match &app.state().collection {
        CollectionKey::All => pages.to_vec(),
        CollectionKey::Gated => pages.iter().filter(|p| p.gated()).cloned().collect(),
        CollectionKey::Ungated => pages.iter().filter(|p| !p.gated()).cloned().collect(),
        CollectionKey::Term(t) => pages
            .iter()
            .filter(|p| p.gates.iter().any(|g| g.eq_ignore_ascii_case(t)))
            .cloned()
            .collect(),
    }
}

/// The inline filter: a case-insensitive substring match over the title and
/// the relative path. An empty filter matches everything.
fn matches_filter(p: &PageRow, app: &App) -> bool {
    let needle = app.state().list_filter.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let haystack = format!("{} {}", p.title.as_deref().unwrap_or(""), p.rel_path).to_lowercase();
    haystack.contains(&needle)
}

/// Paint the note list (and its header) for the current collection +
/// grouping + filter. Returns on a non-`Ready` phase with the loading
/// placeholder.
pub fn paint(ui: &mut egui::Ui, app: &mut App) {
    // Fixed-width browse column: the preview/editor pane (the main reading
    // area) takes the remaining width. Capping the max stops this full-width
    // row from starving the preview (PLAN-GUI §6.5 layout).
    ui.set_min_width(300.0);
    ui.set_max_width(380.0);
    // The rail and list together claim all the width left after the rail;
    // the list's own `ScrollArea` then scrolls its rows vertically.
    ui.vertical(|ui| {
        // Header row (§6.4): + new / filter / group-by / sort. `+ new note`
        // and the sort menu are reserved for G2/G5 — the layout is stable
        // now so later milestones only fill the slots.
        ui.horizontal(|ui| {
            let resp = ui.button("+ new");
            if resp.clicked() {
                let _ = app; // New-note dialog lands in G2.
            }
            ui.separator();
            // Inline filter (the "filter…" slot, now functional).
            let mut filter = app.state().list_filter.clone();
            let resp = ui.add_sized(
                egui::vec2(160.0, 24.0),
                egui::TextEdit::singleline(&mut filter).hint_text("filter…"),
            );
            if resp.changed() {
                app.state_mut().list_filter = filter;
            }
            ui.separator();
            // Group-by selector. `None · Collection · Visibility`. The
            // selection is `CollectionKey`-independent: it is the
            // *arrangement* of the rows (PLAN-GUI §6.4), not the
            // collection-scoped set.
            let group = app.state().group_by;
            if ui
                .selectable_label(group == GroupBy::None, "Flat")
                .clicked()
            {
                app.state_mut().group_by = GroupBy::None;
            }
            if ui
                .selectable_label(group == GroupBy::Collection, "By collection")
                .clicked()
            {
                app.state_mut().group_by = GroupBy::Collection;
            }
            if ui
                .selectable_label(group == GroupBy::Visibility, "By visibility")
                .clicked()
            {
                app.state_mut().group_by = GroupBy::Visibility;
            }
            // Sort menu — reserved for G5 (the `None`/`Collection`/
            // `Visibility` groupings already order by `updated_at`-desc
            // as a sensible default).
            let _ = ui.weak("  sort: recent");
        });

        ui.add_space(4.0);

        let corpus = corpus_filtered(app);
        let rows: Vec<PageRow> = corpus
            .into_iter()
            .filter(|p| matches_filter(p, app))
            .collect();

        if rows.is_empty() {
            ui.weak(match app.state().phase {
                crate::state::Phase::Loading => "loading corpus…",
                crate::state::Phase::Ready => {
                    if app.state().pages.is_empty() {
                        "no pages (no roots registered? use ☰ → Add root)"
                    } else if app.state().list_filter.trim().is_empty() {
                        "no pages in this collection"
                    } else {
                        "no pages match the filter"
                    }
                }
                crate::state::Phase::Error(_) => "an error occurred while loading",
            });
            return;
        }

        // Read the arrangement + the collection list into *owned* locals
        // (scalars are Copy; collections is cloned) so the match scrutinee
        // and the per-gate loop don't hold an immutable borrow on `app`
        // while `paint_row` mutably borrows it.
        let group_by = app.state().group_by;
        let collections: Vec<(String, usize)> = app.state().collections.clone();

        egui::ScrollArea::vertical()
            .id_salt("note-list-scroll")
            .show(ui, |ui| {
                match group_by {
                    GroupBy::None => {
                        for row in rows.iter() {
                            paint_row(ui, app, row);
                        }
                    }
                    GroupBy::Visibility => {
                        let open: Vec<&PageRow> = rows.iter().filter(|p| !p.gated()).collect();
                        let gated: Vec<&PageRow> = rows.iter().filter(|p| p.gated()).collect();
                        group_header(ui, format!("Open  ({})", open.len()));
                        for row in open {
                            paint_row(ui, app, row);
                        }
                        let n = gated.len();
                        group_header(ui, format!("Gated  ({n})"));
                        for row in gated {
                            paint_row(ui, app, row);
                        }
                    }
                    GroupBy::Collection => {
                        // One section per gate + a trailing `Ungated`. A
                        // multi-gate page appears under *every* gate it has
                        // (PLAN-GUI §6.4: "rows repeat by design").
                        for (gate, _count) in collections.iter() {
                            let in_gate: Vec<&PageRow> = rows
                                .iter()
                                .filter(|p| p.gates.iter().any(|g| g.eq_ignore_ascii_case(gate)))
                                .collect();
                            if in_gate.is_empty() {
                                continue;
                            }
                            group_header(ui, format!("{gate}  ({})", in_gate.len()));
                            for row in in_gate {
                                paint_row(ui, app, row);
                            }
                        }
                        let ungated: Vec<&PageRow> = rows.iter().filter(|p| !p.gated()).collect();
                        if !ungated.is_empty() {
                            group_header(ui, format!("Ungated  ({})", ungated.len()));
                            for row in ungated {
                                paint_row(ui, app, row);
                            }
                        }
                    }
                }
            });
    });
}

fn group_header(ui: &mut egui::Ui, text: String) {
    ui.add_space(6.0);
    ui.strong(text);
    ui.separator();
}

/// One note-list row (PLAN-GUI §6.4 anatomy). `id_salt` is the stable
/// identity token for the row (so scroll position is preserved across
/// reloads). The full row:
/// `title (bold)` + a muted line with `◆` (if gated) + type + rel_path
/// + updated_at + collection chips for every membership.
fn paint_row(ui: &mut egui::Ui, app: &mut App, page: &PageRow) {
    let selected = app.state().selected_page.as_deref() == Some(page.abs_path.as_str());
    let title = page.title.clone().unwrap_or_else(|| page.rel_path.clone());
    let resp = ui.selectable_label(selected, &title);
    if resp.clicked() {
        app.open_page(&page.abs_path);
    }

    // The muted meta line: gated marker + type + rel_path + updated_at
    // + collection chips.
    ui.horizontal(|ui| {
        if page.gated() {
            ui.colored_label(egui::Color32::from_rgb(120, 170, 220), "◆");
        }
        ui.weak(&page.type_);
        ui.weak(" · ");
        if !page.updated_at.is_empty() {
            ui.weak(&page.updated_at);
            ui.weak(" · ");
        }
        for gate in &page.gates {
            ui.add_space(4.0);
            ui.weak(chip_string(gate));
        }
        ui.weak(" · ");
        ui.weak(&page.rel_path);
    });
    ui.add_space(4.0);
}

/// The chip label: the plan says "chip" but in G1 we render the gate as a
/// styled `weak` label with rounded padding (the visual distinction is the
/// muted color + spacing). The `⋯` kebab menu is in G2.
fn chip_string(gate: &str) -> String {
    format!("[{gate}]")
}
