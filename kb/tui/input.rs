//! `kb tui` — the key-binding / event-loop dispatch (M8.1: Browse + Read).
//!
//! This is the *assumed* shell (see the module doc in `mod.rs`): crossterm
//! raw-key events are mapped to state transitions on `App`. The transition
//! logic is plain Rust (and its invariants — selection total + bounded,
//! scroll total + bounded — are pure and testable; see `mod.rs` tests).
//!
//! Contexts (the one thing the binding table is *per*):
//!
//! * **Browse** — the page list. `/` arms the filter input; the rest is
//!   the M8.0 table.
//! * **Filter input** — text-input context: printable chars append, Backspace
//!   pops, `/` commits, `Esc` clears, `q` quits. No arrow-jump here (it
//!   belongs to the list).
//! * **Read** — a page's body + outline. Arrow / `j` `k` scroll the body
//!   line-by-line; `PgUp`/`PgDn` jump a viewport; `G`/`g` / `Home`/`End`
//!   jump top/bottom; `Esc`/`q`/`C-c` pop back to Browse (the *selection*
//!   is preserved, so `Esc` then `j` continues where the user left off).
//!
//! The binding tables:
//!
//! | key                        | Browse                    | Filter input            | Read                        |
//! |----------------------------|---------------------------|-------------------------|-----------------------------|
//! | `↑`/`k`, `↓`/`j`          | move selection            | *(no-op)*               | scroll body ∓ 1 line        |
//! | `Home`/`PgUp`, `End`/`PgDn`| jump to top/bottom        | *(no-op)*               | jump a viewport             |
//! | `g` / `G`                  | *(no-op)*                 | *(no-op)*               | jump to top / bottom        |
//! | `Enter`                    | open the selected page    | commit the filter       | *(no-op)*                   |
//! | `/`                        | arm the filter input      | commit (same key)       | *(no-op)*                   |
//! | printable char             | *(reserved for M8.3)*     | append to filter        | *(no-op)*                   |
//! | `Backspace`                | *(no-op)*                 | pop a char              | *(no-op)*                   |
//! | `a`                        | open the alias manager    | *(no-op)*               | open the alias manager      |
//! | `r`                        | open the roots manager    | *(no-op)*               | open the roots manager      |
//! | `Esc` / `q` / `C-c`        | quit / clear filter      | clear filter / quit    | pop to Browse / quit        |
//!
//! Not-yet-wired keys in a context set a **status** note (rather than being
//! silent) so the user always knows whether a key was seen.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::tui::App;

/// Handle a single crossterm key event against the current `App` state.
///
/// Returns `true` if the TUI should quit (the caller restores the terminal
/// and returns); `false` to keep the event loop going. Every key that does
/// *not* quit updates `app.status` with a short confirmation, so the user
/// always gets feedback — the one way this module reports back.
pub fn handle_event(app: &mut App, ev: KeyEvent) -> bool {
    if !matches!(ev.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return false;
    }
    // First: if the filter input is armed, *all* chars go to the filter
    // (including `/` / `Enter` / `Esc`, which in other contexts are
    // "commit" / "open" / "back"). This is why `filter_active` is checked
    // *before* the per-mode dispatch.
    if app.filter_active {
        return handle_filter_input(app, ev);
    }
    match &app.mode {
        crate::tui::Mode::Browse => handle_browse(app, ev),
        crate::tui::Mode::Read(_) => handle_read(app, ev),
        crate::tui::Mode::Edit(_) => handle_edit(app, ev),
        crate::tui::Mode::Alias(_) => handle_alias(app, ev),
        crate::tui::Mode::Roots(_) => handle_roots(app, ev),
    }
}

fn handle_browse(app: &mut App, ev: KeyEvent) -> bool {
    match ev.code {
        KeyCode::Char('q') => true,
        KeyCode::Esc => {
            // Esc in Browse: clear the filter (if any) and disarm it. If
            // the filter is already empty, Esc quits (the user is pressing
            // "out" of the UI).
            if app.filter_text.is_empty() {
                true
            } else {
                app.filter_text.clear();
                app.filter_active = false;
                app.apply_filter();
                app.set_info("filter cleared");
                false
            }
        }
        KeyCode::Char('c') if ev.modifiers.contains(KeyModifiers::CONTROL) => true,

        KeyCode::Up | KeyCode::Char('k') => {
            app.move_selection(-1);
            app.set_info(app.position_note());
            false
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_selection(1);
            app.set_info(app.position_note());
            false
        }
        KeyCode::Home | KeyCode::PageUp => {
            app.jump_to(false);
            app.set_info(app.position_note());
            false
        }
        KeyCode::End | KeyCode::PageDown => {
            app.jump_to(true);
            app.set_info(app.position_note());
            false
        }

        KeyCode::Enter => {
            if let Err((msg, _)) = app.open_read() {
                app.set_error(msg);
            } else {
                app.set_info("open page — scroll with ↑↓/jk, PgUp/PgDn, g/G · Esc back");
            }
            false
        }
        KeyCode::Char('/') => {
            app.filter_active = true;
            app.set_info("type a filter ( / to commit, Esc to clear, q to quit )");
            false
        }
        // `e` opens the selected page's frontmatter in Edit mode (M8.2).
        KeyCode::Char('e') => {
            if let Err((msg, _)) = app.open_edit() {
                app.set_error(msg);
            } else {
                app.set_info("edit — ↑/j field · type value · Tab autocomplete (lists) · x remove-tail · Enter commit · Esc back");
            }
            false
        }
        // `a` opens the Alias manager (M8.3): manage `tag_aliases`.
        KeyCode::Char('a') => {
            if let Err((msg, _)) = app.open_alias() {
                app.set_error(msg);
            } else {
                app.set_info("alias — Tab list/form · Enter add · x remove · Esc back");
            }
            false
        }
        // `r` opens the Roots manager (M8.3): manage registered roots.
        KeyCode::Char('r') => {
            if let Err((msg, _)) = app.open_roots() {
                app.set_error(msg);
            } else {
                app.set_info("roots — Tab list/form · Enter add · x drop · Esc back");
            }
            false
        }

        _ => {
            // Unknown key: no state change, no status change. Deliberate —
            // the status bar should reflect the last *meaningful* event,
            // not every stray keystroke.
            false
        }
    }
}

fn handle_read(app: &mut App, ev: KeyEvent) -> bool {
    match ev.code {
        KeyCode::Char('q') => true,
        KeyCode::Char('c') if ev.modifiers.contains(KeyModifiers::CONTROL) => true,
        // Esc pops back to Browse (the selection is preserved).
        KeyCode::Esc => {
            app.close_read();
            app.set_info(app.position_note());
            false
        }

        // Body scroll (the outline pane is read-only in M8.1; M8.2 will add
        // `Enter`-on-heading to jump to that section).
        KeyCode::Up | KeyCode::Char('k') => {
            app.scroll_body(-1);
            app.set_info(read_position_note(app));
            false
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.scroll_body(1);
            app.set_info(read_position_note(app));
            false
        }
        KeyCode::PageUp => {
            app.scroll_body(-9);
            app.set_info(read_position_note(app));
            false
        }
        KeyCode::PageDown => {
            app.scroll_body(9);
            app.set_info(read_position_note(app));
            false
        }
        KeyCode::Home | KeyCode::Char('g') => {
            app.scroll_body_top();
            app.set_info(read_position_note(app));
            false
        }
        KeyCode::End | KeyCode::Char('G') => {
            app.scroll_body_bottom();
            app.set_info(read_position_note(app));
            false
        }

        // `a` opens the Alias manager (M8.3).
        KeyCode::Char('a') => {
            if let Err((msg, _)) = app.open_alias() {
                app.set_error(msg);
            } else {
                app.set_info("alias — Tab list/form · Enter add · x remove · Esc back");
            }
            false
        }
        // `r` opens the Roots manager (M8.3).
        KeyCode::Char('r') => {
            if let Err((msg, _)) = app.open_roots() {
                app.set_error(msg);
            } else {
                app.set_info("roots — Tab list/form · Enter add · x drop · Esc back");
            }
            false
        }

        // `e` opens the selected page's frontmatter in Edit mode (M8.2):
        // the user is already *reading* this page in Read mode, so they
        // know the id / context; `e` is the "edit this" shortcut.
        KeyCode::Char('e') => {
            if let Err((msg, _)) = app.open_edit() {
                app.set_error(msg);
            } else {
                app.set_info("edit — ↑/j field · type value · Tab autocomplete (lists) · Enter commit · Esc back");
            }
            false
        }

        _ => false,
    }
}

/// The Edit mode's key-binding table (M8.2): the field navigation
/// (`↑`/`↓`/`j`/`k` cycle the field form), typing (`printable char`
/// appends to the value buffer; `Backspace` pops), tab-autocomplete
/// (`Tab` shows the live suggestions — the *filter* is recomputed in
/// `edit_type`; the suggestion *display* is render-side), the
/// "delete tail" key (`x` on a list field), the commit key (`Enter`),
/// and the back key (`Esc` → Browse, selection preserved; `q` /
/// `C-c` → quit the whole TUI).
///
/// Order matters because a later `Char(c)` catch-all would swallow the
/// single-key bindings; the single keys come first (Rust matches top-down).
fn handle_edit(app: &mut App, ev: KeyEvent) -> bool {
    match ev.code {
        KeyCode::Char('q') => true,
        KeyCode::Char('c') if ev.modifiers.contains(KeyModifiers::CONTROL) => true,
        // Esc pops back to Browse (the selection is preserved).
        KeyCode::Esc => {
            app.close_edit();
            false
        }

        // Field navigation: `↑`/`k` cycles *up* the field list (wraps);
        // `↓`/`j` cycles *down* (wraps).
        KeyCode::Up => {
            app.edit_move_field(false);
            false
        }
        KeyCode::Down => {
            app.edit_move_field(true);
            false
        }
        // The `j`/`k` aliases come *before* the `Char(c)` catch-all.
        KeyCode::Char('k') => {
            app.edit_move_field(false);
            false
        }
        KeyCode::Char('j') => {
            app.edit_move_field(true);
            false
        }

        // `x` on a list field: "delete tail". On a scalar, no-op + hint.
        KeyCode::Char('x') => {
            app.remove_tail_item();
            false
        }

        // `e` is the *open* key in Browse/Read — in Edit it's a
        // "re-enter edit" no-op (the user is already here).
        KeyCode::Char('e') => {
            app.set_info("already in edit mode — Esc back to Browse, q quit");
            false
        }

        // `a` opens the Alias manager (M8.3).
        KeyCode::Char('a') => {
            if let Err((msg, _)) = app.open_alias() {
                app.set_error(msg);
            } else {
                app.set_info("alias — Tab list/form · Enter add · x remove · Esc back");
            }
            false
        }
        // `r` opens the Roots manager (M8.3).
        KeyCode::Char('r') => {
            if let Err((msg, _)) = app.open_roots() {
                app.set_error(msg);
            } else {
                app.set_info("roots — Tab list/form · Enter add · x drop · Esc back");
            }
            false
        }

        // Any other printable is a char to *type* into the value buffer.
        KeyCode::Char(c) => {
            app.edit_type(c);
            false
        }
        KeyCode::Backspace => {
            app.edit_backspace();
            false
        }

        // Tab: no-op on the *state* side — the suggestions are already
        // recomputed on every `edit_type` / `edit_backspace` (the
        // "autocomplete is a pure function of (corpus, buffer)" invariant).
        // The render surface shows them; the user picks by typing (buffer
        // converges to a unique token) or by `x` (delete tail) re-typing.
        KeyCode::Tab => {
            app.set_info("tab — suggestions are live; keep typing to narrow, Enter to commit");
            false
        }

        // Enter: commit the current field's edit to disk + re-index.
        KeyCode::Enter => {
            if let Err((msg, _)) = app.commit_edit() {
                app.set_error(msg);
            }
            false
        }

        _ => false,
    }
}

/// A "line X of Y — <id>" status note for the Read mode. (The *first
/// visible line of the body* — `scroll_top` — is the position, not a
/// character offset, because the body is a line-addressed text.)
fn read_position_note(app: &App) -> String {
    if let crate::tui::Mode::Read(r) = &app.mode {
        let lines = r.body.lines().count();
        format!(
            "reading {} — line {} of {} (body)",
            r.id,
            r.scroll_top + 1,
            lines
        )
    } else {
        String::new()
    }
}

/// The filter input context: each printable char *appends* to
/// `filter_text` and re-runs `apply_filter` (so the list re-filters as the
/// user types — the live-preview UX `/` is usually given). Backspace pops.
/// `/` commits (disarms). `Esc` clears. `q` quits. Nothing else is bound.
fn handle_filter_input(app: &mut App, ev: KeyEvent) -> bool {
    match ev.code {
        KeyCode::Char('/') => {
            // Commit: disarm the input; the filter *stays applied* — the
            // user can refine it by pressing `/` again from Browse.
            app.filter_active = false;
            app.apply_filter();
            let n = app.view.len();
            app.set_info(format!("filtered to {n} page(s) ({}): / to edit, Esc to clear", app.filter_text));
            false
        }
        KeyCode::Esc => {
            // Clear: drop the filter text and re-show all rows.
            app.filter_text.clear();
            app.filter_active = false;
            app.apply_filter();
            app.set_info("filter cleared");
            false
        }
        KeyCode::Char('q') => true,
        KeyCode::Char('c') if ev.modifiers.contains(KeyModifiers::CONTROL) => true,
        KeyCode::Backspace => {
            app.filter_text.pop();
            app.apply_filter();
            app.set_info(filter_edit_note(app));
            false
        }
        KeyCode::Char(c) => {
            app.filter_text.push(c);
            app.apply_filter();
            app.set_info(filter_edit_note(app));
            false
        }
        _ => false,
    }
}

/// The **Alias** manager's key-binding table (M8.3):
///
/// | key                  | form focused (`focused == false`) | list focused (`focused == true`) |
/// |----------------------|-----------------------------------|-----------------------------------|
/// | `↑`/`k`, `↓`/`j`    | *(no-op — form is not a list)*    | move selection ± 1 (clamped)      |
/// | `Tab`                | focus the list                    | focus the form                    |
/// | `Enter`              | submit `input` as an upsert       | *(no-op — use `x` to remove)*     |
/// | `x`                  | *(no-op — arrow to the list first)*| remove the selected alias         |
/// | printable char       | append to `input`                 | *(no-op)*                         |
/// | `Backspace`          | pop a char from `input`           | *(no-op)*                         |
/// | `Esc` / `q` / `C-c`  | pop to Browse / quit / quit       | pop to Browse / quit / quit       |
fn handle_alias(app: &mut App, ev: KeyEvent) -> bool {
    match ev.code {
        KeyCode::Char('q') => true,
        KeyCode::Char('c') if ev.modifiers.contains(KeyModifiers::CONTROL) => true,
        // Esc pops back to Browse (the selection is preserved).
        KeyCode::Esc => {
            app.close_alias();
            false
        }

        // `Tab` toggles between the add form and the list (the "form vs.
        // list" contract — the alias manager is the only place where a
        // single key switches the user between two sub-contexts).
        KeyCode::Tab => {
            app.alias_toggle_focus();
            false
        }

        // `↑`/`↓` always move the list selection (no-op when the form is
        // focused or the list is empty). `j`/`k` move *only* while the list
        // is active — with the form active they fall through to the `Char(c)`
        // arm and are *typed* (aliases can legitimately contain `j`/`k`).
        KeyCode::Up => {
            app.alias_move(-1);
            false
        }
        KeyCode::Down => {
            app.alias_move(1);
            false
        }
        KeyCode::Char('k') if matches!(&app.mode, crate::tui::Mode::Alias(a) if a.focused) => {
            app.alias_move(-1);
            false
        }
        KeyCode::Char('j') if matches!(&app.mode, crate::tui::Mode::Alias(a) if a.focused) => {
            app.alias_move(1);
            false
        }

        // `Enter` submits the add form (no-op when the list is focused —
        // use `x` to remove).
        KeyCode::Enter => {
            // Only the form accepts Enter; the list ignores it (the user
            // should `Tab` back to the form to submit).
            let in_form = matches!(&app.mode, crate::tui::Mode::Alias(a) if !a.focused);
            if in_form {
                if let Err((msg, _)) = app.add_alias() {
                    app.set_error(msg);
                }
            }
            false
        }

        // `x` removes the selected alias — but *only* while the list is
        // active; with the form active it is just a typed character
        // (aliases may legitimately contain `x`).
        KeyCode::Char('x') if matches!(&app.mode, crate::tui::Mode::Alias(a) if a.focused) => {
            if let Err((msg, _)) = app.remove_alias() {
                app.set_error(msg);
            }
            false
        }

        // Typing / backspace — only the form captures them (the list is
        // selection-only, no inline-text editing).
        KeyCode::Char(c) => {
            app.alias_type(c);
            false
        }
        KeyCode::Backspace => {
            app.alias_backspace();
            false
        }
        _ => false,
    }
}

/// The **Roots** manager's key-binding table (M8.3) — the same "form vs.
/// list" contract as the Alias manager, applied to the `roots` registry.
fn handle_roots(app: &mut App, ev: KeyEvent) -> bool {
    match ev.code {
        KeyCode::Char('q') => true,
        KeyCode::Char('c') if ev.modifiers.contains(KeyModifiers::CONTROL) => true,
        KeyCode::Esc => {
            app.close_roots();
            false
        }
        KeyCode::Tab => {
            app.roots_toggle_focus();
            false
        }
        KeyCode::Up => {
            app.roots_move(-1);
            false
        }
        KeyCode::Down => {
            app.roots_move(1);
            false
        }
        KeyCode::Char('k') if matches!(&app.mode, crate::tui::Mode::Roots(r) if r.focused) => {
            app.roots_move(-1);
            false
        }
        KeyCode::Char('j') if matches!(&app.mode, crate::tui::Mode::Roots(r) if r.focused) => {
            app.roots_move(1);
            false
        }
        KeyCode::Enter => {
            let in_form = matches!(&app.mode, crate::tui::Mode::Roots(r) if !r.focused);
            if in_form {
                if let Err((msg, _)) = app.add_root_tui() {
                    app.set_error(msg);
                }
            }
            false
        }
        // `x` drops the selected root — but *only* while the list is
        // active; with the form active it is just a typed character
        // (paths may legitimately contain `x`).
        KeyCode::Char('x') if matches!(&app.mode, crate::tui::Mode::Roots(r) if r.focused) => {
            if let Err((msg, _)) = app.drop_root_tui() {
                app.set_error(msg);
            }
            false
        }
        KeyCode::Char(c) => {
            app.roots_type(c);
            false
        }
        KeyCode::Backspace => {
            app.roots_backspace();
            false
        }
        _ => false,
    }
}

/// A live status while editing the filter: "filter 'abc' → 4 page(s)
/// (type / to commit, Esc to clear)".
fn filter_edit_note(app: &App) -> String {
    if app.filter_text.is_empty() {
        String::from("type a filter (no text yet) — / to commit, Esc to clear, q to quit")
    } else {
        format!(
            "filter '{}' → {} page(s) — / to commit, Esc to clear, q to quit",
            app.filter_text,
            app.view.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// The alias add form must accept `j`/`k` as *typed* characters (they
    /// are reserved for list navigation only while the list is focused).
    /// Regression: `"k8"` used to land as `"8"`.
    #[test]
    fn form_chars_jk_are_typed_not_swallowed() {
        let db = crate::tui::Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        db.conn
            .execute(
                "INSERT INTO tag_aliases(alias, canonical) VALUES ('a', 'A'), ('b', 'B')",
                [],
            )
            .unwrap();
        let mut app = App::new(db).unwrap();
        app.open_alias().unwrap();
        assert!(!matches!(&app.mode, crate::tui::Mode::Alias(a) if a.focused), "form focused by default");
        for c in "jk".chars() {
            assert!(!handle_event(&mut app, key(c)));
        }
        let text = match &app.mode {
            crate::tui::Mode::Alias(a) => a.input.clone(),
            _ => String::new(),
        };
        assert_eq!(text, "jk", "'j'/'k' must be typed into the form, not consumed");

        // …but while the LIST is focused the same keys still navigate.
        app.alias_toggle_focus();
        let before = match &app.mode {
            crate::tui::Mode::Alias(a) => a.selected,
            _ => usize::MAX,
        };
        assert!(!handle_event(&mut app, key('j')));
        let after = match &app.mode {
            crate::tui::Mode::Alias(a) => a.selected,
            _ => usize::MAX,
        };
        assert_eq!(after, (before + 1).min(1), "'j' navigates the list when the list is focused");
    }

    /// The roots add form is a *path* input — `j`/`k` are common in paths
    /// and must be typed when the form is focused (regression: `"/tmp/xjk-root"`
    /// used to land as `"/tmp/x-root"`).
    #[test]
    fn roots_form_types_jk_in_path() {
        let db = crate::tui::Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let path = "/tmp/xjk-root";
        std::fs::create_dir_all(path).unwrap();
        let mut app = App::new(db).unwrap();
        app.open_roots().unwrap();
        for c in path.chars() {
            assert!(!handle_event(&mut app, key(c)));
        }
        let text = match &app.mode {
            crate::tui::Mode::Roots(r) => r.input.clone(),
            _ => String::new(),
        };
        assert_eq!(text, path, "every char of the path, including 'j' and 'k', must be typed");
    }
}
