//! `kb tui` — the ratatui render pass (M8.1: Browse + Read · M8.2: Edit + autocomplete).
//!
//! The render code is the *assumed* side: it lays out the screen, paints
//! rows, colors the status bar. It holds no *proof*; the invariant that
//! what it renders is well-formed lives on the App state it reads
//! (`scroll_top` is always in range — clamped by the scroll mutators; the
//! `view` list is always in sync with `filter_text` — enforced by
//! `apply_filter`). The render pass takes a *window* of the body from
//! `scroll_top`, clips to the available height, and leaves it to the
//! terminal to display.
//!
//! Layouts:
//!
//! Browse:
//! ```text
//!  kb tui — browse            ← mode + (the live filter, if any)
//! ┌──────────────────────────────────────────────────────┐
//! │ ▶ note:P3  Transformer  wiki/a.md                     │
//! │   note:P7  Attention   wiki/a.md                      │
//! └──────────────────────────────────────────────────────┘
//!  status bar (colored by severity)
//! ```
//!
//! Search (M8.4 — the real `search_grammar` FTS5 engine, not a substring
//! filter; `s` from Browse):
//! ```text
//!  kb tui — search · 3 result(s) · 2 tag hint(s)  (↑↓ move · Tab inject tag · Enter open · Esc back)
//! ┌ results (live) ──────────────────┬─ tag hints ────┐
//! │ > note:T   Transformer  wiki/a.md │ llm → ai       │
//! │   note:A   Attention   wiki/a.md  │ llm            │
//! └──────────────────────────────────┴────────────────┘
//!  status bar (colored by severity)
//! ```
//!
//! Filter input (the header shows the live filter + "editing"):
//! ```text
//!  kb tui — browse · filter "abc" (editing: / commit, Esc clear)
//! ┌ same list, live-filtered as you type ┐
//! ```
//!
//! Read:
//! ```text
//!  kb tui — reading note:P3  Attention            ← page identity
//! ├── body (scroll-aware)──────────────┬── outline ──┐
//! │   # Transformer                    │ § # Transformer │
//! │   The transformer is a ...         │ §  ## Attention │
//! │   ...                              │ §  ## Decoder   │
//! └────────────────────────────────────┴─────────────────┘
//!  status bar (colored by severity)
//! ```

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List as RatList, ListItem, ListState, Paragraph, Widget, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::{App, FmMeta, LinkCandOption, LinksEntry, LinksState, Mode, PageRow, ReadState, StatusKind};
use crate::tui::palette::Palette;

/// Draw the whole screen for `app`. Called once per event-loop tick.
/// Takes a **mutable** reference: the render pass writes the body pane
/// inner width back to `app.body_pane_width` *before* dispatching (the
/// scroll mutators in `mod.rs` read it on the *next* tick — the event loop
/// draws before it polls a key, so the value is always current).
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    // Write the body-pane inner width to `app` (used by scroll mutators).
    // In Links mode the body pane is the *left* 60% panel (the right 40%
    // holds the target info), so its effective inner width is narrower
    // than the full Read body. We approximate it at dispatch time and the
    // render pass (`draw_links`) refines it from the layout split.
    let in_links = matches!(&app.mode, Mode::Links(_));
    let has_outline = matches!(&app.mode, Mode::Read(rs) if !rs.outline.is_empty());
    let base = body_inner_width(area, has_outline);
    app.body_pane_width = if in_links {
        // 60% of the *content* area (the body pane is the 60% column
        // minus its ALL-side borders — 2 rows × 4 = ~4 cols).
        (base * 6 / 10).saturating_sub(4).max(1)
    } else {
        base
    };
    // Write the body-pane inner height to `app` (used by find-centering math).
    // The body pane's height = total area height minus the bordered rows (2),
    // minus the header (1), minus the status (1). `draw_read` overwrites this
    // every frame with the exact value from the layout split.
    app.body_pane_height = area.height.saturating_sub(4) as usize; // rough upper bound
    // Pre-warm the body roll-up *before* the immutable read pass below. `draw`
    // holds `&mut App`; the match arm immediately takes a `&app` borrow, so we
    // must issue the one `&mut App` write (the roll-up build/update) here, in
    // `draw`'s scope, before the borrow. Cheap on a hit (no body or width
    // change); O(n) on a miss (new page or window resize).
    // The roll-up measures the *spliced* body (the text the user sees) —
    // the same splice `draw_read` paints — so the scroll clamp, the find
    // search and the centering math all agree with what's on screen. The
    // splice is cheap (line-by-line string replace, only the resolved links
    // change) and the roll-up is memoized on the spliced text's (hash, width),
    // so a re-open of the same page is an O(1) hit. Splice + flatten to a
    // single string (the form `ensure_body_rollup` consumes).
    let (body, in_read) = match &app.mode {
        Mode::Read(rs) => {
            let spliced = splice_body(&rs.body, &rs.links);
            (spliced.join("\n"), true)
        }
        _ => (String::new(), false),
    };
    if in_read {
        app.ensure_body_rollup(&body);
    }

    match &app.mode {
        Mode::Browse => draw_browse(f, app, area),
        Mode::Read(rs) => draw_read(f, app, rs, area),
        Mode::Edit(es) => draw_edit(f, app, es, area),
        Mode::Alias(ast) => draw_alias(f, app, ast, area),
        Mode::Roots(rs) => draw_roots(f, app, rs, area),
        Mode::Search(ss) => draw_search(f, app, ss, area),
        Mode::Links(ls) => draw_links(f, app, ls, area),
    }
}

fn draw_browse(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // list
            Constraint::Length(1), // status
        ])
        .split(area);

    let header = header_line(app);
    let header_widget = Paragraph::new(header)
        .style(app.palette.label);
    f.render_widget(header_widget, chunks[0]);

    // Each row is `<gate marker> <id> <rel_path>` fitted to the pane's
    // inner width (the ALL-side borders eat one cell per side). The gate
    // marker (◆ / ◇) is *double-encoded*: as a symbol and as a palette
    // role, so it survives even a colorblind or user-recolor palette.
    let row_width = chunks[1].width.saturating_sub(2) as usize;
    let p = &app.palette;
    let items: Vec<ListItem> = app
        .view
        .iter()
        .map(|r| {
            let spans = browse_row(p, r, row_width);
            ListItem::new(Line::from(spans))
        })
        .collect();

    // The keybinding legend lives in the **status bar** (bottom), not the
    // pane title. The title is identity + count only — this is what removes
    // the duplicate/truncated legend we saw in the captures (title AND bar
    // each used to print the same hints, both clipping).
    let title = browse_pane_title(app.view.len(), app.rows.len(), &app.filter_text);
    // White-on-black (inverted default) so the selected row stands out
    // regardless of the terminal's own fg/bg colors.
    let widget = RatList::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(app.palette.selection);

    let mut list_state = ListState::default();
    if !app.view.is_empty() {
        list_state.select(Some(app.selected));
    }
    f.render_stateful_widget(widget, chunks[1], &mut list_state);

    render_status_bar(f, app, chunks[2]);
}

/// Compute the inner width of the Read body pane (the value `draw_read`
/// passes to `Paragraph` after the two-cell border subtract). The layout
/// math is a pure function of the terminal `area` and whether an outline
/// pane is present — no state, no IO.
fn body_inner_width(area: ratatui::layout::Rect, has_outline: bool) -> usize {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),   // panes
            Constraint::Length(1),// status
        ])
        .split(area);
    let pane_constraints = if has_outline {
        [Constraint::Percentage(60), Constraint::Percentage(40)]
    } else {
        [Constraint::Percentage(100), Constraint::Length(0)]
    };
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(pane_constraints)
        .split(chunks[1]);
    (panes[0].width as usize).saturating_sub(2)
}

/// The 1-line header for the Browse mode. Reflects the current filter
/// state (so a user who typed `/abc` and is still in the input context
/// sees "filter abc (editing…)" — the *visual* confirmation that the
/// input is live), plus the live-matches count.
fn header_line(app: &App) -> String {
    let mode = "browse";
    if app.filter_active {
        format!("  kb tui — {mode} · filter '{}' (editing: / to commit, Esc to clear)", app.filter_text)
    } else if app.filter_text.is_empty() {
        format!("  kb tui — {mode}")
    } else {
        format!("  kb tui — {mode} · filter '{}' ({} match(es))", app.filter_text, app.view.len())
    }
}

fn draw_read(f: &mut Frame, app: &App, rs: &ReadState, area: ratatui::layout::Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // panes
            Constraint::Length(1), // status
        ])
        .split(area);

    // The header now carries the **absolute file path** (issue 3): users
    // want the file location up top so they can `cd` / `copy` / `git add`
    // from a shell prompt without re-typing it. The id remains for the
    // "which kb page" identity (a root may have multiple pages under the
    // same name), and the row count is the corpus position (a `full-rebuild`
    // may reassign rowids, so this is display-only — identity lives on
    // `abs_path`).
    //
    // Fitting the header: the pane is `area.width` wide. After the two-cell
    // left/right borders, the inner is `area.width.saturating_sub(2)`. I
    // build `<id> — <abs_path>` and let the terminal truncate; the id
    // stays visible (it comes first) and the path fills the rest. A CJK
    // path is wider than its byte length; we don't need to be exact here —
    // the terminal's own word-truncate handles the overflow for free.
    let header_text = format!("  kb tui — reading {}  {}  (row {} of {})",
        rs.id, rs.abs_path, rs.row_idx + 1, app.rows.len());
    let header = Paragraph::new(header_text)
        .style(app.palette.label);
    f.render_widget(header, chunks[0]);

    // The panes row: body | [frontmatter + outline, or body only].
    let p = &app.palette;
    let fm_lines = fm_meta_lines(p, &rs.fm);
    let has_fm = !fm_lines.is_empty();
    let has_outline = !rs.outline.is_empty();
    let has_right = has_fm || has_outline;
    let pane_constraints = if has_right {
        [Constraint::Percentage(60), Constraint::Percentage(40)]
    } else {
        [Constraint::Percentage(100), Constraint::Length(0)]
    };
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(pane_constraints)
        .split(chunks[1]);

    // Body pane: render the FULL body in a `Paragraph` with `Wrap{trim:false}`
    // and `Paragraph::scroll((scroll_top, 0))`. The `Paragraph` reflow
    // (its `WordWrapper`) is the *exact* wrap engine the terminal sees, so
    // the scroll offset unit matches what the user perceives as one row
    // on screen. One press of ↓ advances `scroll_top` by 1 (one rendered
    // row, not one logical line) — the word-wrapping "stutter" is gone.
    //
    // P7: ATX headings (`#` through `######`) render in the `p.label` role;
    // every other line stays in the default `p.default` role.
    //
    // **Read-find overlay:** when a find session is armed (with ≥1 match),
    // every needle occurrence is highlighted — non-selected hits in
    // `p.find_match`, the selected hit (the one the viewport is
    // centered on) in `p.find_current` — and blank *padding* rows are
    // prepended / appended above / below the body so the selected hit can
    // sit at the pane's center even when it is on the very first or last
    // rendered row. The padding counts + scroll are the `FindPadding` the
    // input pass already computed (FindState.center), so render and input
    // can't desync.
    // Splice the body against its links (the *identical* text the Links
    // view paints — only the focus highlight differs there). This is the
    // text the user sees, so every downstream consumer (scroll clamp, find
    // search, centering) must measure it. The roll-up is pre-warmed in
    // `draw` against this same spliced body, so `total_rows` is O(1); the
    // defensive fallback re-runs the measure here.
    let spliced = splice_body(&rs.body, &rs.links);
    let rendered: Vec<String> = spliced.iter().map(|s| rendered_line(p, s)).collect();
    let content_rows = if app.body_rollup.valid {
        app.body_rollup.total_rows
    } else {
        // Defensive fallback: the pre-warm should always have run for a Read
        // frame, but belt-and-braces in case a future refactor reorders the
        // calls. One extra O(n) pass, never a panic.
        crate::tui::render::rendered_row_count(&rendered, app.body_pane_width)
    };
    let (pad_above, pad_below) = rs
        .find
        .as_ref()
        .and_then(|fs| fs.center.as_ref())
        .map(|c| (c.above, c.below))
        .unwrap_or((0, 0));
    // The selected hit's (line_idx, byte_offset) — the one match rendered
    // in `find_current` (all others in `find_match`). `None` when there is
    // no match. Matches are byte offsets into the *spliced* rendered line
    // (see `find_center_scroll`, which searches the spliced body).
    let selected: Option<(usize, usize)> = rs
        .find
        .as_ref()
        .and_then(|fs| {
            if fs.matches.is_empty() { None } else {
                Some(fs.matches[fs.cursor.min(fs.matches.len() - 1)])
            }
        });
    let has_find = rs.find.as_ref().map(|fs| !fs.matches.is_empty()).unwrap_or(false);
    let needle_len = rs.find.as_ref().map(|fs| fs.needle.len()).unwrap_or(0);

    let blank = Line::from(vec![Span::styled(String::new(), p.default)]);
    let mut body_lines: Vec<Line<'static>> = Vec::with_capacity(pad_above + spliced.len() + pad_below);
    for _ in 0..pad_above {
        body_lines.push(blank.clone());
    }
    for (li, l) in spliced.iter().enumerate() {
        // Base styling = markdown emphasis + heading role of the spliced text,
        // patched with the link-state styles (both views apply the same base;
        // the Read view passes `focused_key = None`). `apply_find_patched`
        // last-wins per char, so we append the find patches *after* the link
        // patches: an active find overlay (find_match / find_current) takes
        // priority over the link's base style on the same glyph.
        //
        // NOTE: the offsets are computed on the *rendered* line (markdown
        // emphasis stripped), which is the exact text `read_line` produces —
        // passing `rendered[li]` (not the spliced `l`) keeps the byte offsets
        // aligned. `is_heading` sees the same leading `#` either way.
        let rendered_line = rendered.get(li).map(|s| s.as_str()).unwrap_or("");
        let mut patches: Vec<(usize, usize, Style)> = line_patches(p, rendered_line, &rs.links, li, None);
        if has_find {
            let ms: Vec<(usize, usize)> = rs.find.as_ref().unwrap().matches.iter()
                .filter_map(|(mli, off)| {
                    if *mli == li { Some((*off, *off + needle_len)) } else { None }
                })
                .collect();
            let sel = selected;
            for (s, e) in ms {
                let is_sel = sel.map(|(sl, so)| sl == li && so == s).unwrap_or(false);
                patches.push((s, e, if is_sel { p.find_current } else { p.find_match }));
            }
        }
        body_lines.push(apply_find_patched(read_line(p, l), &patches));
    }
    for _ in 0..pad_below {
        body_lines.push(blank.clone());
    }

    // The "row N of M" note in the body pane title now reports rendered
    // rows (what the user sees), not logical lines. `scroll_top` is the
    // *first* rendered row shown, so `+1` gives a 1-based "row N" the user
    // can quote in a bug report. With find armed, the title doubles as the
    // find status (needle + hit position) so the user always sees what's
    // highlighted. While find is armed, `l` is typed into the needle, so the
    // picker affordance moves to `Tab`.
    let total_rows = pad_above + content_rows + pad_below;
    let title = if has_find {
        let fs = rs.find.as_ref().unwrap();
        let m = fs.matches.len();
        let pos = fs.cursor.min(m - 1) + 1;
        format!(
            " body — find '{}': {pos} of {m} · ↑/↓ move · Tab links · Esc close",
            fs.needle
        )
    } else {
        format!(
            " body — row {} of {} (jk/PgUp/Dn, g/G, f find, l links, Esc back)",
            rs.scroll_top + 1,
            total_rows,
        )
    };
    let body_widget = Paragraph::new(body_lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((rs.scroll_top as u16, 0));
    f.render_widget(body_widget, panes[0]);

    if has_right {
        // Right pane: frontmatter block on top (fixed rows), outline below
        // (variable, takes the rest). Each sub-pane is a bordered widget, so
        // the user can visually separate the two.
        let mut sub_constraints: Vec<Constraint> = Vec::new();
        if has_fm {
            // The bordered block needs +1 (top border) row beyond the text rows;
            // the bottom border rides on the next chunk's top border or the
            // pane's edge, so `1 + len` is the guaranteed minimum.
            sub_constraints.push(Constraint::Length(2 + fm_lines.len() as u16));
        }
        if has_outline {
            sub_constraints.push(Constraint::Min(3));
        }
        let right_sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints(sub_constraints)
            .split(panes[1]);

        if has_fm {
            let fm_widget = Paragraph::new(fm_lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" frontmatter"),
            );
            f.render_widget(fm_widget, right_sub[0]);
        }

        if has_outline {
            let outline_area = if has_fm { right_sub[1] } else { right_sub[0] };
            // A `List`, not a `Paragraph`: each heading stays on **one row** and
            // never wraps mid-word (the old `Paragraph` + `Wrap{trim:false}` broke
            // `L3  H2 section` into two visible lines in narrow panes). Rows are
            // indented per heading level; the `L<n>` anchor is dimmed (the title
            // is the focus) and `¶` marks an empty-title heading (same convention
            // as the body's `read_line`).
            let rows: Vec<ratatui::widgets::ListItem> = rs
                .outline
                .iter()
                .map(|(line, level, title)| {
                    ratatui::widgets::ListItem::new(outline_line(p, *line, *level as usize, title))
                })
                .collect();
            let outline_widget = RatList::new(rows).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" outline — {} section(s)", rs.outline.len())),
            );
            f.render_widget(outline_widget, outline_area);
        }
    }

    render_status_bar(f, app, chunks[2]);
}

/// Render the frontmatter metadata as a list of Lines for the Read right pane.
/// Empty input (all fields absent) → an empty vec (the caller hides the pane).
/// Fields are rendered in a canonical order: type → language → description →
/// tags → gates (search_required_phrase). Missing fields are skipped; the
/// user sees only what the page actually has.
fn fm_meta_lines(p: &Palette, fm: &FmMeta) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    fn kv(out: &mut Vec<Line<'static>>, p: &Palette, k: &str, v: &str) {
        out.push(Line::from(vec![
            Span::styled(format!(" {k:<13}"), p.dim),
            Span::styled(v.to_string(), p.default),
        ]));
    }
    if let Some(t) = &fm.page_type {
        kv(&mut out, p, "type", t);
    }
    if let Some(l) = &fm.language {
        kv(&mut out, p, "language", l);
    }
    if let Some(d) = &fm.description {
        out.push(Line::from(vec![
            Span::styled(" description ", p.dim),
            Span::styled(d.clone(), p.default),
        ]));
    }
    if !fm.tags.is_empty() {
        out.push(Line::from(vec![
            Span::styled(" tags         ", p.dim),
            Span::styled(fm.tags.join(", "), p.default),
        ]));
    }
    if let Some(g) = &fm.search_required_phrase {
        out.push(Line::from(vec![
            Span::styled(" gates        ", p.dim),
            Span::styled(g.clone(), p.label),
        ]));
    }
    out
}

/// The form for the Edit mode (M8.2): a 1-line header (the "editing
/// note:P1 · field …" identity), a form pane (the field the user is on,
/// its live value buffer, and — for list fields — the *pending commit
/// plan* (which ops `Enter` will apply) so the user isn't surprised by a
/// write), and a suggestions pane (the live tab-autocomplete for the
/// current buffer; empty for scalar fields).
///
/// Layout:
/// ```text
///  kb tui — editing note:P3 (tags)                ← page identity + field
/// ┌──────── form ──────────────────┬── suggestions ──┐
/// │ current: [mTLS · token bucket] │ • mTLS           │
/// │ new: 'llm'                     │ • token bucket   │
/// │ → append 'llm' to tags         │ • transformer    │
/// └────────────────────────────────┴──────────────────┘
///  status bar (colored by severity)
/// ```
/// The form pane for any field on the *Picker* list: the field's *label*
/// and a *preview* of what's there right now (the scalar value, or the
/// comma-joined list items). A pure function of `EditState` — testable
/// without a DB / Frame. This is the "what will be on screen" preview
/// the user sees *before* choosing a field, so the field is not a mystery
/// (they don't have to enter it to know if it's empty / what value is in
/// there).
fn picker_row(p: &Palette, es: &crate::tui::EditState, field: crate::tui::EditField) -> Line<'static> {
    // `entry_bytes` is the file-as-we-saw-it (the baseline). We use the raw
    // bytes + a fresh parse (not the cached `current_list`) so the user sees
    // what's *on disk*, never a stale working copy (which only ever differs
    // after a tail-removal via the smart `Backspace`, and only while they're
    // already in that field's Value stage).
    let bytes = es.entry_bytes.as_slice();
    let (populated, preview) = if field.is_list() {
        let items: Vec<String> = kb_core::frontmatter::list_values(bytes, field.key())
            .unwrap_or_default();
        if items.is_empty() {
            (false, String::from("(none)"))
        } else {
            (true, format!("[{}]", items.iter().map(|t| truncate(t.as_str(), 30)).collect::<Vec<_>>().join(" · ")))
        }
    } else {
        let v = kb_core::frontmatter::scalar_value(bytes, field.key()).unwrap_or_default();
        if v.is_empty() {
            (false, String::from("(none)"))
        } else {
            (true, format!("'{}'", truncate(v.as_str(), 50)))
        }
    };
    // P8: the *first* character of the row is the gate marker — `◆`
    // (populated) or `◇` (empty) — in the palette's `gated`/`open` role so
    // a user scanning the field list sees at a glance which fields are
    // already set. The rest of the row is unchanged.
    let (marker, style) = if populated { ("◆", p.gated) } else { ("◇", p.open) };
    Line::from(vec![
        Span::styled(marker.to_string(), style),
        Span::raw(format!("  {:<12}  {}", field.label(), preview)),
    ])
}

/// The full *field list* the Picker shows — one row per field in the
/// stable `EditField::all()` order, with a `▶` marker on the current
/// highlight. This is the visual "field picker" pane.
fn build_picker_rows(p: &Palette, es: &crate::tui::EditState) -> Vec<Line<'static>> {
    let fields = crate::tui::EditField::all();
    let mut out = Vec::new();
    for field in fields {
        out.push(picker_row(p, es, *field));
    }
    out
}

/// Draw the whole **Picker** stage pane (the field list). The layout is a
/// *single* bordered pane (no side-by-side suggestions in the Picker —
/// the suggestions are loaded + shown in the Editor stage, not yet); on a
/// scalar the *preview* is the current scalar value, and on a list the
/// *preview* is the existing items.
fn draw_picker_pane(f: &mut Frame, p: &Palette, es: &crate::tui::EditState, rect: ratatui::layout::Rect) {
    let rows = build_picker_rows(p, es);
    let items: Vec<ListItem> = rows.into_iter().map(ListItem::new).collect();
    let widget = RatList::new(items)
        .highlight_style(p.selection)
        .block(
            // Keybinding legend lives in the status bar (one place), not here —
            // this title is identity only. (Removes the dual/truncated legend.)
            Block::default().borders(Borders::ALL).title(" pick a field"),
        );
    let mut state = ListState::default();
    state.select(Some(es.field_cursor));
    f.render_stateful_widget(widget, rect, &mut state);
}

/// The single **Value** stage pane. The *layout depends* on (field kind,
/// sub-mode):
///
/// **Scalar fields (title / description), buffer mode** (always; scalars
/// have no Selection sub-mode):
/// ```
///   buffer (with caret): "Hel|lo World"    ← caret at `value_cursor`
///   → no-op commit (nothing to change)     ← pending plan
/// ```
///
/// **List fields, buffer mode** (the default when entering a list field):
/// ```
///   existing: [mTLS · transformer]        ← `es.current_list`, joined
///   buffer:   'llm'                       ← `es.value`
///   candidates:                            ← live-narrowed list
///     ▶ • mtls                            ← `sug_cursor` on the row
///       • transformer
///   → append tag 'llm'                     ← pending plan
/// ```
///
/// **List fields, Selection mode** (`in_list_select == true`):
/// ```
///   existing tags:                          ← a *combined list* (the
///     ▶ • mTLS                              ← `list_high` highlight;
///       • transformer                       ←   walking order
///       • <new token>                       ←   the *last* entry is a
///                                       virtual "add a new one" slot
///   cached buffer:  'llm'                  ← what the user was *about*
///                                           to type before they hit ←,
///                                           preserved for when they go
///                                           back (by Esc, or by
///                                           selecting `<new>` + Enter)
///   → no-op commit (nothing changed yet)   ← pending plan (live-removals
///                                           are baked into `working_bytes`
///                                           so this reflects what `Enter`
///                                           will apply)
/// ```
///
/// Everything is a pure function of `EditState`, so the display can never
/// be stale relative to the state (the same `plan()` the commit path runs
/// is what we render as the "→" footer).
fn build_value_lines(p: &Palette, es: &crate::tui::EditState) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();

    let active = p.active;
    let caret = p.caret;
    let arrow = p.selection_ctx;
    let star = p.hint;
    let ctx_dot = p.dim;
    let ctx_text = p.default;
    let dim = p.dim;

    if es.field.is_list() {
        if es.in_list_select {
            // ---- SELECTION: `▶` on `list_high`, `·` on the others ----
            for (i, t) in es.current_list.iter().enumerate() {
                let on = i == es.list_high;
                out.push(Line::from(vec![
                    Span::styled(
                        if on { "▶  " } else { "·  " },
                        if on { arrow } else { ctx_dot },
                    ),
                    Span::styled(truncate(t.as_str(), 60), if on { active } else { ctx_text }),
                ]));
            }
            let on = es.current_list.len() == es.list_high;
            out.push(Line::from(vec![
                Span::styled(
                    if on { "▶  " } else { "·  " },
                    if on { arrow } else { ctx_dot },
                ),
                Span::styled("<new>", if on { active } else { ctx_text }),
            ]));
            out.push(Line::from(""));
            out.push(Line::from(vec![
                Span::styled("·  ", ctx_dot),
                Span::styled("cached: ", ctx_text),
                Span::styled(format!("'{}'", truncate(es.value.as_str(), 60)), ctx_text),
            ]));
        } else {
            // ---- BUFFER: existing items are context (`·`); candidates
            // are `»`/`·`; the *buffer itself* is the one `▶`. ----
            if es.current_list.is_empty() {
                out.push(Line::from(vec![
                    Span::styled("·  ", ctx_dot),
                    Span::styled("(no existing)", ctx_text),
                ]));
            } else {
                for t in es.current_list.iter() {
                    out.push(Line::from(vec![
                        Span::styled("·  ", ctx_dot),
                        Span::styled(truncate(t.as_str(), 60), ctx_text),
                    ]));
                }
            }
            out.push(Line::from(""));
            if es.suggestions.is_empty() {
                out.push(Line::from(vec![
                    Span::styled("   ", dim),
                    Span::styled("(no matches — keep typing)", ctx_text),
                ]));
            } else {
                for (i, t) in es.suggestions.iter().take(40).enumerate() {
                    let on = i == es.sug_cursor;
                    out.push(Line::from(vec![
                        Span::styled(
                            if on { "»  " } else { "·  " },
                            if on { star } else { ctx_dot },
                        ),
                        Span::styled(truncate(t.as_str(), 60), if on { active } else { ctx_text }),
                    ]));
                }
            }
            out.push(Line::from(""));
            // The list buffer has *no* text cursor (Left/Right enter Selection
            // mode instead), so no block caret — just the token typed, bold.
            out.push(Line::from(vec![
                Span::styled("▶  ", arrow),
                Span::styled("<buffer> ", ctx_text),
                Span::styled(truncate(es.value.as_str(), 60), active),
            ]));
        }
        out.push(Line::from(""));
    } else {
        // ---- SCALAR: `▶` + a `▮` block caret at the insert point ----
        let before: String = es.value.chars().take(es.value_cursor).collect();
        let after: String = es.value.chars().skip(es.value_cursor).collect();
        let mut spans = vec![
            Span::styled("▶  ", arrow),
            Span::styled(format!("{}  ", es.field.label()), ctx_text),
        ];
        if !before.is_empty() { spans.push(Span::styled(before, active)); }
        spans.push(Span::styled("▮", caret));
        if !after.is_empty() { spans.push(Span::styled(after, active)); }
        out.push(Line::from(spans));
        out.push(Line::from(""));
    }

    let plan = es.plan();
    if plan.is_empty() {
        out.push(Line::from(vec![
            Span::styled("   ", dim),
            Span::styled("no-op commit (nothing to change)", ctx_text),
        ]));
    } else {
        for op in &plan {
            out.push(Line::from(vec![
                Span::styled("   ", dim),
                Span::styled(describe_op(op), ctx_text),
            ]));
        }
    }
    out
}

/// Draw the Edit-metadata screen — a *two-stage* state machine mirroring
/// the zsh `type → live-filter → arrows-select → Enter-adopts` idiom:
///
/// ```text
///  kb tui — editing metadata for <id> · <stage>
/// ┌────────────────────────────────────────────────────┐
/// │  <stage-appropriate single pane>                   │
/// └────────────────────────────────────────────────────┘
///  status bar (colored by severity)
/// ```
///
/// Each stage is a *single* pane — no side-by-side split (two panes each
/// steal `↑/↓` and `Enter` from the other, which is the zsh model we're
/// not doing):
///
/// The **Picker** pane is the field list (one row per field, with the
/// current preview of what's there). The highlight is `es.field_cursor`;
/// `↑/↓` move it, `Enter` enters the **Value** pane for the highlighted
/// field. `q` / `a` / `r` (quit / alias / roots) are only reachable here
/// (and from Browse / Read).
///
/// The **Value** pane is the value buffer for the chosen field, in a
/// single pane that shows (for a list field) the existing items + the
/// buffer + the live candidate list (the `▶` mark is on `sug_cursor` —
/// the one `Tab` would fill) + the pending commit plan (`→` lines —
/// exactly the ops `Enter` will apply). A scalar field just shows the
/// buffer + the pending plan (no list, no candidates).
fn draw_edit(f: &mut Frame, app: &App, es: &crate::tui::EditState, area: ratatui::layout::Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // single stage pane
            Constraint::Length(1), // status
        ])
        .split(area);

    let (stage_label, focus_note) = match es.stage {
        crate::tui::EditStage::Picker => (
            String::from("picking field"),
            String::from("↑/↓ move · Enter open · a alias · r roots · Esc back · q quit"),
        ),
        crate::tui::EditStage::Value if es.field.is_list() => (
            if es.in_list_select {
                format!("editing {} (Selection)", es.field.label())
            } else {
                format!("editing {} (buffer)", es.field.label())
            },
            if es.in_list_select {
                "←/→ move · Backspace remove · Enter load / back · Esc buffer · C-c quit".to_string()
            } else {
                "type · ↑/↓ pick · Tab fill · ←/→ select existing · Enter commit · Esc back · C-c quit".to_string()
            },
        ),
        crate::tui::EditStage::Value => (
            format!("editing {}", es.field.label()),
            "type · ←/→ cursor · Backspace delete · Enter commit · Esc back · C-c quit".to_string(),
        ),
    };
    let header = Paragraph::new(format!(
        "  kb tui — edit metadata for {} · {} · {}",
        es.id, stage_label, focus_note
    ))
    .style(app.palette.label);
    f.render_widget(header, chunks[0]);

    match es.stage {
        crate::tui::EditStage::Picker => {
            // Single pane: the field list (the *deliberate first step* —
            // the user chooses a field before touching its value).
            draw_picker_pane(f, &app.palette, es, chunks[1]);
        }
        crate::tui::EditStage::Value => {
            // Single pane: the value form (existing items + buffer + live
            // candidate list + pending plan). The *title* encodes the
            // current focus state (buffer vs. menu) so the user can see
            // at a glance which of the two things `↑/↓` / `Enter` are
            // addressing.
            let lines = build_value_lines(&app.palette, es);
            let widget = Paragraph::new(lines)
                .block(
                    Block::default().borders(Borders::ALL).title(format!(
                        " {} — {}",
                        es.field.label(),
                        if es.field.is_list() && es.in_list_select {
                            "Selection · ←/↑↓/→ move · Backspace remove · Enter load · Esc back"
                        } else if es.field.is_list() {
                            "buffer · type · ↑/↓ pick · Tab fill · ←/→ select existing · Enter commit"
                        } else {
                            "buffer · ←/→ cursor · Backspace delete · Enter commit"
                        },
                    )),
                )
                .wrap(ratatui::widgets::Wrap { trim: false });
            f.render_widget(widget, chunks[1]);
        }
    }

    render_status_bar(f, app, chunks[2]);
}

/// A human-readable one-line for an `EditOp` — the "what will the commit
/// apply" hint in the form's pending-plan footer.
fn describe_op(op: &crate::tui::EditOp) -> String {
    match op {
        crate::tui::EditOp::SetScalar { key, value } => {
            format!("set {key} = '{}'", truncate(value.as_str(), 40))
        }
        crate::tui::EditOp::AppendItem { key, item } => {
            format!("append '{item}' to {key}")
        }
        crate::tui::EditOp::RemoveField { key } => format!("clear {key}"),
        crate::tui::EditOp::RemoveItem { key, item } => {
            format!("remove '{item}' from {key}")
        }
    }
}

/// The **Alias** manager (M8.3): a 1-line header ("aliases: …"), the tag-
/// alias list (the `list` snapshot) and a single input line. The *input* is
/// the *add form* — the user types `<alias> <canonical>` (space-separated)
/// on the form then `Enter` submits. `Tab` toggles focus: when the *list*
/// is focused the `↑`/`↓` selection moves and `x` removes the selected
/// alias. The render shows which context is active (the list pane title
/// reflects `focused`).
fn draw_alias(
    f: &mut Frame,
    app: &App,
    ast: &crate::tui::AliasState,
    area: ratatui::layout::Rect,
) {
    use ratatui::widgets::{List as RatList, ListItem, ListState};
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // list + input line
            Constraint::Length(1), // status
        ])
        .split(area);

    let header = Paragraph::new(format!(
        "  kb tui — aliases: {} (Tab list/form · Esc back · q quit)",
        ast.list.len()
    ))
    .style(app.palette.label);
    f.render_widget(header, chunks[0]);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(chunks[1]);

    // P9: the alias row is palette-styled — the *alias* in `p.default`,
    // the `→` separator in `p.label` (the "this *points at*" affordance),
    // the *canonical* target in `p.dim` (secondary information). On focus
    // the whole row is reversed via `p.selection`.
    let items: Vec<ListItem> = ast
        .list
        .iter()
        .map(|r| {
            let line = Line::from(vec![
                Span::styled(r.alias.clone(), app.palette.default),
                Span::styled("  →  ".to_string(), app.palette.label),
                Span::styled(truncate(r.canonical.as_str(), 40), app.palette.dim),
            ]);
            ListItem::new(line)
        })
        .collect();
    let list_title = if ast.focused {
        " ✓ alias list — ↑/↓ select · x remove · Tab form"
    } else {
        "   alias list (Tab to focus)"
    };
    let widget = RatList::new(items)
        .highlight_style(app.palette.selection)
        .block(Block::default().borders(Borders::ALL).title(list_title));
    let mut list_state = ListState::default();
    if !ast.list.is_empty() {
        list_state.select(Some(ast.selected));
    }
    f.render_stateful_widget(widget, panes[0], &mut list_state);

    let form_hint = if ast.focused {
        "   (add form — Tab back)"
    } else {
        "   <alias> <canonical>"
    };
    let input_line = format!("{}  {}", form_hint, ast.input);
    let input_widget = Paragraph::new(input_line)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" add — Enter submit (upsert) · Backspace clear · Esc back"),
        )
        .wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(input_widget, panes[1]);

    render_status_bar(f, app, chunks[2]);
}

/// The **Roots** manager (M8.3): a 1-line header, the registered roots list
/// (the `list` snapshot) + a single input line (the *add form*: type an
/// absolute directory path, `Enter` registers it and full-refreshes). `Tab`
/// toggles focus; `x` drops the selected root and full-refreshes.
fn draw_roots(
    f: &mut Frame,
    app: &App,
    rs: &crate::tui::RootsState,
    area: ratatui::layout::Rect,
) {
    use ratatui::widgets::{List as RatList, ListItem, ListState};
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // list + input line
            Constraint::Length(1), // status
        ])
        .split(area);

    let header = Paragraph::new(format!(
        "  kb tui — roots: {} (Tab list/form · Esc back · q quit)",
        rs.list.len()
    ))
    .style(app.palette.label);
    f.render_widget(header, chunks[0]);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(chunks[1]);

    // P9: the roots row is palette-styled — the *path* in `p.default`,
    // any `[options]` / `(added: …)` trailing annotations in `p.dim` (the
    // secondary information), so the path stays the visual focus.
    let items: Vec<ListItem> = rs
        .list
        .iter()
        .map(|r| {
            let mut spans = vec![Span::styled(r.path.clone(), app.palette.default)];
            let opts: Vec<String> = r
                .extra_exts
                .as_deref()
                .map(|s| s.lines().map(|l| format!("ext:{l}")).collect())
                .unwrap_or_default();
            if !opts.is_empty() {
                spans.push(Span::styled(
                    format!("  [{}]", opts.join(",")),
                    app.palette.dim,
                ));
            }
            if r.include_hidden {
                spans.push(Span::styled("  [include-hidden]".to_string(), app.palette.dim));
            }
            if let Some(added) = &r.added_at {
                spans.push(Span::styled(format!("  (added: {added})"), app.palette.dim));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list_title = if rs.focused {
        " ✓ roots list — ↑/↓ select · x drop · Tab form"
    } else {
        "   roots list (Tab to focus)"
    };
    let widget = RatList::new(items)
        .highlight_style(app.palette.selection)
        .block(Block::default().borders(Borders::ALL).title(list_title));
    let mut list_state = ListState::default();
    if !rs.list.is_empty() {
        list_state.select(Some(rs.selected));
    }
    f.render_stateful_widget(widget, panes[0], &mut list_state);

    let form_hint = if rs.focused {
        "   (add form — Tab back)"
    } else {
        "   <absolute dir path>"
    };
    let input_line = format!("{}  {}", form_hint, rs.input);
    let input_widget = Paragraph::new(input_line)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" add — Enter registers + full-refresh · Backspace clear · Esc back"),
        )
        .wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(input_widget, panes[1]);

    render_status_bar(f, app, chunks[2]);
}

/// The **Search** mode (M8.4): a command-palette layout over the full
/// FTS5 engine. Three rows:
/// ```text
///  header:  kb tui — search (n result(s), m tag hint(s))
///  row 1:   query prompt (full width)
///  row 2:   results (left, ~70%) | tag suggestions (right, ~30%)
///  status bar
/// ```
///
/// The results pane mirrors Browse's row shape (gate marker · id ·
/// rel_path) so the two modes read identically; only the ordering
/// differs (FTS relevance vs alphabetical). The tag-suggest pane lists
/// `alias → canonical` lines so the user sees *which* tag they are
/// about to inject.
fn draw_search(
    f: &mut Frame,
    app: &App,
    ss: &crate::tui::SearchState,
    area: ratatui::layout::Rect,
) {
    use ratatui::widgets::{List as RatList, ListItem, ListState};

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(3), // query prompt line (borders eat 2 cells)
            Constraint::Min(1),    // results | tag suggestions
            Constraint::Length(1), // status
        ])
        .split(area);

    let n_res = ss.results.as_ref().map_or(0, |r| r.len());
    let header = Paragraph::new(format!(
        "  kb tui — search · {n_res} result(s) · {} tag hint(s)  (↑↓ move · Tab inject tag · Enter open · Esc back)",
        ss.tag_sugs.len()
    ))
    .style(app.palette.label);
    f.render_widget(header, chunks[0]);

    // Query prompt: the buffer text after a "❯ " caret. No borders
    // (a bordered box looks like a list and confuses the eye — this is
    // a single input line).
    let prompt = format!("❯ {}", ss.query);
    let prompt_widget = Paragraph::new(prompt).style(app.palette.default);
    f.render_widget(prompt_widget, chunks[1]);

    // The horizontal split: results (70) | tag suggestions (30).
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(chunks[2]);

    let row_width = panes[0].width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = ss
        .results
        .as_ref()
        .map(|res| {
            res.iter()
                .map(|r| ListItem::new(Line::from(browse_row(&app.palette, r, row_width))))
                .collect()
        })
        .unwrap_or_default();
    let list_title = if ss.results.as_ref().is_some_and(|r| r.is_empty()) {
        " 0 result — type a query".to_string()
    } else {
        format!(" {n_res} result(s) — FTS5")
    };
    let widget = RatList::new(items)
        .highlight_style(app.palette.selection)
        .block(Block::default().borders(Borders::ALL).title(list_title));
    let mut list_state = ListState::default();
    if n_res > 0 {
        list_state.select(Some(ss.sel));
    }
    f.render_stateful_widget(widget, panes[0], &mut list_state);

    // Tag suggestions pane: each row is `  <display>` (the `→ canonical`
    // hint is part of the display string the App computed — `alias →
    // canonical` or a bare tag).
    let sug_width = panes[1].width.saturating_sub(2) as usize;
    let sug_items: Vec<ListItem> = ss
        .tag_sugs
        .iter()
        .map(|s| ListItem::new(Line::from(Span::styled(
            truncate(&s.display, sug_width),
            app.palette.dim,
        ))))
        .collect();
    let sug_title = if ss.tag_sugs.is_empty() {
        " tag hints — type `tag:` in the query".to_string()
    } else {
        format!(" {0} tag hint(s) — Tab injects", ss.tag_sugs.len())
    };
    let sug_widget = RatList::new(sug_items)
        .highlight_style(app.palette.selection)
        .block(Block::default().borders(Borders::ALL).title(sug_title));
    let mut sug_state = ListState::default();
    if !ss.tag_sugs.is_empty() {
        sug_state.select(Some(ss.tag_sel));
    }
    f.render_stateful_widget(sug_widget, panes[1], &mut sug_state);

    render_status_bar(f, app, chunks[3]);
}

/// Build the status bar as one `Line` (a list of `Span`s). The *severity
/// glyph* is its own span — `✓ ` for success, `! ` for error, none for info —
/// so the meaning survives even if a user's palette makes `error` and
/// `success` the same color (a real-world recolor, e.g. a terminal that
/// doesn't distinguish LightGreen from LightRed). Same "symbols carry
/// meaning" principle as the `◆`/`◇` gate markers — the `!` glyph is the
/// one thing that tells a write-success apart from a validation-failure if
/// the user's `error` and `success` roles collapse onto the same style.
///
/// Returns a single `Line` so `render_status_bar` is a one-liner (the
/// match lives here, where it's testable without a `Frame`).
fn build_status_line(p: &Palette, kind: &StatusKind, text: &str) -> Line<'static> {
    let (glyph, style) = match kind {
        StatusKind::Info => ("", p.info),
        StatusKind::Success => ("✓ ", p.success),
        StatusKind::Error => ("! ", p.error),
    };
    if glyph.is_empty() {
        return Line::from(vec![Span::styled(format!("  {text}"), style)]);
    }
    // Two-space left margin (matches the pre-glyph shape), then the glyph,
    // then the message — all in the severity role.
    Line::from(vec![
        Span::styled("  ".to_string(), style),
        Span::styled(glyph.to_string(), style),
        Span::styled(text.to_string(), style),
    ])
}

fn render_status_bar(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let line = build_status_line(&app.palette, &app.status.kind, &app.status.text);
    let bar = Paragraph::new(line);
    f.render_widget(bar, area);
}

/// P7: turn a single body line into a `Line<'static>`. An ATX heading
/// (`#` … `######` followed by a space) renders in the `p.label` role so a
/// reader can skim the document's structure even when the body is wrapped;
/// non-headings stay in the default role. The check is *visual* (it
/// matches what the user would read as a heading in markdown), so lines
/// inside a fenced code block that happen to start with `#` will also pick
/// up the label style — an acceptable, deterministic false-positive rather
/// than carrying a stateful parser through the render path.
fn read_line(p: &Palette, raw: &str) -> Line<'static> {
    if is_heading(raw) {
        Line::from(vec![Span::styled(raw.to_string(), p.label)])
    } else {
        render_markdown_text(p, raw)
    }
}

/// The **rendered** text of one raw body line, flattened to plain `String` —
/// the single source of truth the find overlay + row/centering math paint
/// onto. `read_line` already emits this exact character stream (with styles);
/// we drop the styles here. Heading lines pass through *unmodified* (the
/// `read_line` short-circuit keeps `#` and any `**` verbatim). This is the
/// function `mod.rs` calls when it needs to search + measure the *rendered*
/// (not raw) body — the raw form would drift as soon as a line has any
/// markdown emphasis, because the find offset is painted onto rendered
/// chars.
pub fn rendered_line(p: &Palette, raw: &str) -> String {
    read_line(p, raw)
        .spans
        .iter()
        .map(|s| s.content.as_ref().to_owned())
        .collect()
}

/// Render every raw line of a body (one rendered `String` per raw line).
/// The pure `find_matches` / `scroll_max_top` / `line_first_row` /
/// `find_padding_for_cursor` helpers in `mod.rs` consume this, so the
/// search + scroll math always agree with what `draw_read` paints.
pub fn rendered_lines(p: &Palette, body: &str) -> Vec<String> {
    body.lines().map(|raw| rendered_line(p, raw)).collect()
}

/// The number of **rendered rows** a set of logical lines occupies at
/// `pane_width`, using `Paragraph`'s own `line_count` — i.e. the exact
/// `WordWrapper` (`Wrap { trim: false }`) engine `draw_read` paints with.
/// This is the single source of truth for the scroll/centering math, so a
/// future ratatui wrap change moves the search scroll with it instead of
/// drifting behind a hand-rolled `div_ceil` model.
///
/// `pane_width <= 1` falls back to the logical-line count (the same guard
/// the scroll mutators use before the first render reports a real width).
/// Empty input → 0.
pub fn rendered_row_count(lines: &[String], pane_width: usize) -> usize {
    if lines.is_empty() {
        return 0;
    }
    if pane_width <= 1 {
        return lines.len();
    }
    let text: Vec<Line<'static>> = lines.iter().map(|l| Line::from(l.clone())).collect();
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .line_count(pane_width.min(u16::MAX as usize) as u16)
}

/// The rendered-row count of **one** logical line at `pane_width`, via that
/// line's own `Paragraph::line_count` (the same `WordWrapper`, so it is exact
/// for that line). Used to build the body roll-up incrementally — the cache
/// only ever (re)renders the logical lines that changed since the last
/// roll-up. `pane_width <= 1` falls back to the logical one-row-per-line
/// model (the same guard `rendered_row_count` uses).
pub fn line_row_count(line: &str, pane_width: usize) -> usize {
    if pane_width <= 1 {
        return 1;
    }
    Paragraph::new(Line::from(line.to_string()))
        .wrap(Wrap { trim: false })
        .line_count(pane_width.min(u16::MAX as usize) as u16)
        .max(1)
}

/// The style used only as an off-screen "where did this land?" probe. It is
/// deliberately not part of any palette, so the scan below cannot mistake a
/// real glyph for the selected hit.
fn find_probe_style() -> Style {
    Style::default()
        .fg(Color::Rgb(1, 2, 3))
        .add_modifier(Modifier::HIDDEN)
}

/// The rendered row containing the selected find hit `(line_idx, byte_off)`,
/// in the **unpadded** body. Instead of guessing from the line's start (the
/// old `line_first_row` bug: a hit deep inside a long wrapped line was
/// centered on the line's *first* row and could be scrolled off screen), we
/// ask ratatui itself: the selected character is painted with
/// [`find_probe_style`] into an off-screen `Buffer`, then the buffer is
/// scanned for that style. Styling does not affect wrapping, so this is the
/// exact row `draw_read` will paint the hit on.
///
/// `pane_width <= 1` falls back to the logical line index. `None` when the
/// hit is out of range or the marked character cannot be found.
pub fn match_rendered_row(
    lines: &[String],
    selected: (usize, usize),
    pane_width: usize,
) -> Option<usize> {
    let (line_idx, byte_off) = selected;
    let line = lines.get(line_idx)?;
    if pane_width <= 1 {
        return Some(line_idx);
    }
    let ch_len = line.get(byte_off..)?.chars().next()?.len_utf8();
    if ch_len == 0 {
        return None;
    }
    let rows_before = rendered_row_count(&lines[..line_idx], pane_width);
    let probe = find_probe_style();
    let styled = apply_find_patched(
        Line::from(line.clone()),
        &[(byte_off, byte_off + ch_len, probe)],
    );
    // Only the selected logical line needs to be rendered: word wrapping is
    // per-logical-line, so its internal row offsets are independent of the
    // lines above it.
    let line_rows = Paragraph::new(vec![styled.clone()])
        .wrap(Wrap { trim: false })
        .line_count(pane_width as u16)
        .max(1);
    let width = pane_width.min(u16::MAX as usize) as u16;
    let height = line_rows.min(u16::MAX as usize) as u16;
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    Widget::render(
        Paragraph::new(vec![styled]).wrap(Wrap { trim: false }),
        area,
        &mut buf,
    );
    for y in 0..height {
        for x in 0..width {
            if let Some(c) = buf.cell((x, y)) {
                // Compare the probe's two independent fields, not the whole
                // `Style`: the buffer fills in `bg`/`underline_color` with
                // `Reset`, which is irrelevant to our marker.
                let style = c.style();
                if style.fg == probe.fg && style.add_modifier.contains(Modifier::HIDDEN) {
                    return Some(rows_before + y as usize);
                }
            }
        }
    }
    None
}

/// Apply the find highlight to a single rendered body line.
/// `patches` is a list of `(start_byte, end_byte, style)` — the byte
/// ranges (*into this line's character stream*) to color with `style`.
/// Each char's effective style is the **last** patch (in `patches`
/// order) that covers it — or the base span's style if no patch covers it
/// (so a patch always overrides the markdown emphasis). Patches may
/// overlap, be non-contiguous, or cover only part of a span; the function
/// walks the rendered chars left-to-right, emitting consecutive chunks of
/// the same effective style as a single `Span` (so the diff engine sees
/// minimal, contiguous runs).
fn apply_find_patched(
    line: Line<'static>,
    patches: &[(usize, usize, Style)],
) -> Line<'static> {
    if patches.is_empty() {
        return line;
    }
    // Flatten the rendered line into a char stream + base styles.
    let flat: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|sp| {
            let base = sp.style;
            sp.content.chars().map(move |ch| (ch, base))
        })
        .collect();
    // Byte offset of each char (into the line's character stream, matching
    // the same byte model the caller used to compute `patches`).
    let mut byte_at: Vec<usize> = Vec::with_capacity(flat.len());
    let mut acc = 0usize;
    for (ch, _) in &flat {
        byte_at.push(acc);
        acc += ch.len_utf8();
    }
    // Walk chars, accumulating consecutive same-style runs.
    let mut chunks: Vec<(String, Style)> = Vec::new();
    for i in 0..flat.len() {
        let (ch, base) = flat[i];
        let by = byte_at[i];
        // Last patch covering `by` wins (the caller sorts so the selected
        // hit is last — but the "last in vec" rule is the contract).
        let eff = patches
            .iter()
            .rev()
            .find(|(s, e, _)| *s <= by && by < *e)
            .map(|(_, _, sty)| *sty)
            .unwrap_or(base);
        if let Some(last) = chunks.last_mut() {
            if last.1 == eff {
                last.0.push(ch);
                continue;
            }
        }
        chunks.push((ch.to_string(), eff));
    }
    Line::from(
        chunks
            .into_iter()
            .map(|(s, st)| Span::styled(s, st))
            .collect::<Vec<_>>(),
    )
}

/// True when `raw` is an ATX heading: 1–6 `#`s followed by exactly one
/// space (or end-of-line). `####### seven`, `##nospace`, `#Title` (no
/// space), and the empty string are *not* headings.
fn is_heading(raw: &str) -> bool {
    let cs = raw.as_bytes();
    if cs.is_empty() || cs[0] != b'#' {
        return false;
    }
    let mut i = 0;
    while i < cs.len() && cs[i] == b'#' {
        i += 1;
    }
    (1..=6).contains(&i) && i < cs.len() && cs[i] == b' '
}

/// Render a Markdown body line into styled spans. Inline emphasis is
/// recognized for **bold** (`**…**`), _italic_ (`*…*`/`_…_`),
/// ~~strikethrough~~ (`~~…~~`), and inline code (`` `…` ``); unmatched
/// delimiters pass through as literal text. Headings are the one exception
/// — they render in the `label` role unmodified (the heading's own styling
/// wins, so a `#` heading with `**bold**` inside stays a single `label`
/// span). The inner text of a code span is opaque: any `**bold**`, `*it*`,
/// or `~~st~~` inside a `…` span is literal (standard markdown semantics).
///
/// This is *not* a full Markdown engine: there is no HTML tag, no URL,
/// no bold-italic combination, and code is single-backtick only (no
/// fenced triple-backtick blocks and no backticks inside a code span).
/// Anything the scanner doesn't match verbatim falls back to the
/// `default` role, which is what the pre-markdown renderer produced,
/// so a plain-prose page renders byte-identically to before.
fn render_markdown_text(p: &Palette, raw: &str) -> Line<'static> {
    if raw.is_empty() {
        return Line::from(vec![Span::styled(String::new(), p.default)]);
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut i = 0usize;
    let bytes = raw.as_bytes();
    while i < bytes.len() {
        match bytes[i] {
            b'*' | b'_' => {
                if bytes[i] == b'_'
                    && ((i > 0 && bytes[i - 1] == b'_')
                        || (i + 1 < bytes.len() && bytes[i + 1] == b'_'))
                {
                    // Double underscore is literal in this subset. Render one
                    // underscore and keep scanning from here (the next char is
                    // also an underscore and will be handled in the next loop
                    // iteration).
                    buf.push('_');
                    i += 1;
                    continue;
                }
                let run = delimiter_run(bytes, i);
                let matched = match run {
                    2 if bytes[i] == b'*' => find_close_delim(i + 2, 2, bytes),
                    1 if bytes[i] == b'*' => find_close_delim(i + 1, 1, bytes),
                    1 if bytes[i] == b'_' => find_close_delim(i + 1, 1, bytes),
                    _ => None,
                };
                if let Some(close) = matched {
                    if !buf.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut buf), p.default));
                    }
                    let inner = &raw[i + run..close];
                    if !inner.is_empty() {
                        let style = if run == 2 { p.md_bold } else { p.md_italic };
                        spans.push(Span::styled(inner.to_string(), style));
                    }
                    i = close + run;
                } else {
                    // No closer — render as a literal delimiter.
                    buf.push(bytes[i] as char);
                    i += 1;
                }
            }
            b'~' => {
                let close = if i + 1 < bytes.len() && bytes[i + 1] == b'~' {
                    find_close_delim(i + 2, 2, bytes)
                } else {
                    None
                };
                if let Some(close) = close {
                    if !buf.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut buf), p.default));
                    }
                    let inner = &raw[i + 2..close];
                    if !inner.is_empty() {
                        spans.push(Span::styled(inner.to_string(), p.md_strike));
                    }
                    i = close + 2;
                } else {
                    buf.push('~');
                    i += 1;
                }
            }
            b'`' => {
                // Inline code span (`code`). A single backtick delimits;
                // the inner text is opaque — `**bold**` inside a code span
                // is *literal*, so we scan to the closest backtick and
                // render the whole inner span in `md_code`. Adjacent
                // backticks (`` `` ` or ` ``` `) are *not* closers (that
                // form is not part of this subset); an unmatched backtick
                // is literal.
                if i + 1 < bytes.len() && bytes[i + 1] == b'`' {
                    // Adjacent to another backtick — render this one as
                    // literal and let the next iteration handle the second.
                    buf.push('`');
                    i += 1;
                    continue;
                }
                let close = find_close_delim(i + 1, 1, bytes);
                if let Some(close) = close {
                    if !buf.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut buf), p.default));
                    }
                    let inner = &raw[i + 1..close];
                    if !inner.is_empty() {
                        spans.push(Span::styled(inner.to_string(), p.md_code));
                    }
                    i = close + 1;
                } else {
                    buf.push('`');
                    i += 1;
                }
            }
            _ => {
                // Non-special byte. For a non-ASCII leading byte (e.g. a
                // CJK leading byte), copy the full UTF-8 codepoint rather
                // than one byte — `bytes[i] as char` would corrupt it.
                // ASCII bytes are a single `char`; non-ASCII bytes start a
                // multi-byte sequence we decode via `std::str::from_utf8`.
                let (ch, n) = if bytes[i] < 0x80 {
                    (bytes[i] as char, 1)
                } else {
                    match std::str::from_utf8(&bytes[i..]) {
                        Ok(s) => {
                            let mut it = s.chars();
                            let c = it.next().unwrap();
                            (c, c.len_utf8())
                        }
                        Err(_) => (bytes[i] as char, 1),
                    }
                };
                buf.push(ch);
                i += n;
            }
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, p.default));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), p.default));
    }
    Line::from(spans)
}

/// Count consecutive ASCII `*` or `_` starting at `i`.
fn delimiter_run(bytes: &[u8], i: usize) -> usize {
    if i >= bytes.len() || (bytes[i] != b'*' && bytes[i] != b'_') {
        return 0;
    }
    let mut n = 0usize;
    while i + n < bytes.len() && bytes[i + n] == bytes[i] {
        n += 1;
    }
    n
}

/// From `start`, find the *next* occurrence of `count` consecutive
/// delimiters matching the byte at `start - 1`. Returns the index of the
/// first `*` (or `_`) of the closer, or `None` if no closer exists.
fn find_close_delim(
    start: usize,
    count: usize,
    bytes: &[u8],
) -> Option<usize> {
    if start >= bytes.len() {
        return None;
    }
    let target = bytes[start.saturating_sub(1)]; // the delimiter char
    let mut i = start;
    while i < bytes.len() {
        if bytes[i] == target {
            // Check for enough consecutive matches.
            let mut j = 0;
            while i + j < bytes.len() && bytes[i + j] == target {
                j += 1;
            }
            if j >= count {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Truncate to at most `max` **chars** (not bytes) so a multi-byte title
/// never panics on a char boundary; render-only, the stored title is
/// untouched. (A true display-width truncate would need `unicode-width`;
/// for v1 a char cap is enough and keeps the dep surface small.)
fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let full = s.chars().count();
    let mut out: String = s.chars().take(max).collect();
    if full > max {
        out.push('…');
    }
    out
}

/// Truncate `s` to at most `max` **display cells** (width-aware, so CJK /
/// fullwidth titles never push a row past the edge) + one trailing `…`
/// when anything was dropped. Pure + total; `…` itself counts as one cell.
fn ellipsize_cells(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.width() <= max {
        return s.to_string();
    }
    // Walk chars, stopping when adding the next would exceed `max - 1`
    // cells, leaving one cell for the ellipsis marker.
    let budget = max - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let w = c.width().unwrap_or(c.len_utf8());
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// One Browse list row as a `Vec<Span>` (the audit's **finding B**):
///
/// ```text
///  ◆ note:attention  wiki/a.md        ← ◆  = gated (has search_required_phrase)
///  ◇ note:plain      wiki/b.md        ← ◇  = open
/// ```
///
/// The gate marker is double-encoded (the symbol *and* the palette role —
/// `gated` vs `open`) so a colorblind user sees the distinction even
/// after recoloring.
///
/// Truncation priority: the **id** (page identity / title) is kept whole;
/// the **rel_path** (file path / locator) is ellipsized to fit; the gate
/// marker is shown only when there is room for it + a minimum path fragment.
/// This means a user browsing a list always sees the page's name first, and
/// the file location is a secondary, truncatable element.
///
/// Total + width-bounded: the output never exceeds `width` display cells.
/// When the id alone is wider than the pane (a degenerate narrow window),
/// the id is ellipsized and no path or marker is shown.
fn browse_row(p: &Palette, row: &PageRow, width: usize) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let id_w = row.id.width();
    let rel_w = row.rel_path.width();

    // Degenerate: the id alone does not fit the pane → truncate only the
    // id (nothing better to truncate).
    if id_w > width {
        let id = ellipsize_cells(&row.id, width);
        return vec![Span::styled(id, p.default)];
    }

    // The id fits whole.  `remaining` = cells available for [marker +] sep + path.
    let remaining = width - id_w;
    let marker_cells = 3usize; // "◇  " or "◆  "
    let sep_cells = 1usize;

    let (show_marker, path_budget) = if remaining > marker_cells + sep_cells {
        // Room for marker + separator + at least 1 cell of path.
        (true, remaining - marker_cells - sep_cells)
    } else if remaining > sep_cells {
        // Room for separator + at least 1 cell of path, but no marker.
        (false, remaining - sep_cells)
    } else {
        // Not even room for separator after the id → show just the id.
        (false, 0)
    };

    let (marker, mstyle): (&str, Style) = if row.gates.is_empty() {
        ("◇  ", p.open)
    } else {
        ("◆  ", p.gated)
    };

    let mut out = Vec::new();
    if show_marker {
        out.push(Span::styled(marker, mstyle));
    }
    out.push(Span::styled(row.id.clone(), p.default));
    if path_budget > 0 {
        out.push(Span::raw(" "));
        let path = if rel_w <= path_budget {
            row.rel_path.clone()
        } else {
            ellipsize_cells(&row.rel_path, path_budget)
        };
        if !path.is_empty() {
            out.push(Span::styled(path, p.dim));
        }
    }
    out
}

/// The Browse pane's **title**: identity + count only. The keybinding legend
/// lives in the status bar (the one place a user glances for "what do I
/// press"), intentionally *not* here — two copies of the same hints (title +
/// bar) is exactly what clip-doubled on real terminal widths. Pure over the
/// three inputs so the contract is testable without a `Frame`.
fn browse_pane_title(view_len: usize, rows_len: usize, filter: &str) -> String {
    if filter.is_empty() {
        format!(" {} page(s)", view_len)
    } else {
        format!(" {} of {} match '{}'", view_len, rows_len, filter)
    }
}

/// One **outline row**: a single `Line` (never a wrapping `Paragraph`), so a
/// heading like `L3  H2 section` stays on one visible line and is never
/// broken mid-word. The row is indented per heading level; the `L<line>`
/// anchor carries the jump target and is styled `p.dim` (the title is the
/// focus, styled `p.label`); an empty title gets the `¶` glyph (the body's
/// `read_line` convention) in the label role. Pure over the palette + entry,
/// one row per heading, in body order.
fn outline_line(p: &Palette, line: usize, level: usize, title: &str) -> Line<'static> {
    let indent = "  ".repeat(level.saturating_sub(1));
    let anchor = Span::styled(format!("{indent}L{line:<4}"), p.dim);
    if title.is_empty() {
        Line::from(vec![anchor, Span::styled("¶".to_string(), p.label)])
    } else {
        Line::from(vec![anchor, Span::styled(title.to_string(), p.label)])
    }
}

#[cfg(test)]
fn browse_row_text(row: &PageRow, width: usize) -> String {
    let p = Palette::default_palette();
    browse_row(&p, row, width)
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>()
}

// =====================================================================
//  M8.5 — Link-picker render
// =====================================================================

/// Draw the **Link-picker** (M8.5): a single full-width list pane of the
/// source page's links, pre-annotated with resolution. The list is the
/// *same* order as `crate::annotated_links` (document order) — the
/// user's "this is the 3rd link on the page" mental model is preserved;
/// only the *cursor* is pre-computed for the find-armed case.
///
/// Each row is:
///
/// ```text
///  <n>  L<line>  [style]  <target>  → <resolved_id | (N candidates — Enter to pick) | (unresolved)>
///      <context>
/// ```
///
/// Two rows per link (header + context) keeps the context inline, which
/// is what makes this picker agent-friendly (no offscreen scroll needed
/// to see "what did this link point at").
///
/// When `ls.candidates` is `Some` (the inline **candidate-picker** sub-state),
/// the list pane shows the candidate options for the entry currently
/// highlighted — `↑/↓` move the *candidate* cursor (not the flat-list
/// cursor), `Enter` opens the highlighted option's page (via the shared
/// `links_open_selected` / `open_linked` path), `Esc` pops back into the
/// flat list at the entry the candidate was opened from (see
/// `close_links`).
fn draw_links(f: &mut ratatui::Frame, app: &App, ls: &LinksState, area: Rect) {
    // Inline candidate picker (the M8.5 sub-state): a distinct list of
    // candidate options for the currently-open entry. Routed to a
    // separate draw path so the header / title / status all
    // disambiguate from the flat-list case.
    if let Some(cand) = &ls.candidates {
        draw_link_candidates(f, app, ls, cand, area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // list
            Constraint::Length(1), // status
        ])
        .split(area);

    // Find-style rework (T2): the body lives in the left pane and the
    // focused link is highlighted on its source line (all other link
    // occurrences on the page are highlighted lighter). The right pane
    // shows the focused link's target info.
    let header = format!(
        "kb tui — links · {} link(s) on {} · ↑↓/jk move · Enter open · Esc back",
        ls.links.len(),
        ls.source_abs_path
    );
    let header_widget = Paragraph::new(header).style(app.palette.label);
    f.render_widget(header_widget, chunks[0]);

    if ls.links.is_empty() {
        let empty = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("   ", Style::default()),
                Span::styled(
                    "(this page has no links — `l` is a no-op here; Esc back to Read)",
                    app.palette.info,
                ),
            ])
        ])
        .wrap(Wrap { trim: false });
        f.render_widget(empty, chunks[1]);
        return;
    }

    // Two-pane layout: body (60%) | target info (40%).
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[1]);

    // ----- Left pane: body with find-style highlight ----------------------
    // The body text is *identical* to the Read view (the same
    // `splice_body` collapse) — only the highlight differs. The focused
    // link is boxed in `find_current`; every other resolved link keeps the
    // `link` role, and dead / ambiguous links render as plain body text
    // (their state is the right-hand target-info pane, not a prose style).
    let p = &app.palette;

    let spliced_body = splice_body(&ls.source_body, &ls.links);
    let rendered: Vec<String> = spliced_body.iter().map(|l| rendered_line(p, l)).collect();
    let pane_w = (panes[0].width.saturating_sub(2)) as usize;
    let pane_h = (panes[0].height.saturating_sub(2)) as usize;
    let content_rows = rendered_row_count(&rendered, pane_w);
    let focused = ls.cursor.min(ls.links.len() - 1);
    let focused_e = ls.links.get(focused);
    let focused_line = focused_e.map(|e| e.line).unwrap_or(0);
    // First rendered row of the focused line = the sum of row counts of the
    // lines above it (ratatui's own `WordWrapper`).
    let focused_row = if focused_line < rendered.len() {
        rendered_row_count(&rendered[..focused_line], pane_w)
    } else {
        content_rows.saturating_sub(1)
    };
    let fp = crate::tui::find_padding(focused_row, content_rows, pane_h);
    let (pad_above, pad_below) = (fp.above, fp.below);

    // The focused link's (line, col) key — `line_patches` upgrades exactly
    // that one link to `find_current` (overriding its state style). `None`
    // when there is no focused link (degenerate; the picker bails earlier).
    let focused_key = focused_e.map(|e| (e.line, e.col));

    let blank = Line::from(vec![Span::styled(String::new(), p.default)]);
    let mut body_lines: Vec<Line<'static>> = Vec::with_capacity(
        pad_above + spliced_body.len() + pad_below,
    );
    for _ in 0..pad_above {
        body_lines.push(blank.clone());
    }
    for (li, raw) in spliced_body.iter().enumerate() {
        let base = read_line(p, raw);
        // Base rendering (markdown emphasis + the spliced text) + the
        // link-state patches (the focused link gets `find_current`, others
        // keep their state style). `line_patches` returns an empty vec for
        // a line with no links, so `apply_find_patched` is identity there —
        // we can branch-free. NOTE: the offsets are computed on the
        // *rendered* line (markdown emphasis stripped), which is the exact
        // text `read_line` produces — passing `rendered[li]` (not `raw`)
        // keeps the byte offsets aligned.
        let rendered_line = rendered.get(li).map(|s| s.as_str()).unwrap_or("");
        let patches = line_patches(p, rendered_line, &ls.links, li, focused_key);
        body_lines.push(apply_find_patched(base, &patches));
    }
    for _ in 0..pad_below {
        body_lines.push(blank.clone());
    }

    let title = if focused_e.is_some() {
        format!(
            " body — {focused} of {total} · L{line} · Enter open · Esc back",
            focused = focused + 1,
            total = ls.links.len(),
            line = focused_line + 1,
        )
    } else {
        " body — ↑/↓ move".to_string()
    };
    let body_widget = Paragraph::new(body_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title),
        )
        .wrap(Wrap { trim: false })
        .scroll((fp.scroll_top as u16, 0));
    f.render_widget(body_widget, panes[0]);

    // ----- Right pane: target info for the focused link -------------------
    if let Some(e) = focused_e {
        draw_link_target_info(f, app, e, ls.source_body.lines().nth(e.line).unwrap_or(""), panes[1]);
    } else {
        let note = Paragraph::new(vec![
            Line::from(""),
            Line::from(vec![Span::styled(
                "   no focused link — ↑/↓ to move",
                app.palette.info,
            )]),
        ]);
        f.render_widget(note, panes[1]);
    }
}

/// The *render state* of a link — the single source of truth both the
/// Read view (`draw_read`) and the Links view (`draw_links`) consult to
/// decide (a) how to splice / style the written reference, and (b) whether
/// the reference earns the navigable `🔗` marker.
///
/// The body text is *identical* in both views (Read and Links splice the
/// same way — the reader never sees the link "change" between them); only
/// the *highlight* differs (the Links view boxes the focused link, the Read
/// view does not). The splice is *state-sensitive* (see the variant docs):
/// every link collapses to its display text; a resolved *page* link
/// additionally earns the `🔗` marker. There is no *stylistic* distinction
/// between a dead link and a resolved one in prose — dead links read as
/// ordinary body text; the only visual markers a link earns are the `link`
/// tint (resolved) and the `🔗` glyph (uniquely-resolved pages).
///
/// - [`LinkState::Resolved`] — navigable. Either a *uniquely-resolved page*
///   (Db / Path, `resolved = Some(_)`, no candidates) or a well-formed
///   `External` / `Current` reference. The reference splices to its display
///   text and renders in the `link` role; a resolved *page* link
///   additionally earns the `🔗` marker (the "this resolves to a kb page"
///   affordance) while `External` / `Current` do not (they are not kb
///   pages — the marker would mislead).
/// - [`LinkState::Dead`] — Db / Path with `resolved = None` and no
///   candidates: the target page is missing. Splices to its display text
///   and renders in the body's `default` role (plain text) — no strikethrough.
/// - [`LinkState::Ambiguous`] — Db / Path with `resolved = None` but a
///   non-empty candidate set: the reference is real but points at several
///   pages. Splices to its display text and renders in the body's
///   `default` role (plain text) — no italic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Resolved,
    Dead,
    Ambiguous,
}

/// Classify a link into [`LinkState`]. Pure (reads only the entry); the
/// same entry yields the same state in both views.
pub fn link_state(e: &LinksEntry) -> LinkState {
    match e.kind {
        kb_core::links::LinkKind::Db | kb_core::links::LinkKind::Path => {
            if e.resolved.is_some() && e.candidates.is_empty() {
                LinkState::Resolved
            } else if e.candidates.is_empty() {
                LinkState::Dead
            } else {
                LinkState::Ambiguous
            }
        }
        // External links are always well-formed — they earn `↗` (the browser
        // affordance), not `🔗` (they are not kb pages).
        kb_core::links::LinkKind::External => LinkState::Resolved,
        // `Current` links point at a section of the page being read. A
        // present section is Resolved (navigable, earns `🔗`); a missing
        // section is Dead — the reference renders as written (brackets
        // intact, author-applied markdown emphasis preserved) since there is
        // nowhere to navigate to.
        kb_core::links::LinkKind::Current => {
            if e.current_resolved {
                LinkState::Resolved
            } else {
                LinkState::Dead
            }
        }
    }
}

/// True when the link is a *uniquely-resolved kb page* (Db / Path,
/// `resolved = Some(_)`, no candidates) — the one state that earns the
/// `🔗` marker and the splice-to-display-text collapse. `External` /
/// `Current` are `Resolved` but are not kb pages, so they do not get the
/// marker (the bare URI / `#section` is self-evident; they still splice to
/// their display text since the wrapper is pure syntax).
fn is_resolved_page(e: &LinksEntry) -> bool {
    matches!(e.kind, kb_core::links::LinkKind::Db | kb_core::links::LinkKind::Path)
        && e.resolved.is_some()
        && e.candidates.is_empty()
}

/// True when the link splices to its *display text* (collapsing the
/// `[[…]]` / `[…](…)` wrapper): exactly the [`LinkState::Resolved`] links.
/// Dead and Ambiguous links are left rendered *as written* (the full
/// reference, brackets included) so the broken / colliding syntax is still
/// visible — their state is carried by the *author-applied markdown
/// emphasis* (`**…**`, `*…*`, `~~…~~`, `` `…` ``), which
/// [`render_markdown_text`] applies and [`line_patches`] no longer
/// flattens; the Links view's focused box is the other cue.
fn splices_to_display(e: &LinksEntry) -> bool {
    matches!(link_state(e), LinkState::Resolved)
}

/// The display text a spliced link collapses to — the `label` if the inline
/// form carries one, else the written `target`. Markers are appended by
/// destination kind: a resolved *page* link and a `Current` link to a
/// present section earn `🔗` (the navigable-section affordance); an
/// `External` link earns `↗` (open-in-browser affordance).
/// Dead / Ambiguous links are *not* spliced (they remain as written) and
/// never reach this function.
fn splice_display(e: &LinksEntry) -> String {
    let base = e.label.clone().unwrap_or_else(|| e.target.clone());
    match e.kind {
        kb_core::links::LinkKind::External => format!("{base} ↗"),
        kb_core::links::LinkKind::Current if e.current_resolved => format!("{base} 🔗"),
        _ if is_resolved_page(e) => format!("{base} 🔗"),
        _ => base,
    }
}

/// Splice alive links out of a raw line, replacing each full reference
/// span (`[label](path)`, `[[target]]`, `<url>`) with just its display
/// text (the `label` for the inline form, otherwise the `target`),
/// exactly the way `render_markdown_text` strips `**` / `*` / `~~` /
/// backticks. Returns the spliced line.
///
/// `ranges` is `(col, end, display_text)` — byte offsets into `raw`
/// (the unmodified line, which is what `col`/`end` were measured on).
/// The slice is built high-to-low (`col` descending) so that an earlier
/// splice never invalidates a later range; the result preserves the
/// surrounding prose exactly.
fn splice_links_on_line(
    raw: &str,
    ranges: &[(usize, usize, String)],
) -> String {
    if ranges.is_empty() {
        return raw.to_string();
    }
    // Sort by `col` descending so we splice from the end of the line
    // first — earlier splice offsets stay valid for later splices.
    let mut sorted: Vec<&(usize, usize, String)> = ranges.iter().collect();
    sorted.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut out = raw.to_string();
    for (col, end, display) in sorted {
        if *col < *end && *col < out.len() {
            let e = (*end).min(out.len());
            out.replace_range(*col..e, display);
        }
    }
    out
}

/// Splice a full body against its link annotations, returning the
/// **spliced** per-line text — the text the user actually sees (the same
/// in both the Read and Links views). This is the single source of truth
/// for the body pane: `draw_read`, `draw_links`, the scroll clamp, the
/// find-match search and the centering math all consume this rather than
/// the raw body, so the scroll unit and the highlight offsets always land
/// on what is on screen.
///
/// State-sensitive (see [`LinkState`]): *resolved* links collapse to their
/// display text — the `[[…]]` / `[…](…)` wrapper is pure reference syntax
/// the reader does not read. Markers follow destination kind: a resolved
/// *page* link or a `Current` link to a present section gains `🔗`
/// (navigable affordance); an `External` link gains `↗` (browser
/// affordance). *Dead* and *Ambiguous* links are left rendered **as
/// written** (the full reference, brackets included) so the broken /
/// colliding syntax is still visible — their state is carried by the
/// author-applied markdown emphasis (which [`render_markdown_text`]
/// applies and [`line_patches`] no longer flattens) and by the Links view's
/// target info. Prose, emphasis delimiters and any line with no link is
/// preserved byte-for-byte.
///
/// Pure over `(body, links)`; no `App`, no IO — so it is unit-testable and
/// idempotent (the same inputs give the same body the render pass paints).
pub fn splice_body(body: &str, links: &[LinksEntry]) -> Vec<String> {
    body.lines()
        .enumerate()
        .map(|(li, raw)| {
            let resolved: Vec<(usize, usize, String)> = links
                .iter()
                .filter(|e| e.line == li && splices_to_display(e))
                .map(|e| (e.col, e.end, splice_display(e)))
                .collect();
            if resolved.is_empty() {
                raw.to_string()
            } else {
                splice_links_on_line(raw, &resolved)
            }
        })
        .collect()
}

/// The (start, end, style) patches to overlay on a *spliced* body line
/// (byte offsets into `rendered` — the line as painted by
/// [`rendered_line`]) so the link reads with state-appropriate emphasis.
/// Both the Read view (`draw_read`) and the Links view (`draw_links`) call
/// this for the **base** styling — the body line is identical in the two —
/// and the Links view additionally passes `focused_key` to upgrade the
/// focused link to `find_current` (the strong box) while the others keep
/// their state style.
///
/// Every link has already gone through [`splice_body`]; this overlay only
/// adds the state-appropriate style:
///
/// - **Resolved** (a real, navigable reference): spliced to its display
///   text; the highlight is the `link` role (a saturated, unboxed link
///   tint — distinct from the `md_code` box that is reserved for backtick
///   spans). A resolved *page* link keeps the `🔗` marker in the
///   surrounding prose; this box (the `link` tint) wraps the display text.
/// - **Dead** (target page / section missing): left **as written** (the
///   full `[[…]]` / `[…](…)` span) with **no style patch** unless focused —
///   the author picks how a dead link reads by wrapping it in normal
///   markdown (`**[dead]**` bold, `*[dead]*` italic, `~~[dead]~~` strike,
///   `` `[dead]` `` code) and [`render_markdown_text`] applies it. A focused
///   dead link is boxed in `find_current` (the Links-view affordance).
/// - **Ambiguous** (several candidate pages): likewise as written in the
///   author-chosen markdown style; the only cue for an unfocused entry is
///   the Links view's box on the focused one.
///
/// `focused_key` is the `(line, col)` of the focused link (Links view only;
/// `None` in the Read view). When it matches a link on this line that link's
/// style is `find_current` (overriding the state style) — so the focused
/// dead / ambiguous / resolved link is *always* boxed, which is the fix for
/// the "no highlight on a dead/ambiguous link" bug.
///
/// ATX headings are exempt (a heading line renders wholly in `label`; a link
/// highlight on it would fight the heading role — a link on a heading line
/// is a degenerate case we leave unstyled).
pub fn line_patches(
    p: &Palette,
    rendered: &str,
    links: &[LinksEntry],
    line: usize,
    focused_key: Option<(usize, usize)>,
) -> Vec<(usize, usize, Style)> {
    if is_heading(rendered) {
        return Vec::new();
    }
    let mut out: Vec<(usize, usize, Style)> = Vec::new();
    for e in links.iter().filter(|e| e.line == line) {
        // The focused link's style is `find_current` (the Links-view
        // affordance); everything else is the state's base role.
        let is_focused = focused_key.map(|(fl, fc)| fl == e.line && fc == e.col).unwrap_or(false);
        // Dead / Ambiguous links stay as written; unless focused we emit
        // *no* patch at all so the markdown emphasis the author applied
        // (via `**`, `*`, `~~`, backticks) carries through — a `p.default`
        // overlay would flatten it.
        if !is_focused && matches!(link_state(e), LinkState::Dead | LinkState::Ambiguous) {
            continue;
        }
        // Resolved → `link` tint unless focused; focused (any state) →
        // `find_current` box. (Dead / ambiguous only reach this point when
        // focused — non-focused dead/ambiguous were skipped above.)
        let style = if is_focused { p.find_current } else { p.link };
        // Resolved links spliced to their display text (`label` if the
        // inline form has one, else the `target`); Dead / Ambiguous stayed
        // as written. The needle below is exactly what `rendered` shows.
        let needle = if matches!(link_state(e), LinkState::Resolved) {
            e.label.clone().unwrap_or_else(|| e.target.clone())
        } else {
            e.raw_text.clone()
        };
        let (s, en) = raw_to_rendered_span(rendered, &needle);
        if s < en {
            out.push((s, en, style));
        }
    }
    out
}

/// Find a link's visible text in the rendered line and return the
/// `(rendered_start, rendered_end)` byte offsets to highlight, or
/// `(0, 0)` to skip.
///
/// `text_to_find` is what the user actually reads — `label` when the
/// link has one (the `[ label ](target)` form), else the raw
/// `target` (wikilinks show `target` verbatim inside `[[…]]`, so it is
/// present in the rendered line as-is).
///
/// We deliberately do NOT try to paint the full reference span
/// (`[[target]]` or `[label](target)`). The user's visual model is the
/// *inner* text — that is what the reader sees and what `kb links`
/// shows in the target column. `render_markdown_text` strips emphasis
/// delimiters around that text (if any), so a plain string search on
/// the rendered line is both correct and robust.
fn raw_to_rendered_span(
    rendered_line: &str,
    text_to_find: &str,
) -> (usize, usize) {
    let needle = text_to_find;
    if needle.is_empty() {
        return (0, 0);
    }
    // Prefer the first occurrence in the rendered line. (A link's inner
    // text appearing twice in the same line is rare; the first is the
    // user's intent.)
    rendered_line.find(needle).map(|s| (s, s + needle.len())).unwrap_or((0, 0))
}

/// Draw the right-hand **target info** pane for the focused link: the
/// resolved page (id/type/title/tags), a rendered section (if any), and a
/// distinct callout for dead / ambiguous / external / current.
fn draw_link_target_info(
    f: &mut ratatui::Frame,
    app: &App,
    e: &crate::tui::LinksEntry,
    raw_line: &str,
    area: Rect,
) {
    let p = &app.palette;
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Header: the raw line (one-line context, highlighted where the focused
    // link's visible text is) so the user can verify the source. We reuse
    // the same string-search strategy the body pane uses to find the
    // link's inner text (`label` or `target`) in the raw line.
    let t = raw_line.trim();
    if !t.is_empty() {
        let needle = e.label.clone().unwrap_or_else(|| e.target.clone());
        let (s, en) = raw_to_rendered_span(t, &needle);
        if en > s {
            let before = t[..s].to_string();
            let mid = t[s..en].to_string();
            let after = t[en..].to_string();
            lines.push(Line::from(vec![
                Span::styled(" source ", p.label),
                Span::styled(truncate(&before, 20), p.dim),
                Span::styled(truncate(&mid, 40), p.find_current),
                Span::styled(truncate(&after, 20), p.dim),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(" source ", p.label),
                Span::styled(truncate(t, 60), p.dim),
            ]));
        }
        lines.push(Line::from(""));
    }
    // Kind (short tag + the full word, so it reads even with a custom palette).
    let kind_word = match e.kind {
        kb_core::links::LinkKind::Db => "wikilink",
        kb_core::links::LinkKind::Path => if e.target.starts_with('/') { "abs-path" } else { "rel-path" },
        kb_core::links::LinkKind::External => "external",
        kb_core::links::LinkKind::Current => "current",
    };
    lines.push(Line::from(vec![
        Span::styled(" kind     ", p.label),
        Span::styled(format!("[{}] {}", e_style_str(e), kind_word), p.info),
    ]));
    // Target.
    if let Some(lbl) = &e.label {
        lines.push(Line::from(vec![
            Span::styled(" label    ", p.label),
            Span::styled(lbl.clone(), p.default),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled(" target   ", p.label),
        Span::styled(e.target.clone(), p.default),
    ]));
    if let Some(sec) = &e.section {
        lines.push(Line::from(vec![
            Span::styled(" section  ", p.label),
            Span::styled(format!("#{sec}"), p.info),
        ]));
    }
    lines.push(Line::from(""));
    // Resolution status.
    match (&e.resolved, e.kind, !e.candidates.is_empty()) {
        (Some(id), _, false) => {
            lines.push(Line::from(vec![
                Span::styled(" resolves ", p.label),
                Span::styled("→ ".to_string(), p.success),
                Span::styled(id.clone(), p.success.add_modifier(Modifier::BOLD)),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(" status   ", p.label), Span::styled("resolved".to_string(), p.success)]));
        }
        (None, kb_core::links::LinkKind::External, _) => {
            lines.push(Line::from(vec![
                Span::styled(" status   ", p.label),
                Span::styled("(external — not a kb page)".to_string(), p.info),
            ]));
        }
        (None, kb_core::links::LinkKind::Current, _) => {
            lines.push(Line::from(vec![
                Span::styled(" status   ", p.label),
                Span::styled("(points to this page)".to_string(), p.info),
            ]));
        }
        (None, _, true) => {
            lines.push(Line::from(vec![
                Span::styled(" status   ", p.label),
                Span::styled(
                    format!("(ambiguous — {} candidates)", e.candidates.len()),
                    p.hint,
                ),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(" candidates", p.label)]));
            for (i, cand) in e.candidates.iter().enumerate() {
                let disp = format!("  {}. {cand}", i + 1);
                lines.push(Line::from(vec![Span::styled(disp, p.dim)]));
            }
        }
        (&Some(_), _, true) | (None, _, false) => {
            lines.push(Line::from(vec![
                Span::styled(" status   ", p.label),
                Span::styled("(unresolved — no page by that name)".to_string(), p.error),
            ]));
        }
    }

    // The right pane's title is informational only — the left pane's title
    // already reports "N of M". We keep the right title stable: the focused
    // entry index (the caller passed the focused entry in).
    let widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" target info "),
        )
        .wrap(Wrap { trim: false })
        .style(Style::default());
    f.render_widget(widget, area);
}


/// Build the row of spans for **one** candidate option in the inline
/// candidate picker's list. `idx` is the 0-based position in the list
/// (1-based for display, right-aligned to 3 cells); `on` selects
/// highlighted vs. not.
///
/// Extracted from `draw_link_candidates` so it can be unit-tested for
/// the *flattened* text the user reads (the same coverage convention as
/// `browse_row_text`).
fn candidate_row_spans(
    p: &Palette,
    idx: usize,
    opt: &LinkCandOption,
    on: bool,
) -> Vec<Span<'static>> {
    let n_str = format!("{:>3}  ", idx + 1);
    let base = if on {
        p.active.add_modifier(Modifier::BOLD)
    } else {
        p.default
    };
    let path_style = if on { p.info } else { p.dim };
    vec![
        Span::styled(n_str, base),
        Span::styled(opt.display.clone(), base),
        Span::styled("   ", base),
        Span::styled(format!("({})", opt.abs_path), path_style),
    ]
}

/// Flattened text for a candidate option's row (for unit tests — same
/// convention as `browse_row_text`).
#[cfg(test)]
fn candidate_row_text(idx: usize, opt: &LinkCandOption, on: bool) -> String {
    let p = Palette::default_palette();
    candidate_row_spans(&p, idx, opt, on)
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>()
}

/// Draw the **inline candidate picker** (the M8.5 sub-state of the
/// link-picker): the candidate options for a single ambiguous entry the
/// user pressed `Enter` on. The flat-list state (`ls.links`,
/// `ls.cursor`, `ls.source_abs_path`) is *still present* (the overlay
/// "zooms in" on a single entry — not a separate `Mode` variant); this
/// draw shows the candidate list in the same list pane, with the
/// *overlay's* cursor (`cand.cursor`) as the selection, and the
/// *source* entry's target (`cand.target`) in the header (so the user
/// sees which link they are disambiguating without scrolling back to
/// the flat list).
///
/// Keys: `↑`/`↓`/`k`/`j` move the candidate highlight (wrap-around —
/// see `App::move_link_candidate`), `Home`/`End` jump, `Enter` opens the
/// highlighted page (routed through the shared
/// `links_open_selected`/`open_linked` path), `x`/`Esc` pops back to the
/// flat list at the entry the candidate was opened from
/// (`cand.entry_idx`).
fn draw_link_candidates(
    f: &mut ratatui::Frame,
    app: &App,
    ls: &LinksState,
    cand: &crate::tui::LinksCandidates,
    area: Rect,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // list
            Constraint::Length(1), // status
        ])
        .split(area);

    // Header: name the entry we are disambiguating + the target. The user
    // is "zoomed in" on entry `cand.entry_idx` of `ls.links` (the flat
    // list's cursor still points at that entry — we don't touch it), so
    // the header makes that explicit.
    let entry_idx = cand.entry_idx;
    let entry_count = ls.links.len();
    let header = format!(
        "kb tui — links · disambiguating `[[{target}]]` (entry {entry} of {count} on {source}) · ↑↓/jk pick · Enter open · Esc back",
        target = cand.target,
        entry = entry_idx + 1,
        count = entry_count,
        source = ls.source_abs_path,
    );
    let header_widget = Paragraph::new(header).style(app.palette.label);
    f.render_widget(header_widget, chunks[0]);

    if cand.options.is_empty() {
        // Defensive — the overlay is only set on an *ambiguous* entry, so
        // `options.len() >= 2` by construction. But the picker stays
        // total: render an empty-state note rather than a blank pane.
        let empty = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("   ", Style::default()),
                Span::styled(
                    "(no candidates — Esc back to the link list)",
                    app.palette.info,
                ),
            ])
        ])
        .wrap(Wrap { trim: false });
        f.render_widget(empty, chunks[1]);
        return;
    }

    // The flat-list cursor (the entry the candidate was opened from) is
    // read out of `ls` so the title shows *both* the entry index and the
    // source page — the user can still tell "I'm zoomed into entry 2 of 5"
    // even from the candidate view.
    let flat_entry = cand.entry_idx;
    let flat_count = ls.links.len();

    // Build the ratatui list — one item per candidate option. Each row
    // is `<n>  <display>   (<abs_path>)` so the user can see both the
    // human id AND the navigation identity (a `type:slug` display id can
    // be shared by multiple pages — e.g. `note:A` for two different
    // bodies — and the `abs_path` is the stable key `open_linked`
    // consumes). The number is 1-based and right-aligned for a column of
    // candidate rows; the `abs_path` is de-emphasized (dimmed for
    // unselected, info for the highlighted row) so the display id stays
    // the focal point.
    let items: Vec<ListItem> = cand
        .options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let on = i == cand.cursor;
            let spans = candidate_row_spans(&app.palette, i, opt, on);
            ListItem::new(vec![Line::from(spans)])
        })
        .collect();

    // ratatui 0.29 — same shape as the flat-list draw above. `highlight_style`
    // is the `active` palette slot (consistent with the other pickers).
    let cand_cursor_plus = cand.cursor + 1;
    let cand_count = cand.options.len();
    let list = RatList::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(app.palette.active)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    "candidates for `[[{target}]]` (entry {flat} of {total} on {source}) · {cc} of {ccn}",
                    target = cand.target,
                    flat = flat_entry + 1,
                    total = flat_count,
                    source = ls.source_abs_path,
                    cc = cand_cursor_plus,
                    ccn = cand_count,
                )),
        );
    let mut state = ListState::default();
    state.select(Some(cand.cursor));
    f.render_stateful_widget(list, chunks[1], &mut state);
}

/// The `LinkKind` as a short display tag (the CLI renders the same enum via
/// `kb_core::links::LinkKind::style_str`).
fn e_style_str(e: &crate::tui::LinksEntry) -> &'static str {
    match e.kind {
        kb_core::links::LinkKind::Db => "wik",
        kb_core::links::LinkKind::Path => {
            if e.target.starts_with('/') {
                "abs"
            } else {
                "rel"
            }
        }
        kb_core::links::LinkKind::External => "ext",
        kb_core::links::LinkKind::Current => "cur",
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, rel: &str) -> PageRow {
        PageRow {
            abs_path: String::new(),
            id: id.to_string(),
            pt: String::new(),
            title: String::new(),
            rel_path: rel.to_string(),
            root: String::new(),
            tags: Vec::new(),
            gates: Vec::new(),
        }
    }

    /// `…` is drawn in one cell by a terminal; measure a string in *cells*
    /// (width-aware) so CJK/fullwidth chars count as two, matching what the
    /// user actually sees on screen.
    fn cells(s: &str) -> usize {
        s.width()
    }

    /// The whole point of the fix: a row must **never overflow** its pane,
    /// and the **id (page identity/title) is kept whole** — it is the thing
    /// a user reads to tell pages apart. The path (rel_path) is the element
    /// that truncates. "Fills exactly" is *not* a real guarantee — a CJK cut
    /// can leave a 1-cell gap that ratatui pads with a trailing space — so
    /// we assert `<= width`, never `== width`. The row is a span list (P6:
    /// the `◆`/`◇` marker needs a palette role); the tests compare the
    /// *flattened* text via `browse_row_text`.
    /// `…` is drawn in one cell by a terminal; measure a string in *cells*
    /// (width-aware) so CJK/fullwidth chars count as two, matching what the
    /// user actually sees on screen.
    #[test]
    fn browse_row_never_overflows_and_keeps_the_id() {
        let long_id = "note:Realistic Document Title with a lot of words";
        let rel = "real60.md";
        let id_cells = cells(long_id);
        for w in [1usize, 5, 20, 38, 40, 50, 78, 100, 200] {
            let out = browse_row_text(&row(long_id, rel), w);
            let got = cells(&out);
            assert!(got <= w, "row ({out:?}) is {got} cells, must be <= {w}");
        }
        // The id is the identity and stays whole whenever it fits the pane.
        // A wide-enough pane: id whole + path whole (no ellipsis).
        let out = browse_row_text(&row(long_id, rel), id_cells + cells(rel) + 5);
        assert!(out.contains(long_id), "id whole: {out:?}");
        assert!(out.contains(rel), "path whole: {out:?}");
        assert!(!out.contains('…'), "no ellipsis at this width: {out:?}");
    }

    /// Degenerate: the id alone overflows the whole pane → the id truncates
    /// (nothing else can compete for the space) and the row is still bounded.
    #[test]
    fn browse_row_truncates_the_id_when_it_alone_overflows() {
        let id = "note:".to_string() + &"a".repeat(120); // 125 cells > 40
        let out = browse_row_text(&row(&id, "x.md"), 40);
        assert!(cells(&out) <= 40);
        assert!(!out.contains("x.md"), "no path once the id fills the pane: {out:?}");
        assert!(out.contains('…'), "the cut id carries an ellipsis: {out:?}");
    }

    /// When id + path together exceed the pane, the **path** is the element
    /// that ellipsizes (the id stays whole) — never a bare clip of the id.
    #[test]
    fn browse_row_ellipsizes_the_path_not_the_id() {
        let id = "note:".to_string() + &"a".repeat(12); // 17 cells — fits a 40 pane
        let rel = "dir/a/b/c/very/long/relative/path/segment.md"; // 45 cells > budget
        let w = 40;
        let out = browse_row_text(&row(&id, rel), w);
        assert!(cells(&out) <= w);
        assert!(out.contains(&id), "id keeps whole when it fits the budget: {out:?}");
        assert!(!out.ends_with(rel), "path is what truncates, not the id: {out:?}");
        assert!(out.contains('…'), "the cut path must carry an ellipsis: {out:?}");
    }

    /// P8: every *populated* field in the picker shows `◆` styled with
    /// `p.gated`; every *empty* field shows `◇` styled with `p.open`.
    /// The fixture below has `title` + `tags` set, so those two must read
    /// `◆`, while `description` + `search_required_phrase` (absent) read `◇`.
    #[test]
    fn picker_rows_carry_gate_markers_for_each_field() {
        let p = Palette::default_palette();
        let fixture = "---\ntype: note\ntitle: P8\ntags:\n- a\n- b\n---\nbody\n";
        let es = crate::tui::EditState {
            field: crate::tui::EditField::Title,
            abs_path: "/fake/p8.md".into(),
            id: "note:P8".into(),
            value: "x".into(),
            suggestions: Vec::new(),
            corpus: Vec::new(),
            entry_bytes: fixture.as_bytes().to_vec(),
            entry_list: Vec::new(),
            current_list: Vec::new(),
            working_bytes: fixture.as_bytes().to_vec(),
            stage: crate::tui::EditStage::Value,
            field_cursor: 0,
            sug_cursor: 0,
            value_cursor: 1,
            in_list_select: false,
            list_high: 0,
            editing_item: None,
        };
        let rows = build_picker_rows(&p, &es);
        assert_eq!(rows.len(), 4, "one row per field");
        // Order matches EditField::all(): Title, Description, Tags, SearchRequiredPhrase.
        let expect = [
            ("◆", p.gated),
            ("◇", p.open),
            ("◆", p.gated),
            ("◇", p.open),
        ];
        for (i, (line, (m, s))) in rows.iter().zip(expect.iter()).enumerate() {
            let first = &line.spans[0];
            assert_eq!(first.content.as_ref(), *m, "row {i} first span content");
            assert_eq!(first.style, *s, "row {i} first span style");
        }
    }
    /// `render_markdown_text` recognizes `**bold**`, `*ital*` / `_ital_`,
    /// `~~strike~~`, and `` `code` ``; unmatched delimiters pass through as
    /// the `default` role. This is the byte-identical guarantee: any line
    /// with no recognized delimiters produces a single `default` span.
    #[test]
    fn markdown_bold_italic_strikethrough_render() {
        let p = Palette::default_palette();
        // A plain line (no delimiters) is a single `default` span — the
        // byte-identical guarantee to the pre-markdown renderer.
        let plain = render_markdown_text(&p, "just a line");
        assert_eq!(plain.spans.len(), 1);
        assert_eq!(plain.spans[0].style, p.default);
        // **bold** → one `md_bold` span.
        let b = render_markdown_text(&p, "**bold**");
        assert_eq!(b.spans.len(), 1);
        assert_eq!(b.spans[0].style, p.md_bold);
        // *italic* → one `md_italic` span.
        let it = render_markdown_text(&p, "*ital*");
        assert_eq!(it.spans.len(), 1);
        assert_eq!(it.spans[0].style, p.md_italic);
        // _italic_ → one `md_italic` span.
        let it2 = render_markdown_text(&p, "_ital_");
        assert_eq!(it2.spans.len(), 1);
        assert_eq!(it2.spans[0].style, p.md_italic);
        // ~~strike~~ → one `md_strike` span.
        let st = render_markdown_text(&p, "~~gone~~");
        assert_eq!(st.spans.len(), 1);
        assert_eq!(st.spans[0].style, p.md_strike);
        // A mixed line preserves order and strips the delimiters from the
        // spans; 7 spans: 4 default + bold + italic + strike.
        let mixed = render_markdown_text(&p, "start **bold** mid *it* end ~~st~~ end");
        assert_eq!(mixed.spans.len(), 7, "expected 7 spans, got {:?}", mixed.spans.len());
        assert_eq!(mixed.spans[0].style, p.default);
        assert_eq!(mixed.spans[1].style, p.md_bold);
        assert_eq!(mixed.spans[2].style, p.default);
        assert_eq!(mixed.spans[3].style, p.md_italic);
        assert_eq!(mixed.spans[4].style, p.default);
        assert_eq!(mixed.spans[5].style, p.md_strike);
        assert_eq!(mixed.spans[6].style, p.default);
        assert_eq!(mixed.spans[0].content.as_ref(), "start ");
        assert_eq!(mixed.spans[1].content.as_ref(), "bold");
        assert_eq!(mixed.spans[2].content.as_ref(), " mid ");
        assert_eq!(mixed.spans[3].content.as_ref(), "it");
        assert_eq!(mixed.spans[4].content.as_ref(), " end ");
        assert_eq!(mixed.spans[5].content.as_ref(), "st");
        assert_eq!(mixed.spans[6].content.as_ref(), " end");
    }
    /// A literal `*` not followed by text is rendered as-is (one `default`
    /// span). This is the "pass through unrecognized" guarantee: no
    /// delimiter, no crash, no silent drop.
    #[test]
    fn markdown_unmatched_delimiters_are_literal() {
        let p = Palette::default_palette();
        // A single `*` with no closer.
        let one = render_markdown_text(&p, "5 * 3 = 15");
        assert_eq!(one.spans.len(), 1, "no spurious spans: #{:?}", one.spans.len());
        assert_eq!(one.spans[0].style, p.default);
        assert_eq!(one.spans[0].content.as_ref(), "5 * 3 = 15");
        // `a ** bold` (unmatched `**` with no closer) renders literally.
        let two = render_markdown_text(&p, "a ** bold");
        assert_eq!(two.spans.len(), 1);
        assert_eq!(two.spans[0].style, p.default);
        assert_eq!(two.spans[0].content.as_ref(), "a ** bold");
        // A single `~` (not a pair) renders literally.
        let one_t = render_markdown_text(&p, "a ~ b");
        assert_eq!(one_t.spans.len(), 1);
        assert_eq!(one_t.spans[0].style, p.default);
        assert_eq!(one_t.spans[0].content.as_ref(), "a ~ b");
    }
    /// `***bold_italic***` falls through to literal (we don't match a 3-char
    /// run). `**bold** *ital*` and `**bold**_ital_` are two spans each.
    #[test]
    fn markdown_bold_italic_adjacent() {
        let p = Palette::default_palette();
        // ***bold_italic*** — the scanner eats the first `*` as a literal,
        // then recognises `**bold_italic**` as bold (the closer is found),
        // then a trailing `*` remains literal.
        let bi = render_markdown_text(&p, "***bold_italic***");
        assert_eq!(bi.spans.len(), 3,
            "*** renders as literal * + bold span + literal *: {:?}", bi.spans.len());
        assert_eq!(bi.spans[0].style, p.default);
        assert_eq!(bi.spans[0].content.as_ref(), "*");
        assert_eq!(bi.spans[1].style, p.md_bold);
        assert_eq!(bi.spans[1].content.as_ref(), "bold_italic");
        assert_eq!(bi.spans[2].style, p.default);
        assert_eq!(bi.spans[2].content.as_ref(), "*");
        // `__ital__` (double underscore) is NOT bold — no match.
        let du = render_markdown_text(&p, "__ital__");
        assert_eq!(du.spans.len(), 1, "__ is literal, not bold: {:?}", du.spans.len());
        assert_eq!(du.spans[0].style, p.default);
        let ba = render_markdown_text(&p, "**bold** *ital*");
        assert_eq!(ba.spans.len(), 3);
        assert_eq!(ba.spans[0].style, p.md_bold);
        assert_eq!(ba.spans[1].style, p.default);
        assert_eq!(ba.spans[2].style, p.md_italic);
        let bu = render_markdown_text(&p, "**bold**_ital_");
        assert_eq!(bu.spans.len(), 2);
        assert_eq!(bu.spans[0].style, p.md_bold);
        assert_eq!(bu.spans[1].style, p.md_italic);
    }
    /// CJK inside a span: the CJK character (multibyte, no 0x2A byte inside)
    /// is included in the bold span verbatim; the delimiters are stripped.
    /// This guards against a byte-slice bug in the scanner.
    #[test]
    fn markdown_bold_with_cjk_inside() {
        let p = Palette::default_palette();
        let cjk = render_markdown_text(&p, "**量**");
        assert_eq!(cjk.spans.len(), 1);
        assert_eq!(cjk.spans[0].style, p.md_bold);
        // The content is the CJK char itself (the delimiters are stripped).
        assert!(cjk.spans[0].content.chars().all(|c| c == '量'),
            "no half-width fragment: {:?}", cjk.spans[0].content);
    }
    /// `` `code` `` → one `md_code` span (the delimiters are stripped).
    /// The `md_code` role carries a single style (no bold/italic by default)
    /// so a code span stays visually distinct from the prose.
    #[test]
    fn markdown_inline_code_render() {
        let p = Palette::default_palette();
        let c = render_markdown_text(&p, "`code`");
        assert_eq!(c.spans.len(), 1);
        assert_eq!(c.spans[0].style, p.md_code);
        assert_eq!(c.spans[0].content.as_ref(), "code");
        // Mixed line: prose + code span + prose.
        let m = render_markdown_text(&p, "use `foo()` to do it");
        assert_eq!(m.spans.len(), 3);
        assert_eq!(m.spans[0].style, p.default);
        assert_eq!(m.spans[1].style, p.md_code);
        assert_eq!(m.spans[2].style, p.default);
        assert_eq!(m.spans[0].content.as_ref(), "use ");
        assert_eq!(m.spans[1].content.as_ref(), "foo()");
        assert_eq!(m.spans[2].content.as_ref(), " to do it");
    }
    /// The inner text of a code span is *opaque*: `**bold**`, `*it*`, and
    /// `~~st~~` inside a `…` span are literal (not emphasis). This is a
    /// strong test of the scanner's greedy closer: the scanner must match
    /// the *first* backtick to close the span, not the last one in the line.
    #[test]
    fn markdown_code_span_is_opaque() {
        let p = Palette::default_palette();
        // `**bold**` inside a code span stays literal: one `md_code` span.
        let c = render_markdown_text(&p, "`**bold**`");
        assert_eq!(c.spans.len(), 1, "opaque code span: {:?}", c.spans.len());
        assert_eq!(c.spans[0].style, p.md_code);
        assert_eq!(c.spans[0].content.as_ref(), "**bold**");
        // `*it*` and `~~st~~` inside a code span are literal too.
        let ci = render_markdown_text(&p, "`*it*`");
        assert_eq!(ci.spans.len(), 1);
        assert_eq!(ci.spans[0].style, p.md_code);
        assert_eq!(ci.spans[0].content.as_ref(), "*it*");
        let cst = render_markdown_text(&p, "`~~st~~`");
        assert_eq!(cst.spans.len(), 1);
        assert_eq!(cst.spans[0].style, p.md_code);
        assert_eq!(cst.spans[0].content.as_ref(), "~~st~~");
    }
    /// Unmatched backticks render literally. `` ``` ` ` ``` ` ` ` ``` are
    /// all literal — the scanner does not match a 3-char run as a closer.
    #[test]
    fn markdown_unmatched_backticks_are_literal() {
        let p = Palette::default_palette();
        // A lone backtick with no closer.
        let one = render_markdown_text(&p, "a ` b");
        assert_eq!(one.spans.len(), 1, "lone backtick: {:?}", one.spans.len());
        assert_eq!(one.spans[0].style, p.default);
        assert_eq!(one.spans[0].content.as_ref(), "a ` b");
        // Triple backticks — all three are literal, NOT the fenced-block
        // opener this scanner doesn't support.
        let three = render_markdown_text(&p, "```");
        assert_eq!(three.spans.len(), 1, "``` all literal: {:?}", three.spans.len());
        assert_eq!(three.spans[0].style, p.default);
        assert_eq!(three.spans[0].content.as_ref(), "```");
    }
    /// A code span adjacent to prose that contains delimiters: the delimiters
    /// in prose *outside* the code span still render as emphasis.
    #[test]
    fn markdown_code_span_preserves_surrounding_emphasis() {
        let p = Palette::default_palette();
        // `**bold**` *around* a code span is bold + code + bold.
        let m = render_markdown_text(&p, "**bold** `code` **more**");
        assert_eq!(m.spans.len(), 5);
        assert_eq!(m.spans[0].style, p.md_bold);
        assert_eq!(m.spans[1].style, p.default);
        assert_eq!(m.spans[2].style, p.md_code);
        assert_eq!(m.spans[3].style, p.default);
        assert_eq!(m.spans[4].style, p.md_bold);
        assert_eq!(m.spans[0].content.as_ref(), "bold");
        assert_eq!(m.spans[1].content.as_ref(), " ");
        assert_eq!(m.spans[2].content.as_ref(), "code");
        assert_eq!(m.spans[3].content.as_ref(), " ");
        assert_eq!(m.spans[4].content.as_ref(), "more");
    }
    /// The md_code role is distinct from md_bold / md_italic / md_strike /
    /// default — a palette user who wants a visually-different code span can
    /// pick one that differentiates (the default palette uses a Green-on-
    /// DarkGray "code box").
    #[test]
    fn markdown_code_role_is_distinct() {
        let p = Palette::default_palette();
        assert_ne!(p.md_code, p.md_bold, "md_code != md_bold");
        assert_ne!(p.md_code, p.md_italic, "md_code != md_italic");
        assert_ne!(p.md_code, p.md_strike, "md_code != md_strike");
        assert_ne!(p.md_code, p.default, "md_code != default");
        // The default palette paints md_code as a White-on-DarkGray "code
        // box" (no bold/italic), distinct from the LightYellow+BOLD md_bold
        // and the LightCyan+ITALIC md_italic. The box is what makes it read
        // as code.
        assert_eq!(
            p.md_code,
            Style::default().fg(Color::White).bg(Color::DarkGray),
            "default palette md_code is a White-on-DarkGray code box"
        );
    }
    /// `label` role; body lines keep their default style. The check is
    /// visual: 0, 7+, or a `##` without a trailing space are *not* headings.
    #[test]
    fn read_line_styles_headings_only() {
        let p = Palette::default_palette();
        // A labelled heading and a plain line must *differ* in style.
        let h1 = read_line(&p, "# Title");
        let body = read_line(&p, "body line");
        assert_ne!(
            h1.spans[0].style, body.spans[0].style,
            "heading must use the label style; body must not"
        );
        assert_eq!(
            h1.spans[0].style, p.label,
            "heading style == palette.label"
        );
        // All six heading levels style as labels.
        for level in 1..=6u8 {
            let h = read_line(&p, &format!("{} Sec", "#".repeat(level as usize)));
            assert_eq!(h.spans[0].style, p.label, "level {level} styles as label");
        }
        // Non-headings must NOT pick up the label role.
        for line in ["", "   ", "####### seven hashes", "##nospace", "#", "text#tail"] {
            let l = read_line(&p, line);
            assert_ne!(l.spans[0].style, p.label, "{line:?} must not style as heading");
        }
    }
    /// (open) — so a user scanning the browse list sees at a glance which
    /// pages require a phrase before search. The marker is only drawn when
    /// it fits the pane; the two cases are otherwise byte-identical.
    #[test]
    fn browse_row_marks_gated_open_differently() {
        let gated = PageRow {
            abs_path: String::new(),
            id: "note:attention".to_string(),
            pt: String::new(),
            title: String::new(),
            rel_path: "wiki/a.md".to_string(),
            root: String::new(),
            tags: Vec::new(),
            gates: vec!["mTLS".to_string()],
        };
        let open = PageRow {
            abs_path: String::new(),
            id: "note:plain".to_string(),
            pt: String::new(),
            title: String::new(),
            rel_path: "wiki/b.md".to_string(),
            root: String::new(),
            tags: vec!["rust".to_string()],
            gates: Vec::new(),
        };
        let p = Palette::default_palette();
        for w in [4usize, 10, 30, 80] {
            let a = browse_row(&p, &gated, w);
            let b = browse_row(&p, &open, w);
            let first_open = b.first().map(|s| s.content.as_ref());
            let first_gated = a.first().map(|s| s.content.as_ref());
            // The marker should only be *dropped* (not swapped) when the
            // budget collapses, so when *either* row shows a marker, the
            // two rows must differ in the first cell.
            if first_open == Some("◇  ") {
                assert_eq!(first_gated, Some("◆  "), "gate marker differs");
            } else if first_gated == Some("◆  ") {
                assert_eq!(first_open, Some("◇  "), "gate marker differs");
            }
        }
    }

    /// A CJK id must be measured in *cells* (each fullwidth char = 2, not
    /// 1), never split mid-char, and never push the row past the pane. The
    /// id is the identity: it stays whole whenever it fits the budget, and
    /// only the *path* truncates (the new priority). We branch on whether
    /// the id fits rather than assuming the row always nearly fills.
    #[test]
    fn browse_row_is_width_aware_for_cjk() {
        // "note:" (5 cells) + 30 x 量 (60 cells) = 65 cells of id; "cjk.md"
        // = 6 cells of path.
        let id = "note:".to_string() + &"量".repeat(30);
        let id_cells = id.width(); // 65
        let rel = "cjk.md";
        let rel_cells = rel.width(); // 6
        for w in [10usize, 20, 30, 40, 60, 70, 80, 100] {
            let out = browse_row_text(&row(&id, rel), w);
            let got = out.width();
            assert!(got <= w, "row ({out:?}) = {got} cells must be <= {w}");
            if id_cells > w {
                // id doesn't fit at all → id (and only the id) truncates.
                assert!(out.contains('…'), "a cut id carries a marker: {out:?}");
                assert!(!out.contains(rel), "no path when the id doesn't fit: {out:?}");
            } else {
                // id fits → shown whole; only the *path* may truncate.
                assert!(out.contains(&id), "fitting id stays whole (CJK, cell-aware): {out:?}");
                let path_budget = w - id_cells;
                // Mirror browse_row's priority: spend marker (1) + separator (1)
                // if it fits and still leaves at least 1 cell for the path
                // itself; otherwise spend only the separator if it still fits.
                let mb = if path_budget.saturating_sub(4) > 0 {
                    path_budget - 4
                } else {
                    path_budget.saturating_sub(1)
                };
                if rel_cells <= mb {
                    assert!(out.contains(rel), "path whole when it fits: {out:?}");
                    assert!(!out.contains('…'), "no ellipsis when path fits: {out:?}");
                } else {
                    assert!(out.contains('…'), "path truncates; id must not: {out:?}");
                }
            }
        }
    }

    /// `ellipsize_cells` is total + bounded: short input passes through
    /// unchanged; long input is capped to at most `max` cells with a `…`,
    /// and (when truncating) always ends in the marker.
    #[test]
    fn ellipsize_cells_is_bounded_and_total() {
        assert_eq!(ellipsize_cells("abc", 5), "abc", "short input unchanged");
        assert_eq!(ellipsize_cells("abc", 3), "abc", "exact fit unchanged");
        assert_eq!(ellipsize_cells("abc", 0), "", "zero width → empty");
        assert_eq!(ellipsize_cells("", 7), "", "empty input → empty");

        let out = ellipsize_cells(&"x".repeat(50), 10);
        assert!(cells(&out) <= 10, "bounded by max: {out:?}");
        assert!(out.ends_with('…'), "truncate carries a marker: {out:?}");

        // 10 CJK chars = 20 cells; a 10-cell budget keeps some full chars +
        // the marker, never a half char, and never exceeds the budget.
        let cjk = "量".repeat(10);
        let out2 = ellipsize_cells(&cjk, 10);
        assert!(cells(&out2) <= 10, "CJK bounded by max: {out2:?}");
        assert!(out2.ends_with('…'));
        let body = out2.strip_suffix('…').unwrap();
        assert!(body.chars().all(|c| c == '量'), "no half-width fragment: {body:?}");
    }

    /// The rel_path-only fallback: if even the locator alone overflows the
    /// pane, the row still shows the locator (marked) — never empty for a
    /// positive width. (With the gate marker, the "pane" is now smaller
    /// than `width` by the 3-cell marker + 1 separator = 4 fixed cells.)
    #[test]
    fn browse_row_falls_back_to_rel_path_when_it_overflows() {
        let id = "note:short";
        let rel = format!("/{}", "a-very-long-absolute-ish-relative-path".repeat(4));
        let out = browse_row_text(&row(id, &rel), 20);
        assert!(!out.is_empty(), "positive width never yields an empty row");
        assert!(cells(&out) <= 20, "fallback still bounded: {out:?}");
        assert!(out.contains('…'), "an overflowed locator is still marked: {out:?}");
    }

    /// **Single-legend contract (the #1 fix)**: the Browse pane's title is
    /// identity + count **only** — it must NOT contain the keybinding hints
    /// (`↑`/`/`/`Enter`/`q quit`). The legend lives in the status bar, so a
    /// keybinding never appears in two places at once (which is what clip-doubled
    /// on real-width terminals: `…e edit m…` in the title AND `…q q` in the bar).
    #[test]
    fn browse_pane_title_carries_no_keybinding_legend() {
        let empty = browse_pane_title(4, 4, "");
        assert_eq!(empty, " 4 page(s)", "no filter -> just the count");
        let filt = browse_pane_title(3, 11, "rust");
        assert_eq!(filt, " 3 of 11 match 'rust'", "with filter -> match summary");
        // Neither form may leak any of the legend's tokens:
        for legend in ["↑", "jk", "/ filter", "Enter open", "e edit", "q quit"] {
            assert!(!empty.contains(legend), "title {empty:?} leaked legend {legend:?}");
            assert!(!filt.contains(legend), "title {filt:?} leaked legend {legend:?}");
        }
    }

    /// **Outline line contract (the #2 fix)**: each heading row is a *single*
    /// `Line` (two spans: dim `L<n>` anchor + label title), indented by level,
    /// and never splits mid-word the way the old wrapping `Paragraph` did
    /// (`H2 section` → `sub` / `section`). An empty title gets `¶` in the label
    /// role. This is the visual precondition for M8.2's Enter-to-jump.
    #[test]
    fn outline_line_is_single_line_per_heading() {
        let p = Palette::default_palette();
        let h2 = outline_line(&p, 3, 2, "H2 section");
        assert_eq!(h2.spans.len(), 2, "anchor + title, one line");
        // indent (level-1)*2 spaces + "L" + line padded to 4 wide.
        assert_eq!(h2.spans[0].content.as_ref(), "  L3   ", "indent(1)=2sp + L3 anchor");
        assert_eq!(h2.spans[0].style, p.dim, "anchor styled dim");
        assert_eq!(h2.spans[1].content.as_ref(), "H2 section", "title intact — not wrapped");
        assert_eq!(h2.spans[1].style, p.label, "title styled as label");
        // Deeper level -> deeper indent, same shape.
        let h4 = outline_line(&p, 9, 4, "H4 detail");
        assert_eq!(h4.spans[0].content.as_ref(), "      L9   ", "indent(3)=6sp");
        assert_eq!(h4.spans[0].style, p.dim);
        // Empty title -> the ¶ glyph (the body's `read_line` convention).
        let blank = outline_line(&p, 1, 1, "");
        assert_eq!(blank.spans.len(), 2);
        assert_eq!(blank.spans[1].content.as_ref(), "¶", "empty title uses the paragraph glyph");
        assert_eq!(blank.spans[1].style, p.label);
    }

    /// The *severity glyph* is what makes a write-success distinguishable
    /// from a validation-failure even when the user's palette makes `error`
    /// and `success` the same color — a real-world recolor, not a bug.
    /// Pin the glyph (and its absence on `Info`) across *every* shipped
    /// palette, since the glyph is the only thing standing between "success"
    /// and "error" if the two roles collapse onto one color.
    #[test]
    fn status_bar_glyph_distincts_success_from_error() {
        for p in [
            Palette::default_palette(),
            Palette::colorblind(),
        ] {
            let err = build_status_line(&p, &crate::tui::StatusKind::Error, "field required: title");
            let ok = build_status_line(&p, &crate::tui::StatusKind::Success, "saved 2 fields");
            let info = build_status_line(&p, &crate::tui::StatusKind::Info, " 4 page(s)");
            let flat = |l: &Line<'static>| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>();
            assert!(flat(&err).starts_with("  ! "), "error carries the `!` glyph: {err:?}");
            assert!(flat(&ok).starts_with("  ✓ "), "success carries the `✓` glyph: {ok:?}");
            assert!(
                !flat(&info).trim_start().starts_with('!') && !flat(&info).trim_start().starts_with('✓'),
                "info has no severity glyph: {info:?}"
            );
            // Glyph + message both carry the severity role's style (the
            // palette is not split — the glyph takes on the message color).
            assert_eq!(err.spans[1].style, p.error, "error glyph is the `error` role");
            assert_eq!(err.spans[2].style, p.error, "error message is the `error` role");
            assert_eq!(ok.spans[1].style, p.success, "success glyph is the `success` role");
            assert_eq!(ok.spans[2].style, p.success, "success message is the `success` role");
            assert_eq!(info.spans.len(), 1, "info is a single span (no glyph split)");
            assert_eq!(info.spans[0].style, p.info, "info is the `info` role");
            // The glyph is the thing that makes "success" vs "error" survive
            // an equal-color recolor — assert the two are different strings.
            assert_ne!(flat(&err), flat(&ok), "error and success must never be identical");
        }
    }

    /// A helper for the find-patch tests: given a rendered `Line`, walk it
    /// left-to-right and return the `Style` of the byte at `byte` (into the
    /// rendered char stream). Returns `None` past the end (total).
    fn byte_style(line: &Line<'_>, byte: usize) -> Option<Style> {
        let mut acc = 0usize;
        for sp in line.spans.iter() {
            let w: usize = sp.content.chars().map(|c| c.len_utf8()).sum();
            if byte >= acc && byte < acc + w {
                return Some(sp.style);
            }
            acc += w;
        }
        None
    }

    /// `apply_find_patched` must color exactly the patched byte ranges with
    /// the requested role and leave every other byte in its base style. It
    /// must also let a *later* patch override an earlier one in any overlap
    /// (the "last in vec wins" contract). Offsets are into the *rendered*
    /// char stream (so `**bold**` contributes the styled text `bold`).
    #[test]
    fn apply_find_patched_colors_exactly_the_ranges() {
        let p = Palette::default_palette();
        // "a **bold** tail" renders to "a bold tail" (11 bytes), where
        // bytes 2..5 = "bold" carry md_bold and the rest carry `default`.
        let line = read_line(&p, "a **bold** tail");
        let flat0: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(flat0, "a bold tail", "rendered baseline: {flat0:?}");
        assert_eq!(byte_style(&line, 2), Some(p.md_bold), "baseline byte 2 is md_bold");

        // Patches: [1,2) = the space (default) → find_match; [2,5) = "bol"
        // (md_bold) → find_current must *override* the bold emphasis. Byte 5
        // = "d" is unpatched → keeps md_bold.
        let patched = apply_find_patched(line, &[(1, 2, p.find_match), (2, 5, p.find_current)]);
        let flat: String = patched.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(flat, "a bold tail", "text is preserved: {flat:?}");
        assert_eq!(byte_style(&patched, 0), Some(p.default), "byte 0 stays default");
        assert_eq!(byte_style(&patched, 1), Some(p.find_match), "byte 1 (space) → find_match");
        assert_eq!(byte_style(&patched, 2), Some(p.find_current), "byte 2 (b) overridden by find_current");
        assert_eq!(byte_style(&patched, 3), Some(p.find_current), "byte 3 (o) → find_current");
        assert_eq!(byte_style(&patched, 4), Some(p.find_current), "byte 4 (l) → find_current");
        assert_eq!(byte_style(&patched, 5), Some(p.md_bold), "byte 5 (d) unpatched keeps md_bold");
        assert_eq!(byte_style(&patched, 6), Some(p.default), "byte 6 (space) stays default");
        assert_eq!(byte_style(&patched, 10), Some(p.default), "byte 10 (l) stays default");

        // Overlap: a later patch wins over an earlier one on a shared byte.
        let patched2 = apply_find_patched(
            read_line(&p, "a **bold** tail"),
            &[(0, 5, p.find_match), (3, 7, p.find_current)],
        );
        assert_eq!(byte_style(&patched2, 3), Some(p.find_current), "later patch wins on shared byte 3");
        assert_eq!(byte_style(&patched2, 4), Some(p.find_current), "later patch wins on byte 4");
        assert_eq!(byte_style(&patched2, 0), Some(p.find_match), "earlier-only byte 0 → find_match");
    }

    /// An empty patch list is a byte-identity no-op (text unchanged; total).
    #[test]
    fn apply_find_patched_empty_patches_is_identity() {
        let p = Palette::default_palette();
        let original = read_line(&p, "hello **world**");
        let out = apply_find_patched(original, &[]);
        let a: String = out.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(a, "hello world", "rendered text (markdown emphasis stripped): {a:?}");
        // No-panic is the real contract (the function is total).
    }

    /// A patch range that straddles *two* rendered spans (a find highlight
    /// covering a boundary between `default` text and a `md_bold` token)
    /// must color *both* halves with the patch role, never leaking a base
    /// style through the middle of the range.
    #[test]
    fn apply_find_patched_span_straddling_range() {
        let p = Palette::default_palette();
        // "abc**def**ghi" renders to "abcdefghi" (9 bytes): 0..2 = "abc"
        // (default), 2..5 = "def" (md_bold), 5..9 = "ghi" (default).
        let line = read_line(&p, "abc**def**ghi");
        let flat0: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(flat0, "abcdefghi", "rendered baseline: {flat0:?}");
        assert_eq!(byte_style(&line, 2), Some(p.default), "byte 2 (c) default");
        assert_eq!(byte_style(&line, 3), Some(p.md_bold), "byte 3 (d) md_bold");

        // Patch [2:6) starts in the `default` span and ends in the
        // `md_bold` span (crosses the boundary at byte 3).
        let out = apply_find_patched(line, &[(2, 6, p.find_match)]);
        assert_eq!(byte_style(&out, 2), Some(p.find_match), "byte 2 in range → find_match");
        assert_eq!(byte_style(&out, 3), Some(p.find_match), "byte 3 in range → find_match (overrides md_bold)");
        assert_eq!(byte_style(&out, 4), Some(p.find_match), "byte 4 in range → find_match");
        assert_eq!(byte_style(&out, 5), Some(p.find_match), "byte 5 in range → find_match");
        assert_eq!(byte_style(&out, 1), Some(p.default), "byte 1 out of range stays default");
        assert_eq!(byte_style(&out, 6), Some(p.default), "byte 6 out of range stays default");
    }

    // ── M8.5 inline candidate-picker — row construction ────────────────
    // Same coverage convention as the `browse_row_*` tests: the row the
    // user reads is the load-bearing part of the picker; the ratatui
    // widget machinery (Layout, ListState, borders) is not. These tests
    // flatten the spans and assert the *text* a user would see, so a
    // regression that drops the `abs_path`, renumbers the candidate, or
    // swaps the order of (display, abs_path) is caught without a live
    // terminal.

    fn cand(display: &str, abs_path: &str) -> LinkCandOption {
        LinkCandOption {
            abs_path: abs_path.to_string(),
            display: display.to_string(),
            section: None,
        }
    }

    /// A candidate row renders as `<n>  <display>   (<abs_path>)` — the
    /// number is 1-based (right-aligned to 3 cells) and the `abs_path`
    /// is *always* present in parentheses (the navigation identity the
    /// user needs when two pages share a display id, e.g. `note:A` for
    /// two different bodies).
    #[test]
    fn candidate_row_renders_display_and_abs_path() {
        let o1 = cand("note:A", "/root/a1.md");
        let o2 = cand("note:A", "/root/a2.md"); // same display id, *different* path
        assert_eq!(
            candidate_row_text(0, &o1, false),
            "  1  note:A   (/root/a1.md)",
            "first option: 1-based number, display, then abs_path; got"
        );
        assert_eq!(
            candidate_row_text(1, &o2, false),
            "  2  note:A   (/root/a2.md)",
            "second option: same display id, *different* abs_path — the user can tell them apart"
        );
    }

    /// The 1-based number column is *stable-width* for a reasonable
    /// candidate count (≤ 999): the right-aligned to-3 rule means
    /// option 1 renders as `"  1  "`, option 12 as `" 12  "`, option 999
    /// as `"999  "` — one cell of visual drift per digit, never a full
    /// column shift. (The `resolver` dedups by `abs_path`, so a real
    /// corpus has at most two or three candidates, but the picker stays
    /// total over a pathological one.)
    #[test]
    fn candidate_row_number_is_stable_width() {
        let o = cand("note:A", "/a.md");
        assert_eq!(candidate_row_text(0, &o, false), "  1  note:A   (/a.md)");
        assert_eq!(candidate_row_text(1, &o, false), "  2  note:A   (/a.md)");
        assert_eq!(candidate_row_text(9, &o, false), " 10  note:A   (/a.md)");
        assert_eq!(candidate_row_text(99, &o, false), "100  note:A   (/a.md)");
        assert_eq!(candidate_row_text(998, &o, false), "999  note:A   (/a.md)");
    }

    /// The `abs_path` is *always* shown, even when it is *long* — it is
    /// the stable identity `open_linked` consumes and the *only* thing a
    /// user can key off to tell two same-named pages apart. (The flat
    /// list's `browse_row` truncates the path for width; the candidate
    /// row does not — the candidate set is ≤3 rows, so a long path is
    /// better than a truncated one.)
    #[test]
    fn candidate_row_keeps_the_abs_path_whole_when_it_is_long() {
        let long = "a-very-long-absolute-path/with-many-segments/to/a/page.md";
        let o = cand("note:A", long);
        let text = candidate_row_text(0, &o, false);
        assert!(
            text.contains(&format!("({})", long)),
            "the full abs_path must be present in its parens even when long; got {text:?}"
        );
        assert_eq!(
            text,
            format!("  1  note:A   ({long})"),
            "display + full path, one row"
        );
    }

    /// Highlighted vs. unselected rows *render the same text* (the
    /// palette role — `active + BOLD` for selected, `default` for not —
    /// is a styling choice the palette owns, not the row's content).
    /// This pins that the *row content* is stable across the cursor
    /// position: moving the cursor (↑/↓) changes the *style*, not the
    /// *text* the user reads.
    #[test]
    fn candidate_row_text_is_stable_across_selection() {
        let o = cand("note:A", "/a.md");
        let on = candidate_row_text(0, &o, true);
        let off = candidate_row_text(0, &o, false);
        assert_eq!(on, off, "the same option renders the same text whether or not it is highlighted");
    }

    // ---- T2: `raw_to_rendered_span` (find-style highlight helper) --------

    /// The needle is present → the span is its byte range (start, start+len).
    #[test]
    fn span_finds_first_occurrence() {
        let line = "see [[transformer]] for how it works";
        let (s, e) = raw_to_rendered_span(line, "transformer");
        assert_eq!(&line[s..e], "transformer");
        assert_eq!(e, s + "transformer".len());
    }

    /// No needle (e.g. label form where the label is not in the line) → `(0, 0)`
    /// so the caller skips the highlight rather than painting the whole line.
    #[test]
    fn span_missing_needle_is_zero_span() {
        let line = "plain prose with no link";
        assert_eq!(raw_to_rendered_span(line, "transformer"), (0, 0));
    }

    /// Empty needle → `(0, 0)` (never paints an empty span).
    #[test]
    fn span_empty_needle_is_zero_span() {
        assert_eq!(raw_to_rendered_span("any text here", ""), (0, 0));
    }

    /// Whitespace / multi-word needles match verbatim when present in the line.
    #[test]
    fn span_whole_phrase_matches_verbatim() {
        let line = "a watchmaker keeps a [[watchmaking guide]] on the shelf";
        let (s, e) = raw_to_rendered_span(line, "watchmaking guide");
        assert_eq!(&line[s..e], "watchmaking guide");
    }

    // ---- T2: `splice_links_on_line` (display-text splicing) -------------
    // `col..end` is the *whole written reference* (brackets/parens included)
    // exactly as `detect_refs` reports it; the display text is what stays.

    /// The inline form `[label](target)` splices the full reference down to
    /// just the label — the prose reads as if the link were plain text.
    #[test]
    fn splice_inlink_keeps_label() {
        let raw = "see [transformers explained](notes/transformer.md) for details";
        let ranges = [(4, 50, "transformers explained".to_string())];
        assert_eq!(
            splice_links_on_line(raw, &ranges),
            "see transformers explained for details"
        );
    }

    /// A wikilink `[[target]]` splices to just the target text (no label).
    #[test]
    fn splice_wikilink_keeps_target() {
        let raw = "read the [[watchmaking guide]] carefully";
        let ranges = [(9, 30, "watchmaking guide".to_string())];
        assert_eq!(
            splice_links_on_line(raw, &ranges),
            "read the watchmaking guide carefully"
        );
    }

    /// An external `< uri >` splices to just the URI.
    #[test]
    fn splice_external_keeps_uri() {
        let raw = "open <https://example.com/x> soon";
        let ranges = [(5, 28, "https://example.com/x".to_string())];
        assert_eq!(
            splice_links_on_line(raw, &ranges),
            "open https://example.com/x soon"
        );
    }

    /// Two links on one line both splice; offsets stay correct because the
    /// splices run high-to-low (later link first), so earlier ranges are
    /// never invalidated.
    #[test]
    fn splice_two_links_on_one_line() {
        let raw = "a [alpha](a.md) then [beta](b.md) end";
        let ranges = vec![
            (2, 15, "alpha".to_string()),
            (21, 33, "beta".to_string()),
        ];
        assert_eq!(
            splice_links_on_line(raw, &ranges),
            "a alpha then beta end"
        );
    }

    /// Surrounding prose — including markdown emphasis delimiters — is
    /// preserved byte-for-byte around the spliced span.
    #[test]
    fn splice_preserves_prose() {
        let raw = "**emph** before  see **[x](y.md)**  after";
        let ranges = [(23, 32, "x".to_string())];
        assert_eq!(
            splice_links_on_line(raw, &ranges),
            "**emph** before  see **x**  after"
        );
    }

    /// Empty ranges is the identity (the caller's fast path).
    #[test]
    fn splice_empty_is_identity() {
        let raw = "no links here";
        assert_eq!(splice_links_on_line(raw, &[]), raw);
    }

    // ---- T2 v2: state-sensitive helpers (`link_state`, `splice_body`,
    // `line_patches`) — the shared Read / Links-view primitives -------------

    /// Build a minimal `LinksEntry` for state tests. Fields not under
    /// inspection are neutral; `resolved` / `candidates` are the two
    /// discriminators `link_state` reads.
    ///
    /// `raw_text` is derived from the label/target pair to mirror the
    /// canonical forms: wikilink `[[target]]` when `label` is `None`,
    /// inline `[label](target)` otherwise.
    fn entry(
        kind: kb_core::links::LinkKind,
        target: &str,
        label: Option<&str>,
        resolved: Option<&str>,
        candidates: &[&str],
        col: usize,
        end: usize,
    ) -> LinksEntry {
        let raw_text = match label {
            None => format!("[[{target}]]"),
            Some(l) => format!("[{l}]({target})"),
        };
        LinksEntry {
            line: 0,
            col,
            end,
            context: String::new(),
            target: target.to_string(),
            raw_text,
            label: label.map(|s| s.to_string()),
            section: None,
            kind,
            resolved: resolved.map(|s| s.to_string()),
            candidates: candidates.iter().map(|s| s.to_string()).collect(),
            abs_path: resolved.map(|s| format!("{s}.md")),
            current_resolved: false,
        }
    }

    /// A Resolved page link — the one state that earns `🔗`.
    #[test]
    fn link_state_resolved_page() {
        let e = entry(
            kb_core::links::LinkKind::Db,
            "watchmaking",
            Some("the watchmaking guide"),
            Some("note:watchmaking"),
            &[],
            9,
            22,
        );
        assert_eq!(link_state(&e), LinkState::Resolved);
    }

    /// External links are always Resolved (the browser destination is
    /// self-evident — they earn `↗`, not `🔗`). A `Current` link is Resolved
    /// only when the referenced section is present in the page being read;
    /// a missing section is Dead (nothing to navigate to).
    #[test]
    fn link_state_external_and_current_are_resolved() {
        let e = entry(kb_core::links::LinkKind::External, "x", None, None, &[], 0, 1);
        assert_eq!(link_state(&e), LinkState::Resolved, "External should be Resolved");
        let cur_alive = {
            let mut e = entry(kb_core::links::LinkKind::Current, "#intro", None, None, &[], 0, 1);
            e.current_resolved = true;
            e
        };
        assert_eq!(link_state(&cur_alive), LinkState::Resolved, "Current w/ present section should be Resolved");
        let cur_dead = entry(kb_core::links::LinkKind::Current, "#nope", None, None, &[], 0, 1);
        assert_eq!(link_state(&cur_dead), LinkState::Dead, "Current w/ missing section should be Dead");
    }

    /// Db / Path with no resolution and no candidates → Dead.
    #[test]
    fn link_state_dead() {
        let e = entry(kb_core::links::LinkKind::Db, "missing", None, None, &[], 0, 9);
        assert_eq!(link_state(&e), LinkState::Dead);
    }

    /// Db / Path with no resolution but a candidate set → Ambiguous.
    #[test]
    fn link_state_ambiguous() {
        let e = entry(
            kb_core::links::LinkKind::Path,
            "guide",
            None,
            None,
            &["a.md", "b.md"],
            0,
            5,
        );
        assert_eq!(link_state(&e), LinkState::Ambiguous);
    }

    /// A Resolved *page* link splices to `label 🔗` — the marker only appears
    /// on resolved page links (the navigable-page affordance).
    #[test]
    fn splice_body_resolved_page_earns_marker() {
        let body = "read the [watching](guide.md)";
        // `[watching](guide.md)` starts at col 9, ends at col 29 (exclusive).
        let links = vec![entry(
            kb_core::links::LinkKind::Path,
            "guide.md",
            Some("watching"),
            Some("note:watching"),
            &[],
            9,
            29,
        )];
        let spliced = splice_body(body, &links);
        assert_eq!(spliced, vec!["read the watching 🔗"]);
    }

    /// External splice to their display text but earn `↗` (the browser
    /// affordance) — NOT `🔗` (that marker is reserved for kb-page
    /// destinations).
    #[test]
    fn splice_body_external_no_marker() {
        let body = "see <https://example.com/x>";
        let links = vec![entry(
            kb_core::links::LinkKind::External,
            "https://example.com/x",
            None,
            None,
            &[],
            4,
            27,
        )];
        let spliced = splice_body(body, &links);
        assert_eq!(spliced, vec!["see https://example.com/x ↗"]);
        assert!(!spliced[0].contains("🔗"), "external must not earn 🔗");
        assert!(spliced[0].ends_with('↗'), "external must earn ↗");
    }

    /// A `Current` link to a section *present* in the page being read splices
    /// to its display text and earns `🔗` — the navigable-section affordance.
    /// The canonical Current form is `[ # section ]` (label `None`, target
    /// `# section`); after splicing the reader sees `# section 🔗`.
    #[test]
    fn splice_body_current_resolved_earns_marker() {
        let body = "see [ # overview ] for the overview";
        // `[ # overview ]` spans cols 4..18 (14 chars, exclusive end).
        let mut links = vec![entry(
            kb_core::links::LinkKind::Current,
            "# overview",
            None,
            None,
            &[],
            4,
            18,
        )];
        links[0].current_resolved = true;
        let spliced = splice_body(body, &links);
        assert_eq!(spliced, vec!["see # overview 🔗 for the overview"]);
    }

    /// A `Current` link to a *missing* section is Dead → left as written
    /// (target preserved, no marker); the author's markdown emphasis around
    /// the bracket form is the only state cue.
    #[test]
    fn splice_body_current_dead_left_as_written() {
        let body = "see [ # overview ] for the overview";
        let links = vec![entry(
            kb_core::links::LinkKind::Current,
            "# overview",
            None,
            None,
            &[],
            4,
            18,
        )];
        let spliced = splice_body(body, &links);
        assert_eq!(spliced, vec!["see [ # overview ] for the overview"]);
        assert!(!spliced[0].contains('🔗'), "current w/ missing section must not earn 🔗");
        assert!(spliced[0].contains("[ # overview ]"), "as-written brackets intact");
    }

    /// Dead and Ambiguous links are left **as written** (the full
    /// `[[target]]` reference, brackets included) — no splice applies, the
    /// prose reads exactly as the user typed it. The only *state* cue is
    /// the Links-view focused box (via `find_current`) — carried by
    /// `line_patches`, never by prose style.
    #[test]
    fn splice_body_dead_and_ambiguous_left_as_written() {
        let body = "see [[missing]] and [[guide]] here";
        let links = vec![
            entry(
                kb_core::links::LinkKind::Db,
                "missing",
                None,
                None,
                &[],
                4,
                15,
            ),
            entry(
                kb_core::links::LinkKind::Db,
                "guide",
                None,
                None,
                &["a.md", "b.md"],
                20,
                29,
            ),
        ];
        // Both stay as written (the full `[[…]]` span). No `🔗` because
        // neither resolves to a kb page.
        let spliced = splice_body(body, &links);
        assert_eq!(spliced, vec![body]);
        assert!(!spliced[0].contains('🔗'), "dead/ambiguous must not earn the marker");
        assert!(spliced[0].contains("[[missing]]"), "dead link's brackets are intact");
        assert!(spliced[0].contains("[[guide]]"), "ambiguous link's brackets are intact");
    }

    /// A `**`-emphasis line is spliced for a resolved link while the
    /// surrounding emphasis delimiters are preserved — `rendered_line` then
    /// strips the `**` and `line_patches` must land on the *stripped* text.
    #[test]
    fn splice_body_preserves_emphasis_delimiters() {
        let body = "see **[x](y.md)** for the rest";
        // "see " (0-3) + "**" (4-5), then `[x](y.md)` spans cols 6..15
        // (6 = `[`, 14 = `)`, exclusive end = 15). `**` closers are 15-16.
        let links = vec![entry(
            kb_core::links::LinkKind::Path,
            "y.md",
            Some("x"),
            Some("note:y"),
            &[],
            6,
            15,
        )];
        let spliced = splice_body(body, &links);
        assert_eq!(spliced, vec!["see **x 🔗** for the rest"]);
    }

    /// `line_patches` paints the resolved link in the `link` role, and
    /// *skips* non-focused Dead / Ambiguous entirely — a `p.default` overlay
    /// would flatten whatever markdown emphasis the author chose to wrap the
    /// dead link in. Only when the dead / ambiguous link is focused (Links
    /// view) does a patch land (in `find_current`).
    #[test]
    fn line_patches_three_states_distinct_roles() {
        let p = Palette::default_palette();
        // One line carrying a dead + an ambiguous + a resolved link.
        // Offsets span each **full reference** (for a wikilink `[[x]]` the
        // span is the byte range of those 4+ chars; for an inline form
        // `[x](y)` the span is the byte range of that whole form) since
        // `splice_body` replaces `raw[col..end]` with the display text.
        let body = "see [[missing]] [[guide]] and [live](live.md) now";
        let links = vec![
            entry(
                kb_core::links::LinkKind::Db,
                "missing",
                None,
                None,
                &[],
                4,
                15,
            ),
            entry(
                kb_core::links::LinkKind::Db,
                "guide",
                None,
                None,
                &["a.md", "b.md"],
                16,
                25,
            ),
            entry(
                kb_core::links::LinkKind::Path,
                "live.md",
                Some("live"),
                Some("note:live"),
                &[],
                30,
                45,
            ),
        ];
        let rendered = rendered_line(&p, body);
        let patches = line_patches(&p, &rendered, &links, 0, None);
        let by_needle = |needle: &str| -> Option<(usize, usize, Style)> {
            patches
                .iter()
                .find(|(s, e, _)| *s < *e && &rendered[*s..*e] == needle)
                .copied()
        };
        // Dead / Ambiguous are *skipped* (non-focused) so the author's
        // markdown emphasis carries through untouched.
        assert!(by_needle("[[missing]]").is_none(), "non-focused dead link must be skipped (no patch)");
        assert!(by_needle("[[guide]]").is_none(), "non-focused ambiguous link must be skipped (no patch)");
        let live = by_needle("live").expect("resolved display text present");
        assert_eq!(live.2, p.link, "resolved (non-focused) → link role");
    }

    /// The focused link (Links view) is boxed in `find_current` — even for a
    /// *dead* link (the "no highlight on a dead link" fix). Non-focused
    /// dead / ambiguous links on the same line are *skipped* entirely (no
    /// patch) so the author's markdown emphasis around them survives.
    #[test]
    fn line_patches_focused_dead_upgrades_to_find_current() {
        let p = Palette::default_palette();
        let body = "see [[missing]] and [[guide]]";
        let missing = entry(
            kb_core::links::LinkKind::Db,
            "missing",
            None,
            None,
            &[],
            4,
            13,
        );
        let guide = entry(
            kb_core::links::LinkKind::Db,
            "guide",
            None,
            None,
            &["a.md", "b.md"],
            18,
            26,
        );
        let links = vec![missing.clone(), guide.clone()];
        let rendered = rendered_line(&p, body);
        let key = (missing.line, missing.col);
        let patches = line_patches(&p, &rendered, &links, 0, Some(key));
        let find_patch = |n: &str| patches.iter().find(|(s, e, _)| s < e && &rendered[*s..*e] == n).copied();
        let focused = find_patch("[[missing]]").expect("focused dead as-written span present");
        assert_eq!(focused.2, p.find_current, "focused dead → find_current (boxed, as written)");
        assert!(find_patch("[[guide]]").is_none(), "unfocused ambiguous → skipped (no patch; author emphasis survives)");
    }

    /// A dead link the author wrapped in markdown emphasis must KEEP that
    /// emphasis after the render pass — `line_patches` skips non-focused
    /// Dead/Ambiguous entirely (no `p.default` overlay), so
    /// `render_markdown_text`'s `md_bold` / `md_italic` / `md_strike` /
    /// `md_code` style survives verbatim. Focused dead links still get
    /// `find_current` (the Links view's box).
    #[test]
    fn line_patches_dead_link_keeps_author_emphasis() {
        let p = Palette::default_palette();
        let body = "see **[[missing]]** here";
        // `**[[missing]]**` — the markdown pair wraps cols 4..18 (14 chars);
        // the *link* `[[missing]]` itself is cols 6..16 (brackets included).
        let dead = entry(
            kb_core::links::LinkKind::Db,
            "missing",
            None,
            None,
            &[],
            6,
            16,
        );
        let links = vec![dead.clone()];
        let rendered = rendered_line(&p, body);
        // `render_markdown_text` strips the `**` pair and applies `md_bold`
        // to the inner text; the rendered line is the 11-char
        // `[[missing]]` (bold-styled) flanked by plain prose.
        assert_eq!(rendered, "see [[missing]] here");
        // Non-focused: `line_patches` must skip the dead link (no patch);
        // the `md_bold` style from `render_markdown_text` carries through.
        let patches = line_patches(&p, &rendered, &links, 0, None);
        assert!(
            patches.is_empty(),
            "non-focused dead link must be skipped (emphasis carries through)"
        );
        // Focused: `line_patches` paints the full as-written span in
        // `find_current` (the Links-view box).
        let focused_key = (dead.line, dead.col);
        let patches_focused = line_patches(&p, &rendered, &links, 0, Some(focused_key));
        assert_eq!(patches_focused.len(), 1);
        assert_eq!(patches_focused[0].2, p.find_current);
    }

    /// ATX headings render wholly in the heading role; links on a heading
    /// line are exempt from the link-state patches (returning empty).
    #[test]
    fn line_patches_heading_is_exempt() {
        let p = Palette::default_palette();
        let body = "# Heading with [[missing]]";
        let links = vec![entry(
            kb_core::links::LinkKind::Db,
            "missing",
            None,
            None,
            &[],
            13,
            22,
        )];
        let rendered = rendered_line(&p, body);
        assert!(
            line_patches(&p, &rendered, &links, 0, None).is_empty(),
            "a heading line yields no link patches"
        );
    }
}
