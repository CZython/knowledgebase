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
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List as RatList, ListItem, ListState, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::{App, Mode, PageRow, ReadState, StatusKind};
use crate::tui::palette::Palette;

/// Draw the whole screen for `app`. Called once per event-loop tick.
pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    match &app.mode {
        Mode::Browse => draw_browse(f, app, area),
        Mode::Read(rs) => draw_read(f, app, rs, area),
        Mode::Edit(es) => draw_edit(f, app, es, area),
        Mode::Alias(ast) => draw_alias(f, app, ast, area),
        Mode::Roots(rs) => draw_roots(f, app, rs, area),
        Mode::Search(ss) => draw_search(f, app, ss, area),
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

    let header = Paragraph::new(format!("  kb tui — reading {} (row {} of {})", rs.id, rs.row_idx + 1, app.rows.len()))
        .style(app.palette.label);
    f.render_widget(header, chunks[0]);

    // The panes row: body (~70%) | outline (~30%, or 0 if the page has no
    // headings).
    let has_outline = !rs.outline.is_empty();
    // 60/40 (was 70/30): the outline pane needs enough width that `L<n>` +
    // indent + title sit on one line at common terminal widths; too narrow
    // and a `Paragraph` wraps mid-word (see the H2→"sub/section" break).
    let pane_constraints = if has_outline {
        [Constraint::Percentage(60), Constraint::Percentage(40)]
    } else {
        [Constraint::Percentage(100), Constraint::Length(0)]
    };
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(pane_constraints)
        .split(chunks[1]);

    // Body pane: render a window from `scroll_top`, clipped to the pane
    // height. We take up to `pane_height` lines — the `Paragraph` widget
    // already scrolls past the end if there are more, but we want the
    // *viewport* to be exactly the pane height, so the scroll stays in
    // sync with what's on screen.
    //
    // P7: ATX headings (`#` through `######`) render in the `p.label` role;
    // every other line stays in the default `p.default` role. The detection
    // is cheap (prefix check on up to 7 leading `#` + a space) and runs on
    // *every* windowed line, so it stays in lockstep with the scroll.
    let p = &app.palette;
    let body_lines: Vec<Line<'static>> = rs
        .body
        .lines()
        .skip(rs.scroll_top)
        .take(body_window_height(panes[0].height))
        .map(|l| read_line(p, l))
        .collect();
    let body_widget = Paragraph::new(body_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" body — line {} of {} (jk/PgUp/Dn, g/G, Esc back)",
                    rs.scroll_top + 1,
                    rs.body.lines().count())),
        )
        .wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(body_widget, panes[0]);

    if has_outline {
        // A `List`, not a `Paragraph`: each heading stays on **one row** and
        // never wraps mid-word (the old `Paragraph` + `Wrap{trim:false}` broke
        // `L3  H2 section` into two visible lines in narrow panes). Rows are
        // indented per heading level; the `L<n>` anchor is dimmed (the title
        // is the focus) and `¶` marks an empty-title heading (same convention
        // as the body's `read_line`). Palette-driven: `p.dim` for the anchor,
        // `p.label` for the title, `p.label` for the empty-title glyph, and
        // `p.selection` for the highlight (so the pane is visually ready for
        // M8.2's Enter-to-jump — today there's no state, so no row stands out).
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
                .title(format!(" outline — {} section(s)  (M8.2: Enter will jump)", rs.outline.len())),
        );
        // No `ListState` (yet) — no highlight. When M8.2 lands, swap to
        // `render_stateful_widget` + an `outline_cursor` field on `ReadState`.
        f.render_widget(outline_widget, panes[1]);
    }

    render_status_bar(f, app, chunks[2]);
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

/// The number of body lines to render in the viewport. The pane height is
/// already in "lines" (minus borders), so this is a direct pass-through —
/// the scroll offset `scroll_top` (in line counts) plus this window gives the
/// visible line range. Capped to ≥ 0 (a zero-height pane renders
/// nothing; the scroll mutators are no-ops).
fn body_window_height(h: u16) -> usize {
    // Subtract 2 for the top+bottom borders (the body title lives in the
    // top border, so it's already accounted for by ratatui's layout).
    h.saturating_sub(2) as usize
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
    let is_heading = || {
        let cs = raw.as_bytes();
        if cs.is_empty() || cs[0] != b'#' {
            return false;
        }
        // Up to 6 `#`s, then *exactly* one space (or end-of-line).
        let mut i = 0;
        while i < cs.len() && cs[i] == b'#' {
            i += 1;
        }
        (1..=6).contains(&i) && i < cs.len() && cs[i] == b' '
    };
    if is_heading() {
        Line::from(vec![Span::styled(raw.to_string(), p.label)])
    } else {
        Line::from(Span::styled(raw.to_string(), p.default))
    }
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
/// `rel_path` (the locator) is kept whole, and the `id` ellipsizes to what
/// fits; total + width-bounded. If the pane is too narrow to fit the 4-cell
/// marker overhead, the marker drops and the row degrades to the old
/// `<id> <rel_path>` shape so the row still renders.
fn browse_row(p: &Palette, row: &PageRow, width: usize) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let (marker, mstyle): (&str, Style) = if row.gates.is_empty() {
        ("◇  ", p.open)
    } else {
        ("◆  ", p.gated)
    };
    let rel_w = row.rel_path.width();
    let has_rel = rel_w > 0;
    // rel_path is the locator: reserve its width and the 1-space separator,
    // then fit the id into what remains.
    let old_id_budget = if width > rel_w + 1 {
        width - rel_w - 1
    } else {
        0
    };
    // The gate marker is a nicety, not a requirement: it is shown only when
    // the id still fits (no ellipsis) inside the narrower budget. Otherwise
    // we degrade to the pre-P6 shape so a narrow pane never loses the id.
    let with_marker = {
        let ib = if width > rel_w + 1 + 3 {
            width - rel_w - 1 - 3
        } else {
            0
        };
        ib >= 1 && ellipsize_cells(&row.id, ib) == row.id
    };
    let mut out = Vec::new();
    if with_marker {
        out.push(Span::styled(marker, mstyle));
    }
    let id = ellipsize_cells(&row.id, old_id_budget);
    if !id.is_empty() {
        out.push(Span::styled(id, p.default));
        if has_rel {
            out.push(Span::raw(" "));
        }
    }
    if has_rel {
        out.push(Span::styled(
            if rel_w > width {
                ellipsize_cells(&row.rel_path, width)
            } else {
                row.rel_path.clone()
            },
            p.dim,
        ));
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

    /// The whole point of the fix: a row must **never overflow** its pane
    /// (that was what pushed `rel_path` off the right edge with no marker),
    /// and the **locator must stay visible**. "Fills exactly" is *not* a
    /// real guarantee — a CJK cut can leave a 1-cell gap that ratatui pads
    /// with a trailing space — so we assert `<= width`, never `== width`.
    /// The `browse_row` is now a span list (P6: the row's `◆`/`◇` marker
    /// needs a palette role). The tests compare the *flattened* text via
    /// `browse_row_text`, so they keep their existing shape.
    #[test]
    fn browse_row_never_overflows_and_keeps_rel_path() {
        let long_id = "note:Realistic Document Title with a lot of words";
        for w in [1usize, 5, 20, 38, 40, 50, 78, 100, 200] {
            let out = browse_row_text(&row(long_id, "real60.md"), w);
            let got = cells(&out);
            assert!(got <= w, "row ({out:?}) is {got} cells, must be <= {w}");
            // The 3-cell gate marker is *part* of the row now; the test's
            // old expectations still hold because they only look at the
            // suffix (the rel_path) and the width bound.
            assert!(
                out.ends_with("real60.md") || cells(&out) < cells("real60.md"),
                "locator (rel_path) never pushed off the edge: {out:?}"
            );
        }
    }

    /// When the id alone is longer than the whole pane, the row still
    /// keeps the locator and marks the drop with `…` — never a bare clip.
    #[test]
    fn browse_row_ellipsizes_the_id_not_the_locator() {
        let id = "note:".to_string() + &"a".repeat(120);
        let out = browse_row_text(&row(&id, "dir/a/b/c.md"), 40);
        assert!(cells(&out) <= 40);
        assert!(out.ends_with("dir/a/b/c.md"), "locator wins over the id: {out:?}");
        assert!(out.contains('…'), "the cut id must carry an ellipsis: {out:?}");
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
    /// 1), never split mid-char, and never push the row past the pane.
    /// Whether it's cut depends on the width: if the id fits, it's shown
    /// whole (no marker); if it must shrink, a `…` marks the drop and the
    /// loss is at most the one cell a 2-wide char can leave. We branch on
    /// which case applies rather than assuming the row always nearly fills.
    #[test]
    fn browse_row_is_width_aware_for_cjk() {
        // "note:" (5 cells) + 30 x 量 (60 cells) = 65 cells of id; "cjk.md"
        // = 6 cells of locator.
        let id = "note:".to_string() + &"量".repeat(30);
        let id_cells = id.width(); // 65
        let rel = "cjk.md";
        let rel_cells = rel.width(); // 6
        for w in [10usize, 20, 30, 40, 70, 80, 100] {
            let out = browse_row_text(&row(&id, rel), w);
            let got = out.width();
            assert!(got <= w, "row ({out:?}) = {got} cells must be <= {w}");
            assert!(out.ends_with(rel), "locator stays visible: {out:?}");
            let budget = w.saturating_sub(rel_cells + 4); // marker(3) + 1 sep
            if id_cells <= budget {
                // The id fits: shown whole, no marker, exact width.
                assert!(
                    !out.contains('…'),
                    "a fitting id must not carry a marker: {out:?}"
                );
                assert!(
                    got == id_cells + 4 + rel_cells,
                    "fitting id fills exactly (marker + sep + id + rel): {got}",
                );
            } else {
                // The id is cut: marked, and the loss is <= one cell (a 2-wide
                // char can straddle the budget boundary — no half-char).
                assert!(out.contains('…'), "a cut id must carry a marker: {out:?}");
                assert!(
                    got >= w.saturating_sub(1),
                    "no half-char truncation: {out:?}"
                );
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
}
