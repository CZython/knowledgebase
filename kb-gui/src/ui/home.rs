//! Home (PLAN-GUI §6.13): the launch screen when a path is *not* the
//! launch target and at least one root exists.
//!
//! Layout: `kb` heading, a centred search field (full
//! `search_grammar`; `Enter` opens the top hit), a row of action buttons
//! (Open existing page, Create new, Open Markdown file), and a `Recent`
//! list derived from `pages.updated_at`.

use eframe::egui;

use crate::app::App;

/// Paint the Home screen.
pub fn paint(ui: &mut egui::Ui, app: &mut App) {
    ui.vertical(|ui| {
        ui.add_space(8.0);
        ui.heading("kb");
        ui.add_space(8.0);

        // The Home search field. Editing it updates the shared query;
        // `Enter` runs the search and walks into the top result.
        let query = app.state().search.query.clone();
        let mut query = query;
        let resp = ui.add_sized(
            egui::vec2(ui.available_width(), 40.0),
            egui::TextEdit::singleline(&mut query)
                .hint_text("type a query, page title, tag, or uuid…")
                .desired_width(ui.available_width()),
        );
        // Canonical egui pattern: a focused single-line field surrenders
        // focus on Enter-*press*, so detect the run on `key_pressed`
        // (a `key_released` check misses the release frame after the
        // surrender and fires the open too late).
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            app.state_mut().search.query = query;
            run_top(app);
            return;
        }
        // Live-update the shared query so the overlay (Ctrl+K) sees it too.
        app.state_mut().search.query = query;

        // Action row.
        ui.add_space(12.0);
        ui.horizontal_centered(|ui| {
            // Clone the abs_path we'd open *before* the mutable call so the
            // immutable borrow on `pages` has ended.
            let first_path: Option<String> = app.state().pages.first().map(|p| p.abs_path.clone());
            if ui.button("Open existing page…").clicked() {
                if let Some(abs) = first_path {
                    app.open_page(&abs);
                }
            }
            ui.add_space(8.0);
            if ui.button("Create new page…").clicked() {
                app.open_create_note();
            }
            ui.add_space(8.0);
            if ui.button("Open Markdown file…").clicked() {
                crate::ui::welcome::picker_open_file(app);
            }
        });

        ui.add_space(20.0);
        ui.strong("Recent");
        ui.add_space(4.0);

        // Owned (abs_path, title, gates, updated_at) tuples so we can
        // borrow-mutably (`open_page`) while iterating them.
        let mut recent: Vec<(String, String, String, String)> = app
            .state()
            .pages
            .iter()
            .filter(|p| !p.updated_at.is_empty())
            .map(|p| {
                (
                    p.abs_path.clone(),
                    p.title.clone().unwrap_or_else(|| p.abs_path.clone()),
                    if p.gates.is_empty() {
                        String::new()
                    } else {
                        p.gates.join(", ")
                    },
                    p.updated_at.clone(),
                )
            })
            .collect();
        // ISO-8601 → lexicographic sort == chronological. Newest first.
        recent.sort_by(|a, b| b.3.cmp(&a.3));
        recent.truncate(10);

        if recent.is_empty() {
            ui.weak("No pages indexed yet. Add a root to get started.");
        }
        for (abs_path, title, gates, updated_at) in recent {
            if ui.selectable_label(false, format!("▤  {title}")).clicked() {
                app.open_page(&abs_path);
                continue;
            }
            let line = weak_meta(&gates, &updated_at);
            ui.weak(format!("   {line}"));
        }
    });
}

/// The muted meta line after a Recent row: `in <gates>` + timestamp.
fn weak_meta(gates: &str, updated_at: &str) -> String {
    let mut out = String::new();
    if !gates.is_empty() {
        out.push_str(&format!("  in {gates}  "));
    }
    out.push_str(updated_at);
    out
}

/// Run the current Home query (strict, unscoped) and open the top result.
fn run_top(app: &mut App) {
    // Clone the top hit's abs_path before the mutable open_page borrow.
    let top: Option<String> = app.state().search.rows.first().map(|h| h.abs_path.clone());
    if let Some(abs) = top {
        app.open_page(&abs);
    }
}
