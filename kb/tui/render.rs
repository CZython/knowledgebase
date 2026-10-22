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
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, List as RatList, ListItem, ListState, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::{App, Mode, PageRow, ReadState, StatusKind};

/// Draw the whole screen for `app`. Called once per event-loop tick.
pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    match &app.mode {
        Mode::Browse => draw_browse(f, app, area),
        Mode::Read(rs) => draw_read(f, app, rs, area),
        Mode::Edit(es) => draw_edit(f, app, es, area),
        Mode::Alias(ast) => draw_alias(f, app, ast, area),
        Mode::Roots(rs) => draw_roots(f, app, rs, area),
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
        .style(Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
    f.render_widget(header_widget, chunks[0]);

    // Each row is `<id>  <rel_path>` fitted to the pane's inner width (the
    // ALL-side borders eat one cell per side) so the locator stays visible
    // and the id ellipsizes to what fits — no clipping-without-marker.
    let row_width = chunks[1].width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .view
        .iter()
        .map(|r| ListItem::new(browse_row(r, row_width)))
        .collect();

    let title = if app.filter_text.is_empty() {
        let hint = if cfg!(feature = "editor") {
            String::from(" e edit metadata · v $EDITOR · q quit")
        } else {
            String::from(" e edit metadata · q quit")
        };
        format!(" {} page(s) — ↑↓/jk move · / filter · Enter open · a alias · r roots ·{}", app.view.len(), hint)
    } else {
        format!(" {} page(s) of {} match '{}' — / to edit filter · Esc to clear",
                app.view.len(), app.rows.len(), app.filter_text)
    };
    // White-on-black (inverted default) so the selected row stands out
    // regardless of the terminal's own fg/bg colors.
    let widget = RatList::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::White));

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
        .style(Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
    f.render_widget(header, chunks[0]);

    // The panes row: body (~70%) | outline (~30%, or 0 if the page has no
    // headings).
    let has_outline = !rs.outline.is_empty();
    let pane_constraints = if has_outline {
        [Constraint::Percentage(70), Constraint::Percentage(30)]
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
    let body_text: Vec<String> = rs
        .body
        .lines()
        .skip(rs.scroll_top)
        .take(body_window_height(panes[0].height))
        .map(|l| l.to_string())
        .collect();
    let body_widget = Paragraph::new(body_text.join("\n"))
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
        let entries: Vec<String> = rs
            .outline
            .iter()
            .map(|(line, level, title)| {
                let indent = "  ".repeat(*level as usize - 1);
                let marker = if title.is_empty() { "¶" } else { "" };
                // `L<line>` gives the user a jump target (M8.2: `Enter` on
                // an outline row will scroll the body to `line`). Show it
                // dimmed so the title stays the visual focus.
                format!("{} L{line:<4}{}{}", indent, marker, title)
            })
            .collect();
        let outline_widget = Paragraph::new(entries.join("\n"))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" outline — {} section(s)", rs.outline.len())),
            )
            .wrap(ratatui::widgets::Wrap { trim: false });
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
fn picker_row(es: &crate::tui::EditState, field: crate::tui::EditField) -> String {
    // `entry_bytes` is the file-as-we-saw-it (the baseline). We use the raw
    // bytes + a fresh parse (not the cached `current_list`) so the user sees
    // what's *on disk*, never a stale working copy (which only ever differs
    // after a tail-removal via the smart `Backspace`, and only while they're
    // already in that field's Value stage).
    let bytes = es.entry_bytes.as_slice();
    let preview = if field.is_list() {
        let items: Vec<String> = kb_core::frontmatter::list_values(bytes, field.key())
            .unwrap_or_default();
        if items.is_empty() {
            String::from("(none)")
        } else {
            format!("[{}]", items.iter().map(|t| truncate(t.as_str(), 30)).collect::<Vec<_>>().join(" · "))
        }
    } else {
        let v = kb_core::frontmatter::scalar_value(bytes, field.key()).unwrap_or_default();
        if v.is_empty() {
            String::from("(none)")
        } else {
            format!("'{}'", truncate(v.as_str(), 50))
        }
    };
    format!("  {:<12}  {}", field.label(), preview)
}

/// The full *field list* the Picker shows — one row per field in the
/// stable `EditField::all()` order, with a `▶` marker on the current
/// highlight. This is the visual "field picker" pane.
fn build_picker_rows(es: &crate::tui::EditState) -> Vec<String> {
    let fields = crate::tui::EditField::all();
    let mut out = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        let row = picker_row(es, *field);
        let marker = if i == es.field_cursor { "▶ " } else { "  " };
        out.push(format!("{}{}", marker, row));
    }
    out
}

/// Draw the whole **Picker** stage pane (the field list). The layout is a
/// *single* bordered pane (no side-by-side suggestions in the Picker —
/// the suggestions are loaded + shown in the Editor stage, not yet); on a
/// scalar the *preview* is the current scalar value, and on a list the
/// *preview* is the existing items.
fn draw_picker_pane(f: &mut Frame, es: &crate::tui::EditState, rect: ratatui::layout::Rect) {
    let rows = build_picker_rows(es);
    let items: Vec<ListItem> = rows.into_iter().map(ListItem::new).collect();
    let highlight = Style::default().fg(Color::Black).bg(Color::White);
    let widget = RatList::new(items)
        .highlight_style(highlight)
        .block(Block::default().borders(Borders::ALL).title(
            " pick a field — ↑/↓ move · Enter open field · Esc back · q quit",
        ));
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
fn build_value_lines(es: &crate::tui::EditState) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    if es.field.is_list() {
        if es.in_list_select {
            // **Selection mode** (list field) — a *combined* list: every
            // existing item + a trailing `<new token>` slot (a "I want to
            // add a new one" entry the user's `←`/`→` spec calls for).
            // The `▶` marker is on `list_high` (Total: `list_high` is
            // always in `0..=current_list.len()`).
            out.push(String::from("  existing tags (Selection)"));
            for (i, t) in es.current_list.iter().enumerate() {
                let marker = if i == es.list_high { "    ▶ • " } else { "        " };
                out.push(format!("{}{}", marker, truncate(t.as_str(), 60)));
            }
            // The trailing `<new token>` slot — this is index
            // `current_list.len()` (the "add here" entry in the user's
            // spec: "selecting `<new>` brings it back to the normal add
            // new tag / element empty input buffer").
            let new_idx = es.current_list.len();
            let marker = if new_idx == es.list_high { "    ▶ " } else { "      " };
            out.push(format!("{}<new token>", marker));
            out.push(String::new());
            // Cached buffer (preserved from the prior Buffer mode —
            // "cache it so when user accidentally presses a right arrow,
            // they can immediately select `<new>`, and continue editing
            // the cached buffer").
            out.push(format!(
                "  cached buffer: '{}'",
                truncate(es.value.as_str(), 60)
            ));
        } else {
            // **Buffer mode** (list field) — the existing items as a
            // *flat list* (the pre-Selection UX), the buffer token, the
            // live-candidate list. (The Selection mode is the new path
            // for removing / loading existing items; this layout is
            // preserved for *add-new* — the zsh "completion" idiom.)
            if es.current_list.is_empty() {
                out.push(String::from("  existing: (none)"));
            } else {
                out.push(String::from("  existing:"));
                for (i, t) in es.current_list.iter().enumerate() {
                    let marker = if es.list_high == i { "    ▶ " } else { "      " };
                    out.push(format!("{}{}", marker, truncate(t.as_str(), 60)));
                }
            }
            out.push(String::new());
            out.push(format!(
                "  buffer:   '{}'",
                truncate(es.value.as_str(), 60)
            ));
            out.push(String::new());
            // Live candidate list (a pure function of buffer + corpus).
            if es.suggestions.is_empty() {
                out.push(String::from("  (no matches — keep typing)"));
            } else {
                out.push(String::from("  candidates:"));
                for (i, t) in es.suggestions.iter().take(40).enumerate() {
                    let marker = if i == es.sug_cursor { "    ▶ • " } else { "        " };
                    out.push(format!("{}{}", marker, truncate(t.as_str(), 60)));
                }
            }
        }
        out.push(String::new());
    } else {
        // **Scalar field**, buffer mode — render the buffer with a
        // *visible cursor glyph* at `value_cursor`. A `|` in monospace is
        // the classic text-cursor marker for a terminal TUI.
        let value = es.value.clone();
        let mut before = String::new();
        let mut after = String::new();
        let mut at = 0usize;
        let cursor = es.value_cursor;
        for c in value.chars() {
            if at == cursor {
                after.push(c);
            } else {
                before.push(c);
            }
            at += 1;
        }
        if at != cursor {
            // Cursor is past the end (shouldn't happen; `value_cursor`
            // is always clamped to `0..=chars.len()` on every mutation) —
            // render the full value + cursor at the end.
            before = value;
        }
        out.push(format!(
            "  buffer (cursor:): {}|{}",
            truncate(before.as_str(), 60),
            truncate(after.as_str(), 60)
        ));
        out.push(String::new());
    }

    // Pending commit plan — the *same* ops `Enter` will apply, so what the
    // user sees is exactly what will happen (no surprise writes).
    let plan = crate::tui::EditState::plan(es);
    if plan.is_empty() {
        out.push(String::from("  → no-op commit (nothing to change)"));
    } else {
        for op in &plan {
            out.push(format!("  → {}", describe_op(op)));
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
    .style(Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
    f.render_widget(header, chunks[0]);

    match es.stage {
        crate::tui::EditStage::Picker => {
            // Single pane: the field list (the *deliberate first step* —
            // the user chooses a field before touching its value).
            draw_picker_pane(f, es, chunks[1]);
        }
        crate::tui::EditStage::Value => {
            // Single pane: the value form (existing items + buffer + live
            // candidate list + pending plan). The *title* encodes the
            // current focus state (buffer vs. menu) so the user can see
            // at a glance which of the two things `↑/↓` / `Enter` are
            // addressing.
            let lines = build_value_lines(es);
            let widget = Paragraph::new(lines.join("\n"))
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
    .style(Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
    f.render_widget(header, chunks[0]);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(chunks[1]);

    let items: Vec<ListItem> = ast
        .list
        .iter()
        .map(|r| {
            let mut line = String::from(&r.alias);
            line.push_str("  →  ");
            line.push_str(&truncate(r.canonical.as_str(), 40));
            ListItem::new(line)
        })
        .collect();
    let list_title = if ast.focused {
        " ✓ alias list — ↑/↓ select · x remove · Tab form"
    } else {
        "   alias list (Tab to focus)"
    };
    let widget = RatList::new(items).block(Block::default().borders(Borders::ALL).title(list_title));
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
    .style(Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
    f.render_widget(header, chunks[0]);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(chunks[1]);

    let items: Vec<ListItem> = rs
        .list
        .iter()
        .map(|r| {
            let mut line = String::from(r.path.as_str());
            let opts: Vec<String> = r
                .extra_exts
                .as_deref()
                .map(|s| s.lines().map(|l| format!("ext:{l}")).collect())
                .unwrap_or_default();
            if !opts.is_empty() {
                line.push_str("  [");
                line.push_str(&opts.join(","));
                line.push(']');
            }
            if r.include_hidden {
                line.push_str("  [include-hidden]");
            }
            if let Some(added) = &r.added_at {
                line.push_str("  (added: ");
                line.push_str(added);
                line.push(')');
            }
            ListItem::new(line)
        })
        .collect();
    let list_title = if rs.focused {
        " ✓ roots list — ↑/↓ select · x drop · Tab form"
    } else {
        "   roots list (Tab to focus)"
    };
    let widget = RatList::new(items).block(Block::default().borders(Borders::ALL).title(list_title));
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

fn render_status_bar(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let bar_style = match app.status.kind {
        StatusKind::Info => Color::Gray,
        StatusKind::Success => Color::Green,
        StatusKind::Error => Color::Red,
    };
    let bar = Paragraph::new(format!("  {}", app.status.text)).style(Style::default().fg(bar_style));
    f.render_widget(bar, area);
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

/// The text of one Browse list row, fitted to `width` display cells.
///
/// The row is `<id>  <rel_path>` — there is **no separate title column**,
/// because `id` is `type:<slug>` and the slug is derived *from* the title
/// (title-wins identity), so the title is already inside the `id`. The old
/// code printed the title a second time and — with a fixed 40-*char* cap —
/// let `rel_path` (the *where the file is* field the user acts on) get
/// clipped off the right edge with no marker. Here `rel_path` (kept whole,
/// it's the locator) is guaranteed visible and `id` is ellipsized to what
/// fits, so every row is exactly `width` cells. Pure + total.
fn browse_row(row: &PageRow, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    // `rel_path` is the part we refuse to cut. Reserve its width, then fit
    // the id into the remaining cells (minus one space separator).
    let rel_w = row.rel_path.width();
    let sep = 1usize;
    if rel_w >= width {
        // Even the relative path alone doesn't fit — show it (truncated)
        // rather than losing the locator to the id.
        return ellipsize_cells(&row.rel_path, width);
    }
    let id_budget = width - rel_w - sep;
    let id = ellipsize_cells(&row.id, id_budget);
    format!("{id} {}", row.rel_path)
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
    #[test]
    fn browse_row_never_overflows_and_keeps_rel_path() {
        let long_id = "note:Realistic Document Title with a lot of words";
        for w in [1usize, 5, 20, 38, 40, 50, 78, 100, 200] {
            let out = browse_row(&row(long_id, "real60.md"), w);
            let got = cells(&out);
            assert!(got <= w, "row ({out:?}) is {got} cells, must be <= {w}");
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
        let out = browse_row(&row(&id, "dir/a/b/c.md"), 40);
        assert!(cells(&out) <= 40);
        assert!(out.ends_with("dir/a/b/c.md"), "locator wins over the id: {out:?}");
        assert!(out.contains('…'), "the cut id must carry an ellipsis: {out:?}");
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
            let out = browse_row(&row(&id, rel), w);
            let got = out.width();
            assert!(got <= w, "row ({out:?}) = {got} cells must be <= {w}");
            assert!(out.ends_with(rel), "locator stays visible: {out:?}");
            let budget = w.saturating_sub(rel_cells + 1);
            if id_cells <= budget {
                // The id fits: shown whole, no marker, exact width.
                assert!(
                    !out.contains('…'),
                    "a fitting id must not carry a marker: {out:?}"
                );
                assert!(
                    got == id_cells + 1 + rel_cells,
                    "fitting id fills exactly: {got} vs {expected}",
                    expected = id_cells + 1 + rel_cells
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
    /// positive width.
    #[test]
    fn browse_row_falls_back_to_rel_path_when_it_overflows() {
        let id = "note:short";
        let rel = format!("/{}", "a-very-long-absolute-ish-relative-path".repeat(4));
        let out = browse_row(&row(id, &rel), 20);
        assert!(!out.is_empty(), "positive width never yields an empty row");
        assert!(cells(&out) <= 20, "fallback still bounded: {out:?}");
        assert!(out.contains('…'), "an overflowed locator is still marked: {out:?}");
    }
}
