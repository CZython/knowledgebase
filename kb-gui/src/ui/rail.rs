//! The collections rail (PLAN-GUI §6.3): `All pages` / `Gated` / `Ungated`
//! pinned tiles + one tile per corpus collection (gate term).
//!
//! G1 wires selection: clicking a tile sets `state.collection` (the rail
//! selection *is* the list scope — PLAN-GUI §2.1 rule 3), and the active
//! tile paints with the selection background. The pinned tiles show their
//! counts; the collection tiles show the corpus's gate term + page count.
//! Bottom-of-rail shortcuts: `+ Add root` (functional via the rfd picker)
//! and `Manage collections` (reserved for G5).

use eframe::egui;

use crate::app::App;
use crate::state::CollectionKey;

/// Click → selection. The rail selection *is* the list scope (PLAN-GUI
/// §2.1 rule 3); the search scope is re-derived from it in the search
/// overlay. We only mutate the single-valued `state.collection`.
fn select(app: &mut App, key: CollectionKey) {
    app.state_mut().collection = key;
}

/// `true` if `current` equals `candidate` (the selection is single-valued).
fn is_selected(current: &CollectionKey, candidate: &CollectionKey) -> bool {
    match (current, candidate) {
        (CollectionKey::All, CollectionKey::All) => true,
        (CollectionKey::Gated, CollectionKey::Gated) => true,
        (CollectionKey::Ungated, CollectionKey::Ungated) => true,
        (CollectionKey::Term(a), CollectionKey::Term(b)) => a == b,
        _ => false,
    }
}

/// Paint the rail's contents (called from `ui/mod.rs`).
pub fn paint(ui: &mut egui::Ui, app: &mut App) {
    ui.vertical(|ui| {
        ui.strong("Collections");
        ui.add_space(4.0);

        let current = app.state().collection.clone();
        let total = app.state().pages.len();
        let gated_count = app.state().gated_count;
        let ungated_count = app.state().ungated_count;
        let collections: Vec<(String, usize)> = app.state().collections.clone();

        // Pinned tiles.
        if ui
            .selectable_label(
                is_selected(&current, &CollectionKey::All),
                format!("All pages · {total}"),
            )
            .clicked()
        {
            select(app, CollectionKey::All);
        }
        if ui
            .selectable_label(
                is_selected(&current, &CollectionKey::Gated),
                format!("Gated · {gated_count}"),
            )
            .clicked()
        {
            select(app, CollectionKey::Gated);
        }
        if ui
            .selectable_label(
                is_selected(&current, &CollectionKey::Ungated),
                format!("Ungated · {ungated_count}"),
            )
            .clicked()
        {
            select(app, CollectionKey::Ungated);
        }

        // Per-collection tiles (gate terms, in the corpus's canonical order).
        for (gate, count) in &collections {
            let key = CollectionKey::Term(gate.clone());
            if ui
                .selectable_label(is_selected(&current, &key), format!("• {gate} · {count}"))
                .clicked()
            {
                select(app, key);
            }
        }

        // Bottom-of-rail shortcuts (PLAN-GUI §6.3). A plain stack below the
        // tiles (egui's `bottom_up` scope does not position correctly inside
        // an infinite-height panel, and the tiles already claim all the
        // height they need).
        ui.separator();
        if ui.button("+ Add root").clicked() {
            let _ = crate::ui::welcome::picker_add_root(app);
        }
        if ui.weak("Manage collections").clicked() {
            let _ = app; // reserved for G5.
        }
    });
}
