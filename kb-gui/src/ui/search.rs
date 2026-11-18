//! The search overlay (PLAN-GUI §6.8 / §6.9): a centred modal box over the
//! shell, opened with `Cmd/Ctrl+K` (or a click on the 🔎 button).
//!
//! G1 wires the functional surface:
//! - A single-line input bound to `SearchView::query`; `Enter` runs the
//!   query through the worker (`run_search`) and the results paint in the
//!   box below the input.
//! - A scope chip (when the rail selection is a gate term) —
//!   `in <gate>` — rendered next to the input.
//! - The results list (cursor-walkable with `↑`/`↓`; `Enter` opens the
//!   top hit). Each row: `type:slug` + (optional) description, and a muted
//!   `rel_path` line.
//! - `Esc` / clicking the backdrop closes the overlay.
//!
//! The overlay paints only when `SearchView::open` is `true`; it is a no-op
//! otherwise, so `ui/mod.rs` can call it unconditionally.

use eframe::egui;

use crate::app::App;
use crate::state::CollectionKey;

/// Paint the overlay when it is open (no-op otherwise). Called last in the
/// paint pass so it renders *over* the shell.
pub fn paint_if_open(app: &mut App, ui: &mut egui::Ui) {
    if !app.state().search.open {
        return;
    }
    egui::Window::new("Search")
        .collapsible(false)
        .resizable(false)
        .min_width(560.0)
        .show(ui.ctx(), |ui| {
            paint_box(ui, app);
        });
}

fn paint_box(ui: &mut egui::Ui, app: &mut App) {
    // Escape closes the overlay (the plan's "Esc closes").
    if ui.input(|i| i.key_released(egui::Key::Escape)) {
        app.toggle_search(None);
        return;
    }

    // The query field. We paint a *local* copy each frame (egui wants an
    // owned `&mut String`), and write it back into the shared state only
    // when it actually changed.
    let mut query = app.state().search.query.clone();
    let resp = ui.add(
        egui::TextEdit::singleline(&mut query)
            .hint_text("query, tag:, or uuid — Enter to search")
            .desired_width(ui.available_width()),
    );
    if resp.changed() {
        app.state_mut().search.query = query.clone();
    }

    // Scope chip: present when the rail selection is a gate term.
    if let CollectionKey::Term(gate) = &app.state().collection {
        ui.weak(format!("  (scoping: in {gate})"));
    }

    // Enter runs the search. Guard: only when the field has focus, so the
    // results-list `Enter` (open top hit) is not also triggered here.
    let input_active = resp.has_focus();
    if resp.lost_focus() && ui.input(|i| i.key_released(egui::Key::Enter)) && input_active {
        app.run_search();
        return;
    }

    // Results.
    let rows: Vec<crate::state::SearchHit> = app.state().search.rows.clone();
    let cursor = app.state().search.cursor;

    ui.add_space(8.0);

    if rows.is_empty() {
        ui.weak("type a query and press Enter…");
        return;
    }

    ui.weak(format!("{} result(s)", rows.len()));
    ui.add_space(4.0);

    egui::ScrollArea::vertical()
        .max_height(360.0)
        .show(ui, |ui| {
            for (i, hit) in rows.iter().enumerate() {
                let is_cursor = i == cursor;
                let title = hit.title.clone().unwrap_or_else(|| hit.id.clone());
                let sub = format!("{} · {}", hit.type_, hit.rel_path);

                // Cursor-walk with the arrows. Mutating the cursor here
                // does not re-fire the search (the query is unchanged).
                let resp = ui
                    .selectable_label(is_cursor, format!("▤ {title}"))
                    .on_hover_text(&sub);

                if resp.clicked() {
                    open_hit(app, hit);
                    return;
                }
                ui.weak(sub.clone());
                if !hit.gates.is_empty() {
                    ui.weak(format!("  in {}", hit.gates));
                }
                ui.add_space(4.0);
            }
        });

    // Arrow-key cursor navigation.
    if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
        if cursor + 1 < rows.len() {
            app.state_mut().search.cursor = cursor + 1;
        }
    } else if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) && cursor > 0 {
        app.state_mut().search.cursor = cursor - 1;
    }
    // Enter on the list (field not focused) opens the cursor row.
    if !input_active && ui.input(|i| i.key_released(egui::Key::Enter)) {
        if let Some(hit) = rows.get(cursor) {
            open_hit(app, hit);
        }
    }
}

/// Open the clicked / cursor search hit: navigate into the page and close
/// the overlay (the plan's "Enter opens the top hit and closes the box").
fn open_hit(app: &mut App, hit: &crate::state::SearchHit) {
    let abs = hit.abs_path.clone();
    app.open_page(&abs);
    app.toggle_search(None);
}
