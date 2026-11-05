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
//! * **Search** (M8.4) — armed by `s` in Browse. `j`/`k` and any other
//!   printable char *type into the query buffer* (they never navigate here,
//!   so no context ambiguity); `↑`/`↓`/`Home`/`End` move the result + tag
//!   suggestion selections in lockstep; `Tab` injects the highlighted tag
//!   (alias-first, `alias → canonical`) onto the trailing `tag:` token;
//!   `Enter` opens the selected result in **Read**; `Esc` back to Browse,
//!   `q`/`C-c` quit the TUI.
//!
//! The binding tables:
//!
//! | key                        | Browse                    | Filter input            | Read                        | Search                    |
//! |----------------------------|---------------------------|-------------------------|-----------------------------|---------------------------|
//! | `↑`/`↓` arrows              | move selection            | move selection (live)   | scroll body ∓ 1 line        | move result + tag sel     |
//! | `Home`/`PgUp`, `End`/`PgDn`| jump to top/bottom        | jump top/bottom (live)  | jump a viewport             | jump result + tag sel     |
//! | `j` / `k`                   | move selection            | append to filter        | scroll body ∓ 1 line        | *(typed into the query)*  |
//! | `g` / `G`                  | *(no-op)*                 | *(no-op)*               | jump to top / bottom        | *(typed into the query)*  |
//! | `Enter`                    | open the selected page    | adopt (open selected)   | *(no-op)*                   | open the selected result  |
//! | `/`                        | arm the filter input      | commit (disarm)         | *(no-op)*                   | *(typed into the query)*  |
//! | printable char             | move selection (`j`/`k`)  | append to filter        | *(no-op)*                   | *(typed into the query)*  |
//! | `Backspace`                | *(no-op)*                 | pop a char              | *(no-op)*                   | pop a char from the query |
//! | `Tab`                      | *(no-op)*                 | *(no-op)*               | *(no-op)*                   | inject highlighted tag    |
//! | `a`                        | open the alias manager    | *(no-op)*               | open the alias manager      | *(typed into the query)*  |
//! | `r`                        | open the roots manager    | *(no-op)*               | open the roots manager      | *(typed into the query)*  |
//! | `s`                        | open the Search mode      | *(no-op)*               | *(no-op)*                   | *(typed into the query)*  |
//! | `Esc` / `q` / `C-c`        | quit / clear filter      | clear filter / quit    | pop to Browse / quit        | back to Browse / quit     |
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
        crate::tui::Mode::Search(_) => handle_search(app, ev),
        crate::tui::Mode::Links(_) => handle_links(app, ev),
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
        // `e` opens the selected page's metadata in Edit mode (M8.2).
        KeyCode::Char('e') => {
            if let Err((msg, _)) = app.open_edit() {
                app.set_error(msg);
            } else {
                app.set_info("edit metadata — ↑/↓ field · Enter open field · v $EDITOR · Esc back");
            }
            false
        }
        // `v` opens the selected page in the user's $EDITOR (or $VISUAL or vi).
        #[cfg(feature = "editor")]
        KeyCode::Char('v') => {
            if let Err((msg, _)) = app.open_in_editor() {
                app.set_error(msg);
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
        // `s` opens the Search mode (M8.4): full FTS5 power search with
        // inline tag completion.
        KeyCode::Char('s') => {
            if let Err((msg, _)) = app.open_search() {
                app.set_error(msg);
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
    // If find is armed, all keys go to the live-input handler (the same
    // idiom as `filter_active` at the `handle_event` level, but scoped to
    // Read mode so other modes' keybinds are unaffected).
    if app.find_armed() {
        return handle_find_input(app, ev);
    }
    match ev.code {
        KeyCode::Char('q') => true,
        KeyCode::Char('c') if ev.modifiers.contains(KeyModifiers::CONTROL) => true,
        // Esc pops back to where the read was opened from (Browse, or the
        // search that armed it — query + results intact). `close_read` owns
        // the status-bar note, so we must not overwrite it here.
        KeyCode::Esc => {
            app.close_read();
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

        // `e` opens the selected page's metadata in Edit mode (M8.2):
        // the user is already *reading* this page in Read mode, so they
        // know the id / context; `e` is the "edit this" shortcut.
        KeyCode::Char('e') => {
            if let Err((msg, _)) = app.open_edit() {
                app.set_error(msg);
            } else {
                app.set_info("edit metadata — ↑/↓ field · Enter open field · v $EDITOR · Esc back");
            }
            false
        }
        // `v` opens this page in the user's $EDITOR (or $VISUAL or vi).
        #[cfg(feature = "editor")]
        KeyCode::Char('v') => {
            if let Err((msg, _)) = app.open_in_editor() {
                app.set_error(msg);
            }
            false
        }
        // `f` arms the reader-find (the `/` filter idiom, scoped to Read):
        // type a string to highlight all occurrences; ↑/↓ to navigate;
        // Esc to close. (j/k/n are typed, not navigation — they're letters
        // the needle may contain.)
        KeyCode::Char('f') => {
            app.find_activate();
            app.find_center_scroll();
            let note = app
                .find_note()
                .unwrap_or_else(|| "find — type a needle · Esc close".to_owned());
            app.set_info(note);
            false
        }
        // `l` opens the **Link-picker** (M8.5) for the page being read:
        // lists every link on the page (wikilink + path) with position,
        // context, and resolution; `Enter` follows a *unique* link, `Esc`
        // pops back to this Read. Pre-selects the link nearest the most
        // recent find hit (if any) so the "I just finished `f`" follow-up
        // path starts at the right place rather than at line 0.
        //
        // **Reachable whether or not find is armed**: with find armed, a
        // dedicated `l` arm in `handle_find_input` (below) also opens the
        // picker — using the currently-highlighted find hit as the anchor
        // — so `f` → navigate → `l` is a one-keystroke follow-through.
        KeyCode::Char('l') => {
            if let Err((msg, _)) = app.open_links() {
                app.set_error(msg);
            }
            false
        }

        _ => false,
    }
}


/// Input handler for the **Link-picker** (M8.5) — the TUI half of the
/// agent-friendly "list links + open one by index" workflow.
///
/// Two sub-states, dispatched by a runtime check on `LinksState::candidates`:
///
/// * **Flat list** (`candidates == None`): a numbered list of every
///   link occurrence in the page body.
///     - `↑` / `↓` / `k` / `j` — move the cursor over the link list.
///     - `Home` / `End` — jump to first / last.
///     - `Enter` — open the selected link.
///       - **unique** → open the target page's Read (the existing
///         `open_linked` pop-back via `read_return`).
///       - **ambiguous** → open the inline **candidate picker** sub-state
///         on the same `LinksState` (no new `Mode` variant; the sub-state
///         holds the candidate options + its own cursor).
///       - **unresolved** → no-op with a status note ("dead link").
///     - `x` / `Esc` — drop the picker (back to the Read that armed it).
///     - `q` / `C-c` — quit the TUI (consistent with every other mode).
/// * **Candidate picker** (`candidates == Some`, the sub-state):
///     - `↑` / `↓` / `k` / `j` — move across the candidates (wrap-around).
///     - `Home` / `End` — jump to first / last candidate.
///     - `Enter` — open the highlighted candidate's page (same pop-back
///       as the unique-hit path — the caller routes to
///       `links_open_selected`, which handles both branches).
///     - `x` / `Esc` — pop back into the flat list at the entry the
///       candidate was opened from (the cursor is restored to
///       `entry_idx` by `close_links`).
///     - `q` / `C-c` — quit the TUI.
///
/// No character-typed input (both views are positional), so no
/// `filter_active`-style top-level check.
fn handle_links(app: &mut App, ev: KeyEvent) -> bool {
    // (0) Quit — consistent with every mode.
    if matches!(ev.code, KeyCode::Char('q'))
        || (matches!(ev.code, KeyCode::Char('c'))
            && ev.modifiers.contains(KeyModifiers::CONTROL))
    {
        return true;
    }

    // (1) Candidate picker overlay (the sub-state). When active, the keys
    // dispatch to the overlay's own cursor / actions — the flat list's
    // cursor is *not* touched (so `Esc` can pop back into the flat list
    // with the cursor still on the entry the candidate was opened from —
    // see `close_links`'s `entry_idx` restore).
    let overlay_active = matches!(&app.mode, crate::tui::Mode::Links(l) if l.candidates.is_some());
    if overlay_active {
        return match ev.code {
            KeyCode::Up | KeyCode::Char('k') => {
                app.move_link_candidate(false); // wrap-around (last → first)
                false
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.move_link_candidate(true); // wrap-around (first → last)
                false
            }
            KeyCode::Home => {
                app.jump_link_candidate(true);
                false
            }
            KeyCode::End => {
                app.jump_link_candidate(false);
                false
            }
            // `Enter` — open the highlighted candidate's page. Routed
            // through `links_open_selected` (which handles both the
            // flat-list and the overlay branches — see the method doc),
            // so the pop-back mechanics are shared.
            KeyCode::Enter => match app.links_open_selected() {
                Ok(()) => false,
                Err((msg, _)) => {
                    app.set_error(msg);
                    false
                }
            },
            // Pop back to the flat list at the entry the candidate was
            // opened from (`close_links` restores the flat-list cursor to
            // `entry_idx`).
            KeyCode::Char('x') | KeyCode::Esc => {
                app.close_links();
                false
            }
            _ => false,
        };
    }

    // (2) Flat link list (the default view).
    match ev.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if let crate::tui::Mode::Links(l) = &mut app.mode {
                if l.cursor > 0 {
                    l.cursor -= 1;
                }
            }
            app.set_links_note();
            false
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let crate::tui::Mode::Links(l) = &mut app.mode {
                if l.cursor + 1 < l.links.len() {
                    l.cursor += 1;
                }
            }
            app.set_links_note();
            false
        }
        KeyCode::Home => {
            if let crate::tui::Mode::Links(l) = &mut app.mode {
                l.cursor = 0;
            }
            app.set_links_note();
            false
        }
        KeyCode::End => {
            if let crate::tui::Mode::Links(l) = &mut app.mode {
                l.cursor = l.links.len().saturating_sub(1);
            }
            app.set_links_note();
            false
        }

        // `Enter` — open the selected entry. Dispatches by resolution:
        // unique → open; ambiguous → open the inline candidate picker;
        // unresolved → no-op. All three branches live inside
        // `links_open_selected` so the pop-back is one place, not
        // three.
        KeyCode::Enter => match app.links_open_selected() {
            Ok(()) => false,
            Err((msg, _)) => {
                app.set_error(msg);
                false
            }
        },

        // Drop the picker (back to the Read the picker was opened from).
        // `close_links` owns the status note.
        KeyCode::Char('x') | KeyCode::Esc => {
            app.close_links();
            false
        }

        _ => false,
    }
}

/// Live-input handler for the reader-find (the `f` key in Read mode, the
/// `/` filter idiom scoped to Read). All keys are captured here: printable
/// chars (including `j`/`k`/`n`) append to the needle, `Backspace` pops,
/// `↑`/`↓` walk the hit list (each move recenters the viewport), `Esc`
/// closes find (keeps the scroll position), `q`/`C-c` quit the TUI.
///
/// Mirrors the `handle_filter_input` structure: the match arms return
/// `bool` (quit?) and the arms that mutate state set the status note.
/// Navigation is arrows-only so that any letter the search term contains
/// can be typed verbatim.
fn handle_find_input(app: &mut App, ev: KeyEvent) -> bool {
    match ev.code {
        KeyCode::Char('q') => true,
        KeyCode::Char('c') if ev.modifiers.contains(KeyModifiers::CONTROL) => true,

        // Close the find session (drop the highlight; keep scroll).
        KeyCode::Esc => {
            app.find_close();
            app.set_info("find closed — scroll position kept");
            false
        }

        // Navigate the hit list (each move recenters the viewport). Arrows
        // only: `j`/`k`/`n`/`N` are *also* letters the user may want to TYPE
        // into the needle, so they fall through to the generic `Char(c)` arm
        // below (mirrors the `/` filter idiom — navigation never captures a
        // printable that the search term might contain).
        KeyCode::Up => {
            app.find_prev();
            let note = app.find_note().unwrap_or_default();
            app.set_info(note);
            false
        }
        KeyCode::Down => {
            app.find_next();
            let note = app.find_note().unwrap_or_default();
            app.set_info(note);
            false
        }

        // Type / delete the needle. `l` is special: it opens the
        // Link-picker (M8.5) anchored on the currently-highlighted find
        // hit — the "I just finished `f`, now follow the closest link"
        // fast path. `l` is a rare needle character, and the design
        // (user constraint #2) is that the picker *pre-selects nearest* on
        // find-armed entry, so it must NOT be typed into the needle here.
        KeyCode::Char('l') => {
            if let Err((msg, _)) = app.open_links() {
                app.set_error(msg);
            }
            false
        }
        KeyCode::Backspace => {
            app.find_backspace();
            let note = app.find_note().unwrap_or_default();
            app.set_info(note);
            false
        }
        KeyCode::Char(c) => {
            app.find_input(c);
            let note = app.find_note().unwrap_or_default();
            app.set_info(note);
            false
        }

        _ => false,
    }
}

/// The Edit-metadata key-binding table (M8.2): a *two-stage* state machine
/// mirroring the zsh `type → live-filter → arrows-select → Enter-adopts`
/// idiom exactly.
///
/// * **Picker** — the field list (title / description / tags /
///   search_required_phrase). `↑`/`↓` move the highlight; `Enter` enters **Value**
///   for that field. The manager keys live *only* here: `q` quits the TUI,
///   `a` opens the alias manager, `r` the roots manager (each also
///   reachable from Browse / Read). **No letter types here** — the user
///   hasn't chosen a field yet.
///
/// * **Value** — the value buffer for the chosen field. **Every letter is
///   a literal** — `q`, `a`, `r`, `x`, `e`, `v` all go straight into the
///   buffer, so "security", "query", "data", "tax" all type. `Tab` is a
///   *pure* focus toggle between the buffer and the live candidate list;
///   typing always goes to the buffer and re-narrows the list (zsh
///   idiom). `↑`/`↓` move the candidate when the list is focused, else
///   move to the next field. `Backspace` pops the buffer (or, on an empty
///   list-field buffer, drops the list's tail item). `Enter` — with the
///   list focused — adopts the highlighted candidate + commits + stays on
///   the field; with the buffer focused, just commits. `Esc` pops to the
///   Picker; `C-c` quits the TUI.
///
/// Order matters: `C-c` + `Esc` are the only *common* keys; the manager
/// keys (`q`/`a`/`r`) are scoped to the **Picker** stage (so they can't
/// steal letters in a Value buffer); and the `Char(c)` catch-all is always
/// the last arm in the Value dispatch.
fn handle_edit(app: &mut App, ev: KeyEvent) -> bool {
    let stage = match &app.mode {
        crate::tui::Mode::Edit(e) => e.stage,
        // `handle_edit` is only called when `app.mode` is `Edit` — this
        // branch is total-on-defensive (an invariant-violating call), kept
        // so the match arm is exhaustive.
        _ => return false,
    };

    // ---- Keys with the same meaning in every stage ----------------------
    if ev.modifiers.contains(KeyModifiers::CONTROL) && matches!(ev.code, KeyCode::Char('c')) {
        return true; // C-c quits
    }

    // `Esc` pops one stage:
    //   Picker → Browse (the user leaves edit entirely)
    //   Value  → (dispatched by `esc_in_value` below):
    //            - in Selection sub-mode → back to Buffer (stay in Value)
    //            - in Buffer mode       → back to Picker
    if ev.code == KeyCode::Esc {
        match stage {
            crate::tui::EditStage::Picker => app.close_edit(),
            crate::tui::EditStage::Value => app.esc_in_value(),
        }
        return false;
    }

    // ---- Stage-specific handlers -----------------------------------------
    match stage {
        // **Picker** — the field list, the *deliberate first step*. Here and
        // only here do the single-letter manager keys fire: `q` quit,
        // `a` alias, `r` roots. `↑/↓/j/k` move the cursor among the fields;
        // `Enter` enters **Value** for the current field. Typing (other
        // than those three) is a no-op — no field is chosen yet.
        crate::tui::EditStage::Picker => match ev.code {
            KeyCode::Char('q') => true,
            KeyCode::Char('a') => {
                if let Err((msg, _)) = app.open_alias() {
                    app.set_error(msg);
                } else {
                    app.set_info("alias — Tab list/form · Enter add · x remove · Esc back");
                }
                false
            }
            KeyCode::Char('r') => {
                if let Err((msg, _)) = app.open_roots() {
                    app.set_error(msg);
                } else {
                    app.set_info("roots — Tab list/form · Enter add · x drop · Esc back");
                }
                false
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.picker_move(false);
                false
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.picker_move(true);
                false
            }
            KeyCode::Enter => {
                app.enter_editor();
                false
            }
            KeyCode::Tab => {
                app.set_info("Picker — Enter to enter a field (↑/↓ · Tab · Enter are the field's keys, once it's open)");
                false
            }
            _ => false, // stray input; the status bar already shows the hint
        },
        // **Value** — the value buffer. Every character (including `q` /
        // `a` / `r` / `x` / `e` / `v`) types straight in — the single-letter
        // manager keys are *only* in the stage above. The *special* keys
        // here are now the full zsh-style editor set:
        //   - **scalar** (title / description): `←/→` move the *text cursor*
        //     (insert / delete at a mid-buffer position), `Backspace`
        //     deletes the char immediately before the cursor.
        //   - **list** (tags / search_required_phrase): `←/→` enter *Selection mode*
        //     (a sub-mode that walks the existing items + a trailing
        //     `<new>`); in Selection mode, `←/→/↑/↓` move the highlight,
        //     `Backspace`/`Del` remove the highlighted item, `Enter`
        //     either loads the item into the buffer (so the user can
        //     refine it in place) or exits to Buffer. In Buffer mode,
        //     `↑/↓` pick a candidate, `Tab` autocompletes, and `Enter`
        //     commits. `Esc` always "pops back one state": Selection
        //     → Buffer, Buffer → Picker. `C-c` quits.
        crate::tui::EditStage::Value => match ev.code {
            // **Text cursor** (scalar) / **enter Selection** (list). Both
            // route through `move_buffer_cursor` (which dispatches on the
            // field's sub-mode).
            KeyCode::Left => {
                app.move_buffer_cursor(false);
                false
            }
            KeyCode::Right => {
                app.move_buffer_cursor(true);
                false
            }
            // **Candidates** (Buffer mode, list) / **Selection highlight**
            // (Selection mode, list). Same key, different job —
            // `move_suggestion` dispatches on `in_list_select`.
            KeyCode::Up | KeyCode::Char('k') => {
                app.move_suggestion(false);
                false
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.move_suggestion(true);
                false
            }
            // **Autocomplete**: fill the buffer with the highlighted
            // candidate (a Buffer-mode-only key — in Selection mode it's a
            // no-op: there is no candidate list to pull from).
            KeyCode::Tab => {
                app.autocomplete();
                false
            }
            // **Enter**: commit (Buffer / scalar) or load/exits (Selection).
            // Dispatches through `enter_action`, which snapshots `in_sel`
            // and routes to the right job.
            KeyCode::Enter => {
                if let Err((msg, _)) = app.enter_action() {
                    app.set_error(msg);
                }
                false
            }
            // **Backspace / Del**: remove.
            //   - scalar: pop a char before the text cursor.
            //   - list, Buffer mode: pop a char, or drop the list tail
            //     (when the buffer is empty).
            //   - list, Selection mode: remove the *highlighted* item.
            // All three go through `remove_or_pop`.
            KeyCode::Backspace => {
                app.remove_or_pop();
                false
            }
            KeyCode::Delete => {
                app.remove_or_pop();
                false
            }
            // *Any* other character types straight into the buffer (or
            // exits Selection mode and appends to the cached buffer).
            KeyCode::Char(c) => {
                app.edit_type(c);
                false
            }
        _ => false,
    }
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
/// `/` commits (disarms). `Esc` clears. `q` quits. Selection navigation
/// (`↑/↓`/`Home`/`End`) and `Enter`-to-open work *live*, so a row can be
/// picked while the filter is armed — the zsh `type → arrows-select →
/// Enter-adopts` idiom. Arrow/Home/End are unambiguous here (they are never
/// literal filter text), so they are safe to steal from the `printable char`
/// arm.
fn handle_filter_input(app: &mut App, ev: KeyEvent) -> bool {
    match ev.code {
        // Move the selection *live* against the current filter result. The
        // filter stays armed (so the user can keep refining after selecting);
        // only `Enter` adopts (disarms + opens).
        KeyCode::Up | KeyCode::Home | KeyCode::PageUp => {
            let before = app.selected;
            app.move_selection(-1);
            if app.selected != before {
                app.apply_filter();
                app.set_info(filter_edit_note(app));
            }
            false
        }
        KeyCode::Down | KeyCode::End | KeyCode::PageDown => {
            let before = app.selected;
            app.move_selection(1);
            if app.selected != before {
                app.apply_filter();
                app.set_info(filter_edit_note(app));
            }
            false
        }
        KeyCode::Enter => {
            // Adopt: commit (disarm) then open the currently-selected row —
            // the whole "select while filtering" promise in one keystroke.
            app.filter_active = false;
            app.apply_filter();
            if let Err((msg, _)) = app.open_read() {
                app.set_error(msg);
            } else {
                app.set_info("open page — scroll with ↑↓/jk, PgUp/PgDn, g/G · Esc back");
            }
            false
        }
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

/// The **Search** mode's key-binding table (M8.4):
///
/// | key           | effect                                              |
/// |---------------|-----------------------------------------------------|
/// | `q` / `C-c`   | quit the TUI                                        |
/// | `Esc`         | back to Browse (selection preserved)                |
/// | `↑`/`↓`       | move result selection **+** tag-suggestion selection|
/// | `j`/`k`       | type into the query buffer                          |
/// | any printable | type into the query buffer                          |
/// | `Backspace`   | pop a char from the query buffer                    |
/// | `Tab`         | inject the highlighted tag suggestion into the query|
/// | `Enter`       | open the selected search result in Read mode        |
///
/// The two panes (results + tag suggestions) are navigated in *lockstep*
/// — Up/Down bumps both cursors. This is the Figma-command-palette idiom
/// for "primary list + secondary completion list" and keeps the
/// navigation rules trivial to state: arrows always move, letters always
/// type, Tab always injects.
fn handle_search(app: &mut App, ev: KeyEvent) -> bool {
    match ev.code {
        KeyCode::Char('q') => true,
        KeyCode::Char('c') if ev.modifiers.contains(KeyModifiers::CONTROL) => true,
        KeyCode::Esc => {
            app.close_search();
            false
        }
        KeyCode::Up => {
            app.search_move(-1);
            app.search_tag_move(-1);
            false
        }
        KeyCode::Down => {
            app.search_move(1);
            app.search_tag_move(1);
            false
        }
        KeyCode::Home => {
            app.search_tag_move(-10_000); // clamp to top of tag list
            if let crate::tui::Mode::Search(s) = &mut app.mode {
                if let Some(r) = &mut s.results {
                    if !r.is_empty() {
                        s.sel = 0;
                    }
                }
            }
            false
        }
        KeyCode::End => {
            app.search_tag_move(10_000); // clamp to bottom of tag list
            if let crate::tui::Mode::Search(s) = &mut app.mode {
                if let Some(r) = &mut s.results {
                    if !r.is_empty() {
                        s.sel = r.len() - 1;
                    }
                }
            }
            false
        }
        KeyCode::Tab => {
            // If there are tag suggestions, Tab injects the highlighted
            // one; otherwise Tab is a no-op (no tag context to complete
            // — the user can still type `tag:` to start one).
            let has_sugs = matches!(&app.mode, crate::tui::Mode::Search(s) if !s.tag_sugs.is_empty());
            if has_sugs {
                app.search_inject_tag();
            } else {
                app.set_info("no tag at the end of the query — type `tag:` first");
            }
            false
        }
        KeyCode::Enter => {
            if let Err((msg, _)) = app.search_open() {
                app.set_error(msg);
            }
            false
        }
        KeyCode::Backspace => {
            app.search_backspace();
            false
        }
        KeyCode::Char(c) => {
            app.search_type(c);
            false
        }
        _ => false,
    }
}
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

    /// While the filter is armed, arrow keys move the selection *live*
    /// (not typed into the filter), whereas `j`/`k` and other printables are
    /// *typed* into the filter. Regression: arrows used to be swallowed as
    /// `(no-op)` and the user could type but not select.
    #[test]
    fn filter_armed_arrows_navigate_and_jk_are_typed() {
        let db = crate::tui::Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // Two pages, both matching the filter "alpha" — so the filter
        // narrows to exactly these two and both are selectable. (Seed *before*
        // `App::new`, which takes ownership of `db`.)
        for name in ["alpha1", "alpha2"] {
            let abs = format!("/root/{name}.md");
            let id = format!("note:{name}");
            db.conn
                .execute(
                    "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase,
                                        mtime_ns, content_hash)
                     VALUES (?1, ?2, 'note', ?3, '/root', ?4, NULL, 0, 'x')",
                    rusqlite::params![abs, id, name, format!("{name}.md")],
                )
                .unwrap();
        }
        let mut app = App::new(db).unwrap();
        app.apply_filter();
        assert_eq!(app.view.len(), 2, "two alpha* pages seeded");

        // Arm the filter, type a narrowing prefix.
        assert!(!handle_event(&mut app, key('/')));
        assert!(app.filter_active, "filter armed");
        for c in "alpha".chars() {
            assert!(!handle_event(&mut app, key(c)));
        }
        assert_eq!(app.filter_text, "alpha", "printable chars typed into the filter");
        assert_eq!(app.view.len(), 2, "both alpha pages still match");

        // `j` must be *typed* (not a nav step) — appends to the filter and
        // narrows the (empty) result set.
        assert!(!handle_event(&mut app, key('j')));
        assert_eq!(app.filter_text, "alphaj", "j is typed into the filter, not consumed for nav");
        assert_eq!(app.view.len(), 0, "'alphaj' matches none");
        app.filter_text.pop(); // "alpha"

        // Arrows are navigation (unambiguous), so they move the selection
        // and do NOT append to the filter.
        app.apply_filter();
        let start = app.selected;
        assert!(!handle_event(&mut app, key_code_down()));
        assert_eq!(app.selected, start.wrapping_add(1).min(1), "Down moves selection");
        assert_eq!(app.filter_text, "alpha", "arrow did not append to the filter");
        assert!(!handle_event(&mut app, key_code_up()));
        assert_eq!(app.selected, 0, "Up moves selection back to row 0");
        assert_eq!(app.filter_text, "alpha", "arrow did not append to the filter");

        // Enter adopts: disarms the filter and opens the selected page.
        assert!(!handle_event(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(!app.filter_active, "Enter adopts (disarms the filter)");
        assert!(
            matches!(&app.mode, crate::tui::Mode::Read(_)),
            "Enter opens the selected page for reading"
        );
    }

    /// While the reader-find is armed, `j`/`k`/`n`/`N` are *typed* into the
    /// needle (they're letters a search term may contain), whereas only
    /// `↑`/`↓` navigate the hit list. Regression: `j`, `k`, and `n` used to
    /// be captured for prev/next navigation *before* the generic typing arm,
    /// so a needle like "jkn" could never be typed — the very bug reported.
    #[test]
    fn find_armed_jkn_are_typed_and_arrows_navigate() {
        let db = crate::tui::Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // A body with the needle "jkn" appearing on two lines, so that
        // navigation has ≥2 hits to walk between.
        let body = "intro\njkn on line one\nmiddle\njkn on line two\ntail";
        db.conn
            .execute(
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase, mtime_ns, content_hash)
                 VALUES ('/root/f.md', 'note:jknav', 'note', 'JK Nav', '/root', 'f.md', NULL, 0, 'x')",
                rusqlite::params![],
            )
            .unwrap();
        let mut app = App::new(db).unwrap();
        app.selected = app.view.iter().position(|r| r.id == "note:jknav").unwrap();
        app.open_read().unwrap();
        let rs = match &mut app.mode {
            crate::tui::Mode::Read(r) => r,
            _ => panic!(),
        };
        rs.body = body.to_string();
        rs.scroll_top = 0;

        // Arm find.
        app.find_activate();
        assert!(app.find_armed(), "find armed");

        // Type the whole needle "jkn": every char (incl. j/k/n) must be
        // appended verbatim, NOT consumed as navigation.
        for c in "jkn".chars() {
            assert!(!handle_event(&mut app, key(c)));
        }
        {
            let fs = match &app.mode {
                crate::tui::Mode::Read(r) => r.find.as_ref().expect("find state"),
                _ => panic!(),
            };
            assert_eq!(fs.needle, "jkn", "'jkn' typed verbatim (j/k/n not eaten for nav)");
            assert_eq!(fs.matches.len(), 2, "two 'jkn' occurrences");
            assert_eq!(fs.cursor, 0, "cursor on the first hit");
        }

        // Arrows are the *only* navigation: Down advances the hit, and the
        // needle is left untouched.
        assert!(!handle_event(&mut app, key_code_down()));
        {
            let fs = match &app.mode {
                crate::tui::Mode::Read(r) => r.find.as_ref().unwrap(),
                _ => panic!(),
            };
            assert_eq!(fs.cursor, 1, "Down advances to hit 2");
            assert_eq!(fs.needle, "jkn", "arrow did not append to the needle");
        }
        assert!(!handle_event(&mut app, key_code_up()));
        {
            let fs = match &app.mode {
                crate::tui::Mode::Read(r) => r.find.as_ref().unwrap(),
                _ => panic!(),
            };
            assert_eq!(fs.cursor, 0, "Up wraps back to hit 1");
            assert_eq!(fs.needle, "jkn", "arrow did not append to the needle");
        }

        // n is typed (not navigation): appends to the needle.
        assert!(!handle_event(&mut app, key('n')));
        {
            let fs = match &app.mode {
                crate::tui::Mode::Read(r) => r.find.as_ref().unwrap(),
                _ => panic!(),
            };
            assert_eq!(fs.needle, "jknn", "'n' is typed verbatim -> 'jknn'");
        }
    }

    /// While find is armed (Read mode), the dedicated `l` arm in
    /// `handle_find_input` (M8.5) must be captured and open the
    /// Link-picker — NOT typed into the needle (the design constraint
    /// that `l` is a one-keystroke follow-through: `f` → navigate → `l`).
    /// Regression: a generic `Char(c)` arm that pre-dates the `l` arm
    /// would have swallowed the character and the picker would never
    /// have pre-selected the nearest link.
    #[test]
    fn find_armed_l_opens_picker_not_typed() {
        let db = crate::tui::Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        let body = String::from("first see [[b]]\nanchor line\nsecond\n[[c]] last\n");
        db.conn
            .execute(
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase, body, frontmatter_text, mtime_ns, content_hash)
                 VALUES ('/root/l.md', 'note:lk', 'note', 'L Test', '/root', 'l.md', NULL, ?1, '---', 0, 'x')",
                rusqlite::params![body],
            )
            .unwrap();
        for (name, title) in [("b", "LB"), ("c", "LC")] {
            db.conn
                .execute(
                    "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase, body, frontmatter_text, mtime_ns, content_hash)
                     VALUES (?1, ?2, 'note', ?3, '/root', ?4, NULL, 'x body\n', '---', 0, 'x')",
                    rusqlite::params![
                        format!("/root/{name}.md"),
                        format!("note:{name}"),
                        title,
                        format!("{name}.md"),
                    ],
                )
                .unwrap();
        }
        let mut app = App::new(db).unwrap();
        app.selected = app.view.iter().position(|r| r.id == "note:lk").unwrap();
        app.open_read().unwrap();

        // Arm find with a needle that matches one specific line,
        // giving us a known anchor.
        app.find_activate();
        for c in "anchor".chars() {
            assert!(!handle_event(&mut app, key(c)));
        }
        {
            let fs = match &app.mode {
                crate::tui::Mode::Read(r) => r.find.as_ref().expect("find state"),
                _ => panic!(),
            };
            assert_eq!(fs.needle, "anchor", "needle 'anchor' before l");
            assert_eq!(fs.matches.len(), 1);
            assert_eq!(fs.cursor, 0);
        }

        // Now `l` must open the picker, not append to the needle.
        assert!(!handle_event(&mut app, key('l')));
        assert!(
            matches!(app.mode, crate::tui::Mode::Links(_)),
            "'l' while find armed opens the Link-picker"
        );
        // The needle on the parked Read must still be "anchor"
        // (NOT "anchorl", proving the capture arm ran first).
        // The parked state is `links_return` (pop target on Esc).
        if let Some(rs) = &app.links_return {
            let fs = rs.find.as_ref().expect("find still armed in parked Read");
            assert_eq!(
                fs.needle, "anchor",
                "'l' was NOT typed into the needle"
            );
        } else {
            panic!("open_links must park the source Read so Esc can return to it");
        }
        // The picker cursor must reflect the find anchor (line 1) — the
        // closest link is line 0 ([[b]]) by `|line-1|` tiebreak with line 3.
        if let crate::tui::Mode::Links(l) = &app.mode {
            assert_eq!(l.links.len(), 2, "two wiki links on the page");
            // |0-1|=1, |3-1|=2 → index 0 (line 0) is nearest.
            assert_eq!(l.cursor, 0, "cursor pre-selected at link closest to anchor (line 1)");
        }
    }

    /// `l` NOT in find-armed Read: the same key in plain Read also
    /// opens the picker (anchor = None → cursor 0). Guards against a
    /// refactor that scopes `l` only to the find-armed branch.
    #[test]
    fn read_armed_l_opens_picker_without_find() {
        let db = crate::tui::Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // One `execute` per page (rusqlite's `execute` is single-statement).
        let body = String::from("see [[q]]\nand [[r]]\n");
        db.conn
            .execute(
                "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase, body, frontmatter_text, mtime_ns, content_hash)
                 VALUES ('/root/p.md', 'note:rl', 'note', 'R L', '/root', 'p.md', NULL, ?1, '---', 0, 'x')",
                rusqlite::params![body],
            )
            .unwrap();
        for (name, title) in [("q", "LQ"), ("r", "Lr")] {
            db.conn
                .execute(
                    "INSERT INTO pages (abs_path, id, type, title, root, rel_path, search_required_phrase, body, frontmatter_text, mtime_ns, content_hash)
                     VALUES (?1, ?2, 'note', ?3, '/root', ?4, NULL, 'x body\n', '---', 0, 'x')",
                    rusqlite::params![
                        format!("/root/{name}.md"),
                        format!("note:{name}"),
                        title,
                        format!("{name}.md"),
                    ],
                )
                .unwrap();
        }
        let mut app = App::new(db).unwrap();
        app.selected = app.view.iter().position(|r| r.id == "note:rl").unwrap();
        app.open_read().unwrap();
        // Find NOT armed.
        assert!(!app.find_armed());
        assert!(!handle_event(&mut app, key('l')));
        assert!(
            matches!(app.mode, crate::tui::Mode::Links(_)),
            "'l' in plain Read opens the Link-picker"
        );
        if let crate::tui::Mode::Links(l) = &app.mode {
            assert_eq!(l.cursor, 0, "no anchor → cursor 0 (first link)");
        }
    }

    fn key_code_down() -> KeyEvent {
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
    }
    fn key_code_up() -> KeyEvent {
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)
    }

    /// The roots add form is a *path* input — `j`/`k` are common in paths
    /// and must be typed when the form is focused (regression: a typed `j` and
    /// `k` once caused those chars to be dropped, e.g. `...-jk-root` landing as
    /// `...-root`).
    #[test]
    fn roots_form_types_jk_in_path() {
        let db = crate::tui::Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // W7: derive the path from the platform temp dir so the test builds
        // and runs on Windows, not just POSIX (where /tmp exists).
        let path = std::env::temp_dir().join("xjk-root").display().to_string();
        std::fs::create_dir_all(&path).unwrap();
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
        let _ = std::fs::remove_dir_all(&path);
    }

    /// The Search mode (M8.4) full loop: `s` arms Search; typing a partial
    /// `open_roots` opens with the right default focus:
    /// - **list** focused when a root exists (so `↑↓`/`jk`/`x` work
    ///   immediately — no `Tab` press needed to start navigating);
    /// - **form** focused when no root exists (there is nothing to select;
    ///   the user is dropped straight into typing a path).
    #[test]
    fn roots_open_picks_the_right_default_focus() {
        // Case 1: empty DB → no root → form focused.
        {
            let db = crate::tui::Db::open_in_memory_test().unwrap();
            db.migrate().unwrap();
            let mut app = App::new(db).unwrap();
            app.open_roots().unwrap();
            match &app.mode {
                crate::tui::Mode::Roots(r) => assert!(
                    !r.focused,
                    "with no roots, the add form is focused so a path can be typed immediately"
                ),
                _ => panic!("expected Roots mode"),
            }
        }
        // Case 2: a real root exists → list focused.
        {
            let db = crate::tui::Db::open_in_memory_test().unwrap();
            db.migrate().unwrap();
            let root = std::env::temp_dir().join("kb_tui_roots_focus");
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("note.md"),
                "---\ntitle: Focus Test\n---\n\nbody\n",
            )
            .unwrap();
            let root_str = root.to_string_lossy().into_owned();
            db.add_root(&root_str, None, false).expect("add_root");
            // `list_roots` reads the `roots` table directly; `App::new` will
            // populate `r.list` from it without a full refresh
            // (`add_root` already registered the row).
            let mut app = App::new(db).unwrap();
            app.open_roots().unwrap();
            match &app.mode {
                crate::tui::Mode::Roots(r) => {
                    assert!(
                        !r.list.is_empty(),
                        "precondition: list has at least one root"
                    );
                    assert!(
                        r.focused,
                        "with roots present, the list is focused (no Tab press needed)"
                    );
                }
                _ => panic!("expected Roots mode"),
            }
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// tag (`tag:se`) populates the suggestion list; `Down` moves the
    /// suggestion cursor; `Tab` injects the highlighted tag into the query
    /// (replacing the `tag:se` partial); `Esc` pops back to Browse.
    /// Exercises the input dispatch for every state transition in one test
    /// (the *rendering* of the two panes is `draw_search`'s concern,
    /// covered by `render`'s own test).
    #[test]
    fn search_full_flow_type_tab_inject_esc_back() {
        let db = crate::tui::Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        // Seed the tag corpus + an alias (so the suggestion list has
        // content to navigate).
        db.conn
            .execute(
                "INSERT INTO page_tags(abs_path, tag) VALUES ('/a.md', 'security'),
                 ('/b.md', 'secret'), ('/c.md', 'llm')",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO tag_aliases(alias, canonical) VALUES ('sec', 'security')",
                [],
            )
            .unwrap();
        let mut app = App::new(db).unwrap();

        // `s` arms Search.
        assert!(!handle_event(&mut app, key('s')));
        assert!(matches!(&app.mode, crate::tui::Mode::Search(_)), "s opens Search");

        // Type the partial tag `tag:se`. Each char routes through
        // `search_type` → `search_commit` → `run_search_bundle`.
        for c in "tag:se".chars() {
            assert!(!handle_event(&mut app, key(c)));
        }
        match &app.mode {
            crate::tui::Mode::Search(s) => {
                assert_eq!(s.query, "tag:se");
                assert!(!s.tag_sugs.is_empty(), "a partial tag must produce suggestions");
                assert!(
                    s.tag_sugs.iter().any(|t| t.canonical == "security"),
                    "the `sec → security` alias must surface for partial `se`"
                );
            }
            _ => panic!("still in Search mode"),
        }

        // `Down` moves the suggestion cursor (arrows navigate both panes).
        // Record the *before* cursor so we can assert it actually advanced.
        let sel_before = match &app.mode {
            crate::tui::Mode::Search(s) => s.tag_sel,
            _ => panic!("still in Search mode"),
        };
        let n_sugs = match &app.mode {
            crate::tui::Mode::Search(s) => s.tag_sugs.len(),
            _ => panic!("still in Search mode"),
        };
        assert!(!handle_event(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        match &app.mode {
            crate::tui::Mode::Search(s) => {
                if n_sugs > 1 {
                    assert_eq!(s.tag_sel, 1, "Down advanced the suggestion cursor");
                } else {
                    assert_eq!(s.tag_sel, sel_before, "single-suggestion list clamps at the end");
                }
            }
            _ => panic!("still in Search mode"),
        }

        // `Tab` injects the highlighted suggestion into the query.
        assert!(!handle_event(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        match &app.mode {
            crate::tui::Mode::Search(s) => {
                // Whatever suggestion was highlighted, the query now ends
                // with a *complete* `tag:` token + trailing space, and the
                // suggestion list is empty (no `tag:` partial at the tail).
                assert!(
                    s.query.ends_with(' '),
                    "the injected query should end with a trailing space"
                );
                assert!(!s.tag_sugs.iter().any(|t| t.canonical.is_empty()), "suggestions rebuilt");
                assert!(
                    s.query.contains("tag:"),
                    "the query must still contain the `tag:` token"
                );
            }
            _ => panic!("still in Search mode"),
        }

        // `Esc` pops back to Browse.
        assert!(!handle_event(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(
            matches!(&app.mode, crate::tui::Mode::Browse),
            "Esc pops back to Browse"
        );
    }
}
