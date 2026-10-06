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

use crate::tui::{App, Mode, ReadState, StatusKind};

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

    let items: Vec<ListItem> = app
        .view
        .iter()
        .map(|r| {
            let mut line = String::from(r.id.as_str());
            if !r.title.is_empty() {
                line.push(' ');
                line.push_str(&truncate(r.title.as_str(), 40));
            }
            line.push(' ');
            line.push_str(&r.rel_path);
            ListItem::new(line)
        })
        .collect();

    let title = if app.filter_text.is_empty() {
        format!(" {} page(s) — ↑↓/jk move · / filter · Enter open · a alias · r roots · e edit · q quit", app.view.len())
    } else {
        format!(" {} page(s) of {} match '{}' — / to edit filter · Esc to clear",
                app.view.len(), app.rows.len(), app.filter_text)
    };
    let widget = RatList::new(items).block(Block::default().borders(Borders::ALL).title(title));

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
fn draw_edit(f: &mut Frame, app: &App, es: &crate::tui::EditState, area: ratatui::layout::Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // form + suggestions side-by-side
            Constraint::Length(1), // status
        ])
        .split(area);

    let header = Paragraph::new(format!(
        "  kb tui — editing {} · field {} · Esc back, q quit",
        es.id,
        es.field.label(),
    ))
    .style(Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
    f.render_widget(header, chunks[0]);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    // LEFT: the form. For a scalar: the typed value. For a list: the
    // current items, the new value (typed), and the pending commit plan.
    let form_lines = build_form_lines(es);
    let form_widget = Paragraph::new(form_lines.join("\n"))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    " {} (↑/j field · Tab suggest(list) · x remove-tail(list) · Enter commit)",
                    es.field.label()
                )),
        )
        .wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(form_widget, panes[0]);

    // RIGHT: the live suggestions. Empty for scalar; a dim hint when the
    // buffer matches nothing.
    let sug_text = if es.suggestions.is_empty() {
        String::from("  (no matches — keep typing)")
    } else {
        let mut s = String::new();
        for (i, t) in es.suggestions.iter().take(50).enumerate() {
            if i > 0 {
                s.push('\n');
            }
            s.push_str(&format!("  • {}", truncate(t.as_str(), 60)));
        }
        s
    };
    let sug_widget = Paragraph::new(sug_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" suggestions — {} match(es)", es.suggestions.len())),
        )
        .wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(sug_widget, panes[1]);

    render_status_bar(f, app, chunks[2]);
}

/// Assemble the form-lines for the active field (a pure function of
/// `EditState` — testable without a DB / Frame). The "pending plan"
/// footer is the one source of truth for "what will Enter apply"; it is
/// the same `build_ops` + `apply_op` pair `commit_edit` runs, surfaced
/// one step earlier so the user isn't surprised by a write.
fn build_form_lines(es: &crate::tui::EditState) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if es.field.is_list() {
        out.push(format!(
            "  current: [{}]",
            es.current_list
                .iter()
                .map(|t| truncate(t.as_str(), 60))
                .collect::<Vec<_>>()
                .join(" · ")
        ));
        out.push(format!("  new:     '{}'", es.value));
    } else {
        out.push(format!("  value:   '{}'", es.value));
    }
    out.push(String::new());
    // The pending commit plan. (See `crate::tui::build_ops`; empty on a
    // no-op commit, which the form renders as "no-op commit (no changes)".)
    let plan = crate::tui::EditState::plan(es);
    if plan.is_empty() {
        out.push(String::from("  → no-op commit (no changes)"));
    } else {
        for op in &plan {
            out.push(format!("  → {}", describe_op(op)));
        }
    }
    out
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
