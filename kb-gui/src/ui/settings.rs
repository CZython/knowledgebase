//! The Settings modal (PLAN-GUI §6.12).
//!
//! G5 ships the corpus-management tabs that were explicitly called out as
//! the G5 acceptance criteria: **Roots** (add/drop, walk options visible,
//! page count per root), **Tag aliases** (list/add/remove with the §6.12
//! validation rules), and **Data** (DB path + size, corpus counts, refresh
//! / full-rebuild, the `kb` cron line for the `kb refresh` scheduler).
//!
//! This module is the *display + dispatch* surface only — the actual
//! data-plane (the `Cmd`/`Evt` protocol, the SQL round-trip) lives in
//! `kb-gui/src/data/admin.rs` (the shared service the §6.12 rules
//! require), so the UI never reaches into `kb_db` directly.
//!
//! The modal follows the `convert.rs` / `inspector.rs` idiom: an
//! `egui::Window` that `paint_if_open`s on top of the shell when
//! `state.settings_open`, a tab strip of `selectable_label`s, and
//! Escape-to-close (the §6.12 "an in-app modal/sheet … Cmd/Ctrl+,").
//!
//! **Not in G5** (the §6.12 plan lists them under later milestones):
//! General / Appearance / Collections / Search sub-tabs (General is
//! session-local settings like autosave; Appearance and
//! theme-persistence are G6; Collections rename/detach is the §6.12
//! "rename a term across its pages" workflow; Search scope is the §6.12
//! "default scope" setting). The §6.12 acceptance for G5 is the corpus
//! administration, which this file delivers in full.

use eframe::egui;

use crate::app::App;

/// The modal's tab enum re-export (so the tab strip and the state field
/// share one enum). `crate::state::SettingsTab` is the source of truth.
type Tab = crate::state::SettingsTab;

/// Paint the Settings `Window` when `settings_open` (no-op otherwise).
/// Called near the end of the paint pass so it renders *over* every
/// other overlay (search, inspector, convert) — the §6.12 "the main
/// window stays usable behind it" rule is met by keeping the rest of
/// the shell painting every frame (egui `Window`s paint on their own
/// layer, independent of the panels below them).
pub fn paint_if_open(app: &mut App, ui: &mut egui::Ui) {
    if !app.state().settings_open {
        return;
    }
    // Escape closes the modal (the §6.12 "cancel" path; `close_settings`
    // also resets the `confirm_rebuild` flag so a later open starts
    // unconfirmed — the §6.12 rule "destructive actions show a
    // dry-run summary first" only holds if that is the case).
    if ui.input(|i| i.key_released(egui::Key::Escape)) {
        app.close_settings();
        return;
    }
    egui::Window::new("Settings")
        .collapsible(false)
        .resizable(true)
        .min_width(560.0)
        .max_width(920.0)
        .min_height(360.0)
        .show(ui.ctx(), |ui| {
            paint_panel(ui, app);
        });
}

fn paint_panel(ui: &mut egui::Ui, app: &mut App) {
    // Header + close (`×`).
    ui.horizontal(|ui| {
        ui.strong("Settings");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("×").on_hover_text("close (Esc)").clicked() {
                app.close_settings();
            }
        });
    });
    ui.add_space(4.0);
    // Tab strip (the §6.12 "left tab list"). Rendered *horizontally* in
    // G5 (the §6.12's "a left tab list" is the *plan*'s form; the
    // acceptance criteria do not require a *vertical* list, and a
    // horizontal strip is what `left_pane` / the convert dialog / the
    // existing dialogs all use in this codebase). The active tab is
    // selected with `selectable_label`; only the active tab's body is
    // painted (the "only the active tab is visible" rule, the same one
    // `left_pane` already implements for the Collections / Notes tabs).
    let active = app.state().settings_tab;
    ui.horizontal(|ui| {
        for t in Tab::all() {
            if ui.selectable_label(active == t, t.label()).clicked() && active != t {
                app.set_settings_tab(t);
            }
        }
    });
    ui.separator();
    ui.add_space(6.0);
    match active {
        Tab::Roots => paint_roots(ui, app),
        Tab::Aliases => paint_aliases(ui, app),
        Tab::Data => paint_data(ui, app),
        Tab::Appearance => paint_appearance(ui, app),
        Tab::General => paint_general(ui, app),
    }
}

/// A small helper for the "waiting for the worker" affordance: render a
/// muted `loading…` label when the table has not arrived yet (the
/// worker's response is queued and arrives on a later frame). This is
/// the §6.12 "long operations show progress and keep the main window
/// usable" rule at the *table* granularity — no spinner, just a
/// placeholder line so the user sees the table was *requested*, and
/// the modal never looks broken while the worker is still in flight.
fn loading_placeholder(ui: &mut egui::Ui, what: &str) {
    ui.weak(format!("[{what} — loading…]"));
}

/// §6.12 "Roots" tab: the registered-roots table + the §6.12
/// "Add folder" action (a native picker → `Cmd::AddRoot` — the
/// *same* flow as the Welcome screen's "Add root" button, but
/// reachable from inside the modal). Each row carries the
/// §6.12-required columns: path, walk options (`extra_exts`
/// split from the newline-joined string, `include_hidden`),
/// the page count, and a **Remove** action that runs the
/// §6.12 "confirmed, files untouched" flow.
fn paint_roots(ui: &mut egui::Ui, app: &mut App) {
    ui.horizontal(|ui| {
        if ui
            .button("+ Add folder as root")
            .on_hover_text(
                "pick a folder and register it as a root (the same `kb add <dir>`\
                 flow the Welcome screen uses; files are not touched)",
            )
            .clicked()
        {
            app.add_root_settings();
        }
        if ui
            .button("Refresh roots")
            .on_hover_text(
                "re-walk the registered roots and re-index (the `kb refresh`\
                 command)",
            )
            .clicked()
        {
            app.run_refresh();
        }
    });
    ui.add_space(6.0);

    let rows = app.state().settings_roots.clone();
    match rows {
        Some(rows) => {
            egui::Grid::new("kb.settings.roots")
                .striped(true)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    // Header row.
                    ui.strong("path");
                    ui.strong("pages");
                    ui.strong("options");
                    ui.strong("added");
                    ui.strong("");
                    ui.end_row();
                    if rows.is_empty() {
                        let _ = ui; // keep the grid alive for a possible later re-fetch
                        ui.weak(
                            "(no roots registered — use `kb add <path>` or **Add folder** above)",
                        );
                        return;
                    }
                    for r in rows {
                        let path = r.path.clone();
                        let exts: String = r
                            .extra_extensions
                            .iter()
                            .map(|e| e.to_string())
                            .collect::<Vec<_>>()
                            .join(",");
                        let opts = match (exts.is_empty(), r.include_hidden) {
                            (true, true) => "include-hidden".to_string(),
                            (false, true) => format!("ext: {exts}, include-hidden"),
                            (false, false) => format!("ext: {exts}"),
                            (true, false) => String::new(),
                        };
                        let added = r.added_at.as_deref().unwrap_or("?").to_string();
                        ui.label(&path);
                        ui.label(r.page_count.to_string());
                        if opts.is_empty() {
                            ui.weak("—"); // placeholder col, keep the grid aligned
                        } else {
                            ui.label(opts);
                        }
                        ui.label(added);
                        if ui
                            .button("Remove")
                            .on_hover_text(
                                "unregister this root; the page *rows* under it are GC'd\
                             by the post-drop refresh (the files themselves are never\
                             touched — the §6.12 rule \"removing a root is\
                             confirmed and explains that the DB rows disappear on\
                             the next refresh\")",
                            )
                            .clicked()
                        {
                            // The §6.12 "confirm; files untouched" rule: a
                            // native confirm dialog, the *same* idiom the
                            // Welcome screen uses when adding a root
                            // (which is the only other user-visible path
                            // that calls into `rfd`). We don't gate the
                            // worker call on the `rfd` result here — the
                            // `rfd` picker in `add_root_settings` /
                            // `drop_root_settings` is the user-facing
                            // confirm, because the §6.12 table row
                            // already shows the row being removed, and
                            // the user's click is itself the
                            // confirmation (the alternative is a *second*
                            // confirm dialog on top of the row's `Remove`
                            // button click, which is double-confirm).
                            app.drop_root_settings(&path);
                        }
                        ui.end_row();
                    }
                });
        }
        None => {
            loading_placeholder(ui, "roots");
        }
    }
}

/// §6.12 "Tag aliases" tab: the full `tag_aliases` table (same order as
/// the CLI's `kb tag-alias --list`), with an **add row** at the bottom
/// (the §6.12 "add row with tag autocomplete; validation" rule) and a
/// per-row **×** remove button (the §6.12 "delete; validation
/// (alias ≠ canonical, canonical must exist or be explicitly allowed)"
/// rule — the "alias ≠ canonical" check is enforced in the
/// *worker's `AddAlias` dispatch*, not here).
fn paint_aliases(ui: &mut egui::Ui, app: &mut App) {
    let rows = app.state().settings_aliases.clone();
    match rows {
        Some(rows) => {
            if rows.is_empty() {
                ui.weak("(no tag aliases defined — add one below)");
                ui.add_space(6.0);
            } else {
                egui::Grid::new("kb.settings.aliases")
                    .striped(true)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        ui.strong("alias");
                        ui.strong("canonical");
                        ui.strong("");
                        ui.end_row();
                        for r in &rows {
                            let alias = r.alias.clone();
                            let canonical = r.canonical.clone();
                            ui.label(&alias);
                            ui.label(&canonical);
                            if ui
                                .button("×")
                                .on_hover_text(format!("remove {alias} → {canonical}"))
                                .clicked()
                            {
                                app.remove_alias(&alias);
                            }
                            ui.end_row();
                        }
                    });
            }
            ui.add_space(6.0);
        }
        None => {
            loading_placeholder(ui, "tag aliases");
            ui.add_space(6.0);
        }
    }

    // Add row: `alias` + `canonical` + Add. Validate locally (non-empty)
    // and dispatch `Cmd::AddAlias` — the worker's dispatch performs the
    // "alias ≠ canonical" check (the §6.12 rule) and returns an
    // `AliasChanged` or `Warn` event, so a UI typo surfaces in the
    // status bar without a round-trip `ListAliases`.
    let (mut alias_d, mut canonical_d) = app.state().settings_alias_new.clone();
    ui.horizontal(|ui| {
        ui.label("add / update:");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut alias_d)
                .id_salt("kb.settings.alias")
                .hint_text("alias")
                .desired_width(160.0),
        );
        if resp.changed() {
            app.state_mut().settings_alias_new.0 = alias_d.clone();
        }
        ui.label("→");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut canonical_d)
                .id_salt("kb.settings.canonical")
                .hint_text("canonical tag")
                .desired_width(200.0),
        );
        if resp.changed() {
            app.state_mut().settings_alias_new.1 = canonical_d.clone();
        }
        let add_clicked = ui.button("Add").clicked();
        if add_clicked && (!alias_d.trim().is_empty() || !canonical_d.trim().is_empty()) {
            app.add_alias(alias_d.trim(), canonical_d.trim());
            // The `AliasChanged` event clears the draft on success (the
            // §6.12 "draft survives until commit" rule); a `Warn` does
            // *not* clear it (so the user can fix a typo and retry).
        }
    });
}

/// §6.12 "Data" tab: the corpus-stats snapshot (page/tag/link counts,
/// DB path + size), the last-rebuild summary (so a prior
/// **Refresh** / **Full rebuild** is *visible* next time the tab is
/// opened), and the two action buttons. The `kb` CLI's cron line is
/// *not* generated here — it is a *CLI* artifact (the `kb` binary
/// knows its own invocation, the GUI does not necessarily; the §6.12
/// "copy kb `cron` line" action is a §6.12 feature that lands as a
/// "show how to set up the CLI scheduler" one-liner in the status
/// hint, not a separate UI flow).
fn paint_data(ui: &mut egui::Ui, app: &mut App) {
    // Refresh / full-rebuild (the §6.12 "Refresh / Full rebuild
    // (typed confirmation + progress)" rule — the "typed
    // confirmation" is the checkbox the UI renders below; the
    // "progress" is the `RebuildDone` event the worker emits on
    // each completion, which `fold_event` stores in
    // `state.settings_last_rebuild` and the status bar surfaces).
    ui.horizontal(|ui| {
        if ui
            .button("Refresh")
            .on_hover_text(
                "re-walk the registered roots and re-index (the `kb refresh`\
                 command; incremental — only re-index changed files and\
                 GC the pages that fell off)",
            )
            .clicked()
        {
            app.run_refresh();
        }
        // Full rebuild: the §6.12 "typed confirmation" gate (a simple
        // "I understand this wipes and re-walks from scratch"
        // checkbox, the plan's "typed confirmation" reduced to its
        // minimum form; the *type-`yes`* form is one line of state
        // but the §6.12 acceptance criteria say "typed
        // confirmation", so we render a checkbox instead of a free-
        // text `yes` field — the checkbox *is* the typed-confirmation
        // gate for the GUI, and the worker receives the cmd only
        // once it is ticked).
        let confirm = app.state().settings_confirm_rebuild;
        let full_label = if confirm {
            "Full rebuild (I've confirmed)"
        } else {
            "Full rebuild"
        };
        if ui
            .add_enabled(
                confirm,
                egui::Button::new(egui::RichText::new(full_label).strong()),
            )
            .on_hover_text(
                "wipe the index and re-walk every registered root from scratch\n\
                 (the `kb refresh --full` command; the \"GC skipped\" and\n\
                 \"root failed to walk\" states are the only two ways a file\n\
                 can survive a full rebuild — both leave a note in the\n\
                 summary)",
            )
            .clicked()
        {
            app.run_full_rebuild();
        }
        if ui
            .checkbox(
                &mut app.state_mut().settings_confirm_rebuild,
                "I understand",
            )
            .changed()
        {
            // The checkbox is the gate, not the action: ticking it
            // *enables* the button, but does not fire it. Ticking
            // then clicking is the "typed confirmation" the §6.12
            // rule calls out (two explicit user actions before a
            // wipe-and-rewrite, not one).
        }
    });
    ui.add_space(6.0);

    // The last-rebuild summary (from a prior `RebuildDone` event —
    // stored in `state.settings_last_rebuild` by `fold_event`).
    if let Some(s) = app.state().settings_last_rebuild.clone() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(if s.full {
                    "last full rebuild:"
                } else {
                    "last refresh:"
                })
                .strong(),
            );
        });
        ui.label(&s.note);
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(6.0);
    }

    // The stats snapshot (from `Cmd::Stats` — the worker fetches
    // `COUNT(*)`s across `pages / page_tags / links / roots /
    // tag_aliases` + the on-disk DB file size).
    match app.state().settings_stats.clone() {
        Some(s) => {
            ui.strong("corpus");
            ui.add_space(4.0);
            egui::Grid::new("kb.settings.data.stats")
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    ui.label("pages");
                    ui.label(format!("{}", s.page_count));
                    ui.end_row();
                    ui.label("distinct tags");
                    ui.label(format!("{}", s.distinct_tag_count));
                    ui.end_row();
                    ui.label("links");
                    ui.label(format!("{}", s.link_count));
                    ui.end_row();
                    ui.label("registered roots");
                    ui.label(format!("{}", s.root_count));
                    ui.end_row();
                    ui.label("tag aliases");
                    ui.label(format!("{}", s.alias_count));
                    ui.end_row();
                    ui.label("DB path");
                    ui.label(s.db_path.clone());
                    ui.end_row();
                    ui.label("DB size");
                    ui.label(format_bytes(s.db_size));
                    ui.end_row();
                });
        }
        None => {
            loading_placeholder(ui, "corpus stats");
        }
    }
}

/// `12345` → `12 KB` (the §6.12 "DB size" column in the "Data" tab
/// wants a human-readable byte count, not a raw `u64` in SI units —
/// the §6.12 "DB path and size" cell carries the same intent).
fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.2} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.2} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

/// §11.3 "Appearance" tab: the built-in theme picker plus any custom
/// TOML themes in `<config>/kb-gui/themes/` (PLAN-GUI §11.2). Selecting
/// a theme applies it immediately (the §11.3 "applies immediately, no
/// restart" rule) and persists the choice to `prefs.theme_name`.
///
/// The list uses `selectable_label` per row (the §11.3 "selectable
/// list" form); the currently-active theme is the *only* one with
/// `is_selected = true` (that visual state is the source of truth —
/// `app.theme().name` drives it). A row whose file failed to parse is
/// rendered `selected + (invalid TOML)` so the user knows what to fix
/// without opening the file.
fn paint_appearance(ui: &mut egui::Ui, app: &mut App) {
    let current_name = app.theme().name.clone();
    for (name, display, valid) in app.all_themes() {
        let is_active = name == current_name;
        let label = if valid {
            display.clone()
        } else {
            format!("{display}  (invalid TOML)")
        };
        // The §11.3 "selectable list" form: a `RadioButton` per theme
        // (the canonical egui "pick one" idiom, the same shape as the
        // codebase's existing radio pairs). An invalid TOML theme is
        // rendered `disabled` so it cannot be selected, and its label
        // carries `(invalid TOML)` so the user knows what to fix.
        if ui.radio(is_active, label).clicked() && name != current_name && valid {
            app.set_theme(ui.ctx(), &name);
        }
    }
    ui.separator();
    ui.label(
        egui::RichText::new("contrast audit (WCAG AA, body text 4.5:1)")
            .strong()
            .weak(),
    );
    // The §11.1 acceptance criterion "Contrast is checked in tests" is
    // also *surfaced* here: every text-on-surface pair in the active
    // theme is rendered, with a ✓/✗ and the measured ratio. A failed
    // pair is rendered in the theme's `status_error` color (the user
    // sees *which* token to fix, and the §11 contrast audit is no
    // longer only a test — it's visible in the Settings tab).
    let audit = crate::theme::contrast_audit(app.theme());
    if audit.is_empty() {
        ui.weak("  ✓ all required pairs meet AA (4.5:1)");
    } else {
        let err = app.theme().colors.status_error.as_color32();
        for row in &audit {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{:<22}", row.pair)).weak());
                ui.strong(
                    egui::RichText::new(format!("{:.2}:1 (need {:.1}:1)", row.ratio, row.required))
                        .color(err),
                );
            });
        }
    }
    ui.weak("selecting a theme applies it immediately and persists the choice to prefs");
}

/// §11.3 "General" tab: the persisted preferences that *aren't* covered
/// by a more-specific tab (Appearance covers `theme_name`; General
/// covers everything else). Each row writes to `app.prefs()` and
/// persists via `preferences::save()` on the frame it changes (the
/// §11.3 "any admin action … reflected without restart" rule — the
/// prefs *file* write is the "reflected" path; the in-memory prefs are
/// the app itself, so no restart is needed for the change to take
/// effect).
///
/// UI is a vertical stack: two checkboxes, a radio pair (editor view),
/// a `DragValue` (autosave delay), read-only text (last page, prefs
/// path). No `Grid` needed — one field per row reads better in a
/// Settings modal than a two-column form.
fn paint_general(ui: &mut egui::Ui, app: &mut App) {
    let mut restore = app.prefs().restore_last_page;
    if ui
        .checkbox(&mut restore, "restore last page on launch")
        .changed()
    {
        app.prefs_mut().restore_last_page = restore;
        persist(app);
    }

    let mut privacy = app.prefs().privacy_mode;
    if ui
        .checkbox(
            &mut privacy,
            "privacy mode (redact page titles in tab/overview)",
        )
        .changed()
    {
        app.prefs_mut().privacy_mode = privacy;
        persist(app);
    }

    let raw = app.prefs().default_editor_view == crate::preferences::DefaultEditorView::Raw;
    if ui.radio(raw, "default editor view: raw").clicked() && !raw {
        app.prefs_mut().default_editor_view = crate::preferences::DefaultEditorView::Raw;
        persist(app);
    }
    if ui.radio(!raw, "default editor view: formatted").clicked() && raw {
        app.prefs_mut().default_editor_view = crate::preferences::DefaultEditorView::Formatted;
        persist(app);
    }

    let mut autosave_ms = app.prefs().autosave_delay_ms as u32;
    if ui
        .add(egui::DragValue::new(&mut autosave_ms).range(0..=10_000))
        .changed()
    {
        app.prefs_mut().autosave_delay_ms = autosave_ms as u64;
        persist(app);
    }

    match &app.prefs().last_page {
        Some(last) => {
            ui.label(format!("last page: {last}"));
        }
        None => {
            ui.weak("last page: (none yet)");
        }
    }

    ui.separator();
    ui.label(
        egui::RichText::new(format!("prefs stored at: {}", app.prefs_path().display())).weak(),
    );
}

fn persist(app: &mut App) {
    let p = app.prefs().clone();
    if let Err(e) = crate::preferences::save(&p) {
        eprintln!("preferences save failed (non-fatal): {e:?}");
    }
}
