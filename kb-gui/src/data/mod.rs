//! The data layer: the DB worker thread and its `Cmd`/`Evt` protocol
//! (PLAN-GUI §3.2).
//!
//! The worker is the *only* thread that touches the database. The UI sends
//! `Cmd` values over a channel and consumes `Evt` values off another; it
//! never blocks on disk or SQL. The worker is spawned from the *first*
//! `App::update` after the empty shell has laid out (PLAN-GUI §3.2: "spawned
//! from the first `update` after laying out the empty shell, then detached").
//! Its thread owns its `Db` for the whole session.
//!
//! `data::corpus` holds the G0 slice of the page-list fetch (a port of the
//! `kb browse` row fetch, `kb/cli/browse.rs`): the same SQL, so the corpus
//! shape the GUI renders is the same the CLI answers with — the parity story
//! starts here (PLAN-GUI §3.1: duplication is accepted for now, guarded by
//! parity tests later).

pub mod admin;
pub mod corpus;
pub mod search;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;

use crate::state::{LaunchRoute, PageRow, SearchHit};

/// A command from the UI thread to the DB worker (PLAN-GUI §3.2). G0 wires
/// the startup path (`LoadCorpus`) and shutdown (`Quit`); the rest are
/// reserved for G1–G5 and are part of the protocol now so the shape is
/// stable across milestones (they answer with `Evt::Warn` in G0).
pub enum Cmd {
    /// Open the DB (once, on first use), run migrations, load the corpus
    /// and apply the launch route (reserved: G0 records it, G1+ acts on it).
    LoadCorpus {
        launch: LaunchRoute,
        db: Option<PathBuf>,
    },
    /// Re-walk the registered roots and re-index. Wired in G5 (the
    /// Settings → Data **Refresh** action).
    Refresh,
    /// Wipe the index and re-walk every registered root from scratch
    /// (`kb refresh --full` / `kb_db::refresh::full_rebuild`). Wired in G5
    /// (the Settings → Data **Full rebuild** action; a *full* rebuild is
    /// what the §6.12 table distinguishes from an incremental refresh).
    FullRebuild,
    /// Register a root directory + (optionally) re-index + reload the
    /// corpus. Wired in G1.
    AddRoot(PathBuf),
    /// Unregister a root directory + re-index (so the GC can drop the
    /// pages that were under the dropped root only — pages under another
    /// root survive). Wired in G5 (the Settings → Roots **Remove**
    /// row; PLAN-GUI §6.12 rule: "removing a root is confirmed and
    /// explains that the DB rows disappear on the next refresh").
    DropRoot(PathBuf),
    /// Fetch the registered roots (in *presentation* order,
    /// `seq DESC`, with the walk options) + the per-root page counts.
    /// Wired in G5 (the Settings → Roots table).
    ListRoots,
    /// Add / update a `tag_aliases` row: `(alias, canonical)`. Same
    /// upsert SQL the CLI `kb tag-alias --add` uses (a conflicting
    /// `alias` is *updated*, not rejected). Wired in G5 (Settings →
    /// Tag aliases **add** row).
    AddAlias { alias: String, canonical: String },
    /// Remove a `tag_aliases` row by `alias` (0 or 1 row). Wired in G5
    /// (Settings → Tag aliases **remove** row).
    RemoveAlias { alias: String },
    /// Fetch the full `tag_aliases` table, sorted by `alias`. Wired in
    /// G5 (the Settings → Tag aliases table).
    ListAliases,
    /// Fetch the corpus stats snapshot (page counts, link count, DB
    /// size, …). Wired in G5 (the Settings → Data tab).
    Stats,
    /// Open a single page's body. Reserved (G2).
    #[allow(dead_code)] // reserved for G2
    OpenPage(String),
    /// Run a search query. Optional *scope* = a collection term (the
    /// rail's selected `CollectionKey::Term`), None = unscoped (CLI
    /// strict gate rule). Wired in G1.
    Search {
        query: String,
        scope: Option<String>,
    },
    /// Save a page's body (conflict-check → atomic write → reindex). wired in G2.
    /// `force = true` skips the conflict guard (a mid-session external edit is
    /// overwritten rather than refused) — the §6.10 "Keep mine" resolve step.
    Save {
        abs_path: String,
        expected_disk: Vec<u8>,
        new_bytes: Vec<u8>,
        force: bool,
    },
    /// Create a new note (scaffold frontmatter + body, write, upsert into index).
    /// `directory` is the parent dir (a registered root path). `title` is
    /// required; `page_type`, `tags`, `gates`, `description`, `language`
    /// are optional frontmatter values for the scaffold.
    CreatePage {
        directory: String,
        title: String,
        page_type: String,
        tags: Vec<String>,
        gates: Vec<String>,
        description: Option<String>,
        language: Option<String>,
    },
    /// Save a page's *frontmatter* by applying a batch of verified `FmOp`s
    /// over the current on-disk bytes (G2, PLAN-GUI §6.9 "commit path"):
    /// conflict-guard → apply ops in order → atomic write → single-page
    /// reindex. The *byte* payload is computed by the worker (the UI sends
    /// the ops, not the new bytes), so the on-wire shape stays small and
    /// the worker owns the "bytes are the source of truth" rule.
    SaveFrontmatter {
        abs_path: String,
        expected_disk: Vec<u8>,
        ops: Vec<FmOp>,
    },
    /// Convert a `GenericMarkdown` / `UnindexedMarkdown` document into a
    /// full `KbPage` (G3, PLAN-GUI §6.15 "Convert flow"). Conflict-guard →
    /// set `type:` / `title:` + append chosen `tags:` / `search_required_phrase:`
    /// over the on-disk bytes (the body and all other fields are *preserved*;
    /// the file is never rewritten whole) → if the page is outside every root,
    /// add its containing folder as a root → atomic write → single-page
    /// reindex (`build_page` + `upsert_page`) → reload the corpus (the next
    /// `OpenPage` re-classifies the document as `KbPage` and the banner
    /// disappears). `page_type` is one of `wiki|note|label`; an empty `title`
    /// / `tags` / `gates` leave the existing value untouched.
    ConvertPage {
        abs_path: String,
        expected_disk: Vec<u8>,
        page_type: String,
        title: String,
        tags: Vec<String>,
        gates: Vec<String>,
    },
    /// Stop the worker: finish the current operation, drop the connection,
    /// exit the loop.
    Quit,
}

/// A single frontmatter edit, expressed in the *verified* kernel's terms
/// (PLAN-GUI §6.9: "verified `kb_core::frontmatter` ops only"). The worker
/// applies a batch of these over the on-disk bytes in order, via
/// `set_scalar` / `append_list_item` / `remove_list_item` — never a free-form
/// YAML rewrite. `Clone` so the UI can diff drafts into a batch and recompute
/// it on a `Revert` (the op set is the source of truth for a commit).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum FmOp {
    /// `set_scalar(key, value)`. A scalar set: `SetScalar`.
    SetScalar { key: String, value: String },
    /// `remove_field(key)` — drop a scalar/list field entirely (`Remove`).
    Remove { key: String },
    /// `append_list_item(key, value)` — add a token to a list
    /// (`AppendItem`).
    AppendItem { key: String, value: String },
    /// `remove_list_item(key, value)` — remove a specific list token
    /// (`RemoveItem`).
    RemoveItem { key: String, value: String },
}

/// The outcome of a refresh / full rebuild, in *display* terms
/// (PLAN-GUI §6.12 "Data" tab). The DB worker carries this across the
/// channel; the UI renders the `note` + the numeric columns. `Clone` so
/// a single event can update the status bar *and* the Settings table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RebuildSummary {
    pub indexed: usize,
    pub skipped_no_type: usize,
    pub skipped_missing_slug: usize,
    pub gc: usize,
    pub roots_walked: usize,
    pub gc_skipped: bool,
    /// The human-readable one-line summary from `kb_db::refresh` (the
    /// CLI shows this verbatim, so parity is one `note: String` field).
    pub note: String,
    /// `true` for `FullRebuild` (the UI's "Full rebuild" button), `false`
    /// for the incremental `Refresh`. The UI keeps both columns distinct
    /// in the §6.12 table.
    pub full: bool,
}

impl From<kb_db::refresh::RefreshSummary> for RebuildSummary {
    fn from(s: kb_db::refresh::RefreshSummary) -> Self {
        // The `full` flag is set by the dispatch arm based on which cmd
        // ran (the summary itself doesn't carry it), so `false` here and
        // the FullRebuild arm overwrites.
        RebuildSummary {
            indexed: s.indexed,
            skipped_no_type: s.skipped_no_type,
            skipped_missing_slug: s.skipped_missing_slug,
            gc: s.gc,
            roots_walked: s.roots_walked,
            gc_skipped: s.gc_skipped,
            note: s.note,
            full: false,
        }
    }
}

/// A registered root row in the §6.12 settings table. The `added_at`
/// column is the ISO timestamp `kb_db` wrote (the CLI surfaces this as
/// "added: <ts>"); `extra_extensions` is the newline-joined `extra_exts`
/// field split back into a `Vec` (the `kb roots` JSON shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootInfo {
    pub path: String,
    pub added_at: Option<String>,
    pub extra_extensions: Vec<String>,
    pub include_hidden: bool,
    /// The number of *indexed* pages under this root (computed in the
    /// worker as `SELECT COUNT(*) FROM pages WHERE root = ?1`; the CLI
    /// `kb info` reports the same column).
    pub page_count: usize,
}

/// A `tag_aliases` row (`alias`, `canonical`) — the §6.12 "tag aliases"
/// table. `Clone` so the UI can keep a fresh copy after each mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasRow {
    pub alias: String,
    pub canonical: String,
}

/// The corpus stats snapshot (the §6.12 "Data" tab). All counts are
/// `usize` (a `COUNT(*)` never goes negative); the DB *size* is `Bytes`
/// via `.size()` on the on-disk file (or zero when the DB is `:memory:`
/// in a test — the worker returns 0 in that case).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatsSnapshot {
    pub page_count: usize,
    pub distinct_tag_count: usize,
    pub link_count: usize,
    pub root_count: usize,
    pub alias_count: usize,
    pub db_path: String,
    pub db_size: u64,
}

/// An event from the DB worker to the UI thread (PLAN-GUI §3.2).
pub enum Evt {
    /// Corpus rows arrived; the note list can render. Carries:
    /// * `pages` — the full `All pages` set (browse parity, with
    ///   gate/tag/uuid/updated_at attached).
    /// * `roots_known` — drives the Welcome-vs-Home split.
    /// * `collections` — the term facets for the rail tiles.
    /// * `gated_count` / `ungated_count` — the two visibility pseudo-
    ///   collection counts (computed by the worker; the UI does not
    ///   recompute from `pages`, so the rail and the list can never
    ///   disagree).
    Corpus {
        pages: Vec<PageRow>,
        roots_known: bool,
        collections: Vec<(String, usize)>,
        gated_count: usize,
        ungated_count: usize,
        /// Distinct non-empty `language:` values across the corpus
        /// (PLAN-GUI §6.9 autocomplete source for the frontmatter inspector).
        /// Sorted for stability. Free-text values are still accepted by the
        /// inspector; this list is only a speed aid.
        distinct_languages: Vec<String>,
    },
    /// A page body arrived after an `OpenPage`. Carries the `bytes` as they
    /// were on disk at read time — the editor's conflict-guard baseline
    /// (PLAN-GUI §3.3: a later `Save` is refused unless disk still matches).
    Page {
        abs_path: String,
        body: String,
        baseline: Vec<u8>,
    },
    /// Search results arrived. `missing_tag` is `Some(tag_value)` when the
    /// query's `tag:` filter had no corpus row (the CLI's specific message).
    SearchResults {
        query: String,
        scope: Option<String>,
        rows: Vec<SearchHit>,
        missing_tag: Option<String>,
    },
    /// A save completed (conflict check passed, atomic write + single-page
    /// reindex landed).
    Saved { abs_path: String },
    /// A save was refused because the on-disk bytes no longer match what the
    /// editor opened (PLAN-GUI §3.3: conflict guard → Reload / Keep-mine /
    /// Diff, never a silent clobber).
    Conflict { abs_path: String },
    /// A new note was created (scaffold + write + upsert succeeded). The
    /// `id` (`type:slug`) is the stable identity token returned by the
    /// upsert; the UI does not surface it in G2 (the file path is what the
    /// user sees) but it is here for future navigation / deep-link work.
    Created {
        abs_path: String,
        #[allow(dead_code)]
        id: String,
    },
    /// A `ConvertPage` succeeded (conflict guard passed, `type:`/`title:`/
    /// `tags:` written, the page is now indexed *or* its folder was added as
    /// a root). The UI re-classifies the open document as a `KbPage` via `Cmd::OpenPage`
    /// and closes the conversion dialog (PLAN-GUI §6.15 step 4: "the banner
    /// disappears and all `kb` features enable").
    Converted {
        abs_path: String,
        /// The page's new id (`type:slug`), for navigation / deep links.
        #[allow(dead_code)]
        id: String,
    },
    /// Result of a refresh or full rebuild (`kb refresh` / `kb refresh
    /// --full` / `kb_db::refresh::full_rebuild`). Carries the `RefreshSummary`
    /// verbatim (the `note` field is the human-readable one-liner the
    /// CLI shows). The UI displays it next to the action that fired (the
    /// Settings → Data tab) and in the status bar.
    RebuildDone(RebuildSummary),
    /// Result of a `Cmd::ListRoots` — the registered roots in
    /// presentation order (`seq DESC`) with their walk options and the
    /// per-root page count (the §6.12 table needs both the option
    /// columns *and* the "page count" column).
    Roots(Vec<RootInfo>),
    /// Result of a `Cmd::ListAliases` — the full `tag_aliases` table
    /// as `(alias, canonical)` pairs, sorted by `alias` (the CLI's
    /// `kb tag-alias --list` order). The UI renders it as a two-column
    /// table.
    Aliases(Vec<AliasRow>),
    /// Result of a `Cmd::Stats` — the corpus stats snapshot (the
    /// §6.12 "Data" tab). The DB path + size are resolved here in the
    /// worker so the UI never has to know where the DB lives.
    Stats(StatsSnapshot),
    /// Result of a `Cmd::DropRoot` — the number of roots removed and the
    /// `RefreshSummary` of the GC sweep that ran right after (dropping
    /// a root orphans its pages; the GC sweep on the next `refresh()` is
    /// what actually deletes the rows, but the page count change shows
    /// up in the corpus reload).
    RootDropped {
        removed: usize,
        summary: RebuildSummary,
    },
    /// Result of a `Cmd::AddAlias` / `Cmd::RemoveAlias` — the affected
    /// `(alias, canonical)` pair, so the UI can update its local table
    /// state without a round-trip `ListAliases`.
    AliasChanged {
        alias: String,
        canonical: Option<String>,
        /// `true` when one was added/updated, `false` when one was removed.
        added: bool,
    },
    /// A non-fatal error in a worker operation (the corpus remains usable).
    Warn(String),
    /// A fatal startup error (DB open / migration failed); the UI should
    /// switch to its Error phase.
    Fatal(String),
}

/// The open-document save state (PLAN-GUI §6.5: `Saved` / `Saving…` /
/// `Unsaved changes` / `Conflict`). The UI renders this in the editor host's
/// footer; the worker's events drive the transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SaveStatus {
    /// The page is clean and matches disk (or was just created).
    #[default]
    Saved,
    /// A save is in flight (a `Cmd::Save` was queued, no answer yet).
    Saving,
    /// The editor has edits not yet on disk (uncommitted).
    Unsaved,
    /// A save was refused (the file changed on disk under us). The editor
    /// offers Reload / Keep-mine.
    Conflict,
}

/// UI-side handle to a running worker. `recv` drains pending events without
/// blocking (the UI polls once per frame); `send` queues a command.
#[derive(Debug)]
pub struct WorkerHandle {
    tx: Sender<Cmd>,
    rx: Mutex<mpsc::Receiver<Evt>>,
    // Read by `shutdown` (app-exit cleanup); never touched in the render path.
    #[allow(dead_code)]
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WorkerHandle {
    /// Queue a command for the worker. Best-effort: if the worker is gone,
    /// the send is a no-op (the `Cmd` is dropped) rather than a returned
    /// `SendError` (which would be large and is always discarded by callers
    /// — see `app.rs` / `welcome.rs`).
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// Drain every event currently pending. Non-blocking: an empty `Vec`
    /// is the normal steady state (once idle, nothing is pending).
    pub fn recv(&self) -> Vec<Evt> {
        let Ok(rx) = self.rx.lock() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            out.push(evt);
        }
        out
    }

    /// Ask the worker to exit and wait for it to finish (app-exit cleanup).
    // Wired to the window-close handler in G1; the API is stable now.
    #[allow(dead_code)]
    pub fn shutdown(mut self) {
        let _ = self.tx.send(Cmd::Quit);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn the DB worker thread. The thread does *no* database work until it
/// receives a command, so this is cheap enough to call right after frame 1 —
/// but never on the first-frame path (PLAN-GUI §3.4: "never call Db::open,
/// migrations, load_corpus … before frame 1").
pub fn spawn_worker() -> WorkerHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (evt_tx, evt_rx) = mpsc::channel::<Evt>();

    let thread = std::thread::Builder::new()
        .name("kb-gui-db".into())
        .spawn(move || {
            let mut db: Option<DbGuard> = None;
            for cmd in cmd_rx {
                match cmd {
                    Cmd::LoadCorpus {
                        launch,
                        db: db_path,
                    } => {
                        // Lazily open the connection on first use; subsequent
                        // `LoadCorpus` commands reuse the same instance so the
                        // worker holds one connection for the whole session
                        // (PLAN-GUI §3.2).
                        if db.is_none() {
                            match DbGuard::open(&db_path) {
                                Ok(g) => db = Some(g),
                                Err(msg) => {
                                    let _ = evt_tx.send(Evt::Fatal(msg));
                                    break;
                                }
                            }
                        }
                        // `launch` is carried for G1+ (route to the page after
                        // the corpus loads). In G0 it's already recorded in
                        // `AppState` at startup, so the worker only logs it
                        // when it would matter (debug builds / tracing).
                        if std::env::var_os("KB_GUI_TRACE_STARTUP").is_some() {
                            eprintln!("[kb-gui] LoadCorpus launch={launch:?}");
                        }
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        match load_corpus_evt(&evt_tx, conn) {
                            Ok(()) => {}
                            Err(msg) => {
                                let _ = evt_tx.send(Evt::Fatal(msg));
                                break;
                            }
                        }
                    }
                    Cmd::Refresh => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        match kb_db::refresh::refresh(&conn.db) {
                            Ok(_) => {
                                // Re-load after a refresh so the UI reflects
                                // the new index state.
                                let _ = load_corpus_evt(&evt_tx, conn);
                            }
                            Err(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg.to_string()));
                            }
                        }
                    }
                    Cmd::AddRoot(path) => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        let path_str = path.to_string_lossy().into_owned();
                        // Register the root (upsert, `include_hidden = true`,
                        // no extra extensions), then re-index + re-load.
                        match conn.db.add_root(&path_str, None, true) {
                            Ok(_) => match kb_db::refresh::refresh(&conn.db) {
                                Ok(_) => {
                                    let _ = load_corpus_evt(&evt_tx, conn);
                                }
                                Err(msg) => {
                                    let _ = evt_tx.send(Evt::Warn(msg.to_string()));
                                }
                            },
                            Err(msg) => {
                                let _ = evt_tx.send(Evt::Warn(format!("add root: {msg}")));
                            }
                        }
                    }
                    Cmd::FullRebuild => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        match kb_db::refresh::full_rebuild(&conn.db) {
                            Ok(summary) => {
                                let mut s: RebuildSummary = summary.into();
                                s.full = true;
                                let _ = evt_tx.send(Evt::RebuildDone(s));
                                let _ = load_corpus_evt(&evt_tx, conn);
                            }
                            Err(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg.to_string()));
                            }
                        }
                    }
                    Cmd::DropRoot(path) => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        let path_str = path.to_string_lossy().into_owned();
                        // Match the CLI's canonicalize-then-fallback flow
                        // (`kb/cli/roots.rs::cmd_drop_root`): if the
                        // directory is still on disk, canonicalize it to
                        // the form `add` stored; if it's gone, look up a
                        // registered root that matches the literal (case-
                        // insensitive, trailing-slash-trimmed) form. The
                        // fallback is what lets a user unregister a root
                        // whose folder has been deleted.
                        let target = match std::fs::canonicalize(&path_str) {
                            Ok(c) => c.to_string_lossy().into_owned(),
                            Err(_) => match admin::literal_root_match(&conn.db, &path_str) {
                                Some(found) => found,
                                None => {
                                    let _ = evt_tx.send(Evt::Warn(format!(
                                        "{path_str}: no registered root matches it (and the
                                         directory is no longer on disk)"
                                    )));
                                    continue;
                                }
                            },
                        };
                        match conn.db.drop_root(&target) {
                            Ok(removed) => {
                                // Same GC-sweep-after-drop-then-reload as
                                // the CLI's `cmd_drop_root` — the orphan
                                // pages (under this root only) are the
                                // ones the next `refresh()` will GC.
                                let summary = kb_db::refresh::refresh(&conn.db).map_or_else(
                                    |e| RebuildSummary {
                                        note: format!("drop OK but post-drop refresh failed: {e}"),
                                        ..Default::default()
                                    },
                                    Into::into,
                                );
                                let _ = evt_tx.send(Evt::RootDropped { removed, summary });
                                let _ = load_corpus_evt(&evt_tx, conn);
                            }
                            Err(msg) => {
                                let _ = evt_tx.send(Evt::Warn(format!("drop root: {msg}")));
                            }
                        }
                    }
                    Cmd::ListRoots => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        match admin::list_roots_full(&conn.db) {
                            Ok(rows) => {
                                let _ = evt_tx.send(Evt::Roots(rows));
                            }
                            Err(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg));
                            }
                        }
                    }
                    Cmd::AddAlias { alias, canonical } => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        // Validation (PLAN-GUI §6.12 "Tag aliases" rule:
                        // alias ≠ canonical). The DB accepts *anything*;
                        // the worker is where the semantic check belongs,
                        // so a UI typo can't silently round-trip through
                        // the worker as a no-op "add".
                        if alias.trim().is_empty() || canonical.trim().is_empty() {
                            let _ = evt_tx.send(Evt::Warn(
                                "tag alias: alias and canonical must both be non-empty".into(),
                            ));
                            continue;
                        }
                        if alias == canonical {
                            let _ = evt_tx.send(Evt::Warn(format!(
                                "tag alias: alias equals canonical ({alias}) — no-op"
                            )));
                            continue;
                        }
                        match admin::upsert_alias(&conn.db, &alias, &canonical) {
                            Ok(()) => {
                                let _ = evt_tx.send(Evt::AliasChanged {
                                    alias: alias.clone(),
                                    canonical: Some(canonical),
                                    added: true,
                                });
                            }
                            Err(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg));
                            }
                        }
                    }
                    Cmd::RemoveAlias { alias } => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        match admin::remove_alias(&conn.db, &alias) {
                            Ok(n) if n > 0 => {
                                let _ = evt_tx.send(Evt::AliasChanged {
                                    alias: alias.clone(),
                                    canonical: None,
                                    added: false,
                                });
                            }
                            Ok(_) => {
                                let _ = evt_tx.send(Evt::Warn(format!(
                                    "tag alias: no alias {alias:?} found — nothing removed"
                                )));
                            }
                            Err(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg));
                            }
                        }
                    }
                    Cmd::ListAliases => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        match admin::list_aliases(&conn.db) {
                            Ok(rows) => {
                                let _ = evt_tx.send(Evt::Aliases(rows));
                            }
                            Err(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg));
                            }
                        }
                    }
                    Cmd::Stats => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        match admin::stats_snapshot(&conn.db, conn.db.path()) {
                            Ok(snap) => {
                                let _ = evt_tx.send(Evt::Stats(snap));
                            }
                            Err(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg));
                            }
                        }
                    }
                    Cmd::Search { query, scope } => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        let limit = 20; // CLI default
                        match search::search_pages(&conn.db, &query, scope.as_deref(), limit) {
                            Ok((rows, missing_tag)) => {
                                let _ = evt_tx.send(Evt::SearchResults {
                                    query,
                                    scope,
                                    rows,
                                    missing_tag,
                                });
                            }
                            Err(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg));
                            }
                        }
                    }
                    Cmd::OpenPage(abs_path) => {
                        // G2: read the file body *and* capture the on-disk
                        // bytes as the conflict-guard baseline (PLAN-GUI §3.3).
                        match std::fs::read(&abs_path) {
                            Ok(bytes) => {
                                let body = String::from_utf8_lossy(&bytes).into_owned();
                                let _ = evt_tx.send(Evt::Page {
                                    abs_path,
                                    body,
                                    baseline: bytes,
                                });
                            }
                            Err(e) => {
                                let _ = evt_tx.send(Evt::Warn(format!("open {abs_path}: {e}")));
                            }
                        }
                    }
                    Cmd::Save {
                        abs_path,
                        expected_disk,
                        new_bytes,
                        force,
                    } => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        match save_page(conn, &abs_path, &expected_disk, &new_bytes, force) {
                            PageWrite::Saved => {
                                let _ = evt_tx.send(Evt::Saved { abs_path });
                                // Re-load the corpus so the list/outline reflect
                                // the new update time + any title/gate changes.
                                let _ = load_corpus_evt(&evt_tx, conn);
                            }
                            PageWrite::Conflict => {
                                let _ = evt_tx.send(Evt::Conflict { abs_path });
                            }
                            PageWrite::Failed(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg));
                            }
                        }
                    }
                    Cmd::CreatePage {
                        directory,
                        title,
                        page_type,
                        tags,
                        gates,
                        description,
                        language,
                    } => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        match create_page(
                            conn,
                            &directory,
                            &title,
                            &page_type,
                            &tags,
                            &gates,
                            description.as_deref(),
                            language.as_deref(),
                        ) {
                            Created::Done { abs_path, id } => {
                                let _ = evt_tx.send(Evt::Created { abs_path, id });
                                let _ = load_corpus_evt(&evt_tx, conn);
                            }
                            Created::Skipped(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg));
                            }
                            Created::Failed(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg));
                            }
                        }
                    }
                    Cmd::SaveFrontmatter {
                        abs_path,
                        expected_disk,
                        ops,
                    } => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        match save_frontmatter(conn, &abs_path, &expected_disk, &ops) {
                            PageWrite::Saved => {
                                let _ = evt_tx.send(Evt::Saved { abs_path });
                                // Re-load the corpus so the list/outline/rail
                                // tags + gate facets reflect the new frontmatter.
                                let _ = load_corpus_evt(&evt_tx, conn);
                            }
                            PageWrite::Conflict => {
                                let _ = evt_tx.send(Evt::Conflict { abs_path });
                            }
                            PageWrite::Failed(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg));
                            }
                        }
                    }
                    Cmd::ConvertPage {
                        abs_path,
                        expected_disk,
                        page_type,
                        title,
                        tags,
                        gates,
                    } => {
                        let Some(conn) = db.as_ref() else {
                            let _ = evt_tx.send(Evt::Fatal("db not open".into()));
                            break;
                        };
                        match convert_page(
                            conn,
                            &abs_path,
                            &expected_disk,
                            &page_type,
                            title.trim(),
                            &tags,
                            &gates,
                        ) {
                            Converted::Done { id } => {
                                let _ = evt_tx.send(Evt::Converted { abs_path, id });
                                // Reload the corpus so the list/outline/rail
                                // reflect the newly-indexed page (the page is
                                // now a `KbPage`). The `OpenPage` re-fold in
                                // the UI re-classifies and drops the banner.
                                let _ = load_corpus_evt(&evt_tx, conn);
                            }
                            Converted::Conflict => {
                                let _ = evt_tx.send(Evt::Conflict { abs_path });
                            }
                            Converted::Failed(msg) => {
                                let _ = evt_tx.send(Evt::Warn(msg));
                            }
                        }
                    }
                    Cmd::Quit => break,
                }
            }
            // `db` (and the `Db` it wraps) drops on the worker thread — the
            // connection is closed off the UI thread.
            drop(db);
        });

    match thread {
        Ok(handle) => WorkerHandle {
            tx: cmd_tx,
            rx: Mutex::new(evt_rx),
            thread: Some(handle),
        },
        Err(err) => {
            eprintln!("kb-gui: failed to spawn DB worker: {err}");
            WorkerHandle {
                tx: cmd_tx,
                rx: Mutex::new(evt_rx),
                thread: None,
            }
        }
    }
}

/// Wraps `kb_db::Db` for the worker loop. The worker holds a *single*
/// connection for the whole session (PLAN-GUI §3.2: "the worker thread owns
/// the `Db` connection for the whole session").
struct DbGuard {
    db: kb_db::Db,
}

impl DbGuard {
    fn open(db_path: &Option<PathBuf>) -> Result<Self, String> {
        let db: kb_db::Db = match db_path {
            Some(path) => open_at(path)?,
            None => kb_db::Db::open_default().map_err(|e| format!("open default DB: {e}"))?,
        };
        db.migrate().map_err(|e| format!("migrate: {e}"))?;
        Ok(Self { db })
    }
}

/// Shared "load corpus + compute facets + emit `Evt::Corpus`" step used by
/// `LoadCorpus`, `Refresh`, and `AddRoot`. Returns `Ok(())` on success;
/// the caller maps `Err(String)` to the appropriate event (fatal for
/// first-load, warn for refresh / add-root).
fn load_corpus_evt(evt_tx: &Sender<Evt>, conn: &DbGuard) -> Result<(), String> {
    let pages = corpus::load_pages(&conn.db, None)?;
    let roots_known = corpus::has_roots(&conn.db);
    let (collections, gated_count, ungated_count) = corpus::collection_facets(&pages);
    let distinct_languages = corpus::distinct_languages(&conn.db);
    evt_tx
        .send(Evt::Corpus {
            pages,
            roots_known,
            collections,
            gated_count,
            ungated_count,
            distinct_languages,
        })
        .map_err(|e| format!("emit corpus: {e}"))
}

fn open_at(path: &Path) -> Result<kb_db::Db, String> {
    let db = kb_db::Db::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
    Ok(db)
}

/// The outcome of a `Save` (PLAN-GUI §3.3). `Saved` = the conflict guard
/// passed and the write + reindex landed; `Conflict` = the on-disk bytes no
/// longer match the editor's baseline (the *write* was not attempted);
/// `Failed` = a real I/O or index error (already formatted for the status bar).
#[derive(Debug)]
enum PageWrite {
    Saved,
    Conflict,
    Failed(String),
}

/// `Save` → conflict guard → atomic write → single-page reindex.
///
/// Mirrors the TUI commit path (`kb/tui/mod.rs`, `Mode::Edit` branch) and the
/// CLI `create` index path verbatim, so the GUI's writes reconcile
/// byte-identically with `kb refresh`:
/// 1. Re-read the file; it must equal the byte slice the editor opened
///    (`expected_disk`) — a mid-session external edit is refused as a
///    *Conflict* (PLAN-GUI §3.3 "never silent clobbering"), *not* written.
/// 2. If `new_bytes == on_disk` the edit is a no-op: the file is byte-
///    identical, so there is nothing to write and nothing to reindex (the
///    "open then commit" no-artifact rule, PLAN-GUI §3.3 rule 3 "a save never
///    rewrites bytes it did not edit").
/// 3. Otherwise atomically write (`unique_tmp_in`-style temp + `rename`) and
///    reindex the single page with `build_page(_, include_language: true)` +
///    `upsert_page` (the `language:`-clobber fix: `true`, not the historical
///    `false` the TUI carried — see the TUI comment at the call site).
fn save_page(
    conn: &DbGuard,
    abs_path: &str,
    expected_disk: &[u8],
    new_bytes: &[u8],
    force: bool,
) -> PageWrite {
    let path = Path::new(abs_path);
    // 1) Conflict guard: re-read and compare against the editor's baseline.
    //    `force = true` (the §6.10 "Keep mine" resolve step) skips this guard
    //    so the user can overwrite a mid-session external change.
    let on_disk = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return PageWrite::Failed(format!("re-read {abs_path} before write: {e}")),
    };
    if !force && on_disk != expected_disk {
        return PageWrite::Conflict;
    }
    // 2) No-op write: byte-identical ⇒ nothing to do (no write, no reindex).
    if on_disk == new_bytes {
        return PageWrite::Saved;
    }
    // 3) Atomic write.
    if let Err(e) = atomic_write(path, new_bytes) {
        return PageWrite::Failed(format!("write {abs_path} atomically: {e}"));
    }
    // Reindex the single page (best-effort: disk is authoritative, a reindex
    // failure is a *warn* on the already-written file — same as the TUI/CLI).
    match reindex_single(conn, abs_path) {
        Ok(_indexed_or_skipped) => PageWrite::Saved,
        Err(msg) => PageWrite::Failed(format!("wrote {abs_path}; DB upsert note: {msg}")),
    }
}

/// `SaveFrontmatter` → conflict guard → apply the `FmOp` batch (in order, via
/// the verified `kb_core::frontmatter` kernel) → atomic write → single-page
/// reindex.
///
/// The worker recomputes the new bytes from the on-disk bytes + ops, so the
/// "bytes are the source of truth" rule (PLAN-GUI §6.9) stays local to the
/// worker: the UI sends *what to change* (the op set) and the worker decides
/// *what the bytes become* using only `set_scalar` / `append_list_item` /
/// `remove_list_item` / `remove_field`. Same conflict-guard + atomic-write +
/// no-op-write semantics as `save_page` (PLAN-GUI §3.3).
fn save_frontmatter(
    conn: &DbGuard,
    abs_path: &str,
    expected_disk: &[u8],
    ops: &[FmOp],
) -> PageWrite {
    let path = Path::new(abs_path);
    // 1) Conflict guard: re-read and compare against the UI's baseline.
    let on_disk = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return PageWrite::Failed(format!("re-read {abs_path} before write: {e}")),
    };
    if on_disk != expected_disk {
        return PageWrite::Conflict;
    }
    if ops.is_empty() {
        // No-op commit: the drafts equal the on-disk values — nothing to write.
        return PageWrite::Saved;
    }
    // 2) Apply the op batch in order over the on-disk bytes. An op that
    //    errors (e.g. a `RemoveItem` on a missing token the UI lost track of)
    //    is a *warn*, not a clobber — the file is not yet written.
    let mut bytes = on_disk.clone();
    for op in ops {
        let out = match op {
            FmOp::SetScalar { key, value } => kb_core::frontmatter::set_scalar(&bytes, key, value),
            FmOp::Remove { key } => kb_core::frontmatter::remove_field(&bytes, key),
            FmOp::AppendItem { key, value } => {
                kb_core::frontmatter::append_list_item(&bytes, key, value)
            }
            FmOp::RemoveItem { key, value } => {
                kb_core::frontmatter::remove_list_item(&bytes, key, value)
            }
        };
        match out {
            Ok(v) => bytes = v,
            Err(err) => {
                return PageWrite::Failed(format!(
                    "frontmatter op {:?} failed: {err} (file unchanged)",
                    summarize_op(op)
                ))
            }
        }
    }
    // 3) No-op write: byte-identical ⇒ nothing to do (the op set collapsed).
    if bytes == on_disk {
        return PageWrite::Saved;
    }
    // 4) Atomic write.
    if let Err(e) = atomic_write(path, &bytes) {
        return PageWrite::Failed(format!("write {abs_path} atomically: {e}"));
    }
    // Reindex the single page (best-effort: a reindex failure is a *warn*).
    match reindex_single(conn, abs_path) {
        Ok(_indexed_or_skipped) => PageWrite::Saved,
        Err(msg) => PageWrite::Failed(format!("wrote {abs_path}; DB upsert note: {msg}")),
    }
}

/// `ConvertPage` → conflict guard → apply frontmatter ops (set `type:`, set
/// `title:` if non-empty, append each `tags:` / `search_required_phrase:`
/// term) → if the page is outside every root, add its containing folder as a
/// root → atomic write → single-page reindex (`build_page` + `upsert_page`).
///
/// The body and all *other* frontmatter fields are preserved: only the ops
/// above are applied over the on-disk bytes (PLAN-GUI §6.15 step 3:
/// "conversion preserves the body and all existing frontmatter fields; it
/// only adds/sets the missing `type:`, title fallback, and any chosen list
/// items. It never rewrites the whole file"). A `page_type` not in
/// `wiki|note|label` is a `Failed` before any write is attempted.
fn convert_page(
    conn: &DbGuard,
    abs_path: &str,
    expected_disk: &[u8],
    page_type: &str,
    title: &str,
    tags: &[String],
    gates: &[String],
) -> Converted {
    if !matches!(page_type, "wiki" | "note" | "label") {
        return Converted::Failed(format!(
            "--type must be wiki|note|label (got {page_type:?})"
        ));
    }
    // 1) Conflict guard: re-read and compare against the UI's baseline.
    let path = Path::new(abs_path);
    let mut on_disk = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return Converted::Failed(format!("re-read {abs_path} before write: {e}")),
    };
    if on_disk != expected_disk {
        return Converted::Conflict;
    }
    // 1b) If the file carries no frontmatter frame at all (a pure body, as a
    //     generic Markdown page typically is), seed a minimal empty frame so
    //     the field ops below can land. §6.15: conversion *adds* a valid
    //     `type:` (and optional fields); it never rewrites or drops the body.
    //     The empty frame is byte-transparent to the body — `parse` still
    //     returns the whole text as `body`.
    if kb_core::frontmatter::find_fm(&on_disk).is_none() {
        let mut seeded = Vec::with_capacity(on_disk.len() + 9);
        seeded.extend_from_slice(b"---\n---\n\n");
        seeded.extend_from_slice(&on_disk);
        on_disk = seeded;
    }
    // 2) Build the op set (in order): set type always, set title if
    //    non-empty, append each tag, append each gate term.
    let mut ops: Vec<FmOp> = vec![FmOp::SetScalar {
        key: "type".to_string(),
        value: page_type.to_string(),
    }];
    if !title.is_empty() {
        ops.push(FmOp::SetScalar {
            key: "title".to_string(),
            value: title.to_string(),
        });
    }
    for t in tags {
        ops.push(FmOp::AppendItem {
            key: "tags".to_string(),
            value: t.clone(),
        });
    }
    for g in gates {
        ops.push(FmOp::AppendItem {
            key: "search_required_phrase".to_string(),
            value: g.clone(),
        });
    }
    // 3) Apply the ops in order over the on-disk bytes.
    let mut bytes = on_disk.clone();
    for op in &ops {
        let out = match op {
            FmOp::SetScalar { key, value } => kb_core::frontmatter::set_scalar(&bytes, key, value),
            FmOp::Remove { key } => kb_core::frontmatter::remove_field(&bytes, key),
            FmOp::AppendItem { key, value } => {
                kb_core::frontmatter::append_list_item(&bytes, key, value)
            }
            FmOp::RemoveItem { key, value } => {
                kb_core::frontmatter::remove_list_item(&bytes, key, value)
            }
        };
        match out {
            Ok(v) => bytes = v,
            Err(err) => {
                return Converted::Failed(format!(
                    "convert: frontmatter op {:?} failed: {err} (file unchanged)",
                    summarize_op(op)
                ))
            }
        }
    }
    // 4) If the page is outside every root, add its containing folder as a
    //    root (PLAN-GUI §6.15 step 2: "If the file is outside every root, the
    //    primary action is Add containing folder as root & convert").
    let roots = match roots_with_seq(&conn.db) {
        Ok(r) => r,
        Err(e) => return Converted::Failed(format!("convert: read roots: {e}")),
    };
    if kb_db::walk_snapshot::winning_root(&roots, abs_path).is_none() {
        let parent = path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !parent.is_empty() {
            if let Err(e) = conn.db.add_root(&parent, None, true) {
                return Converted::Failed(format!("convert: add root {parent:?}: {e}"));
            }
        }
    }
    // 5) Atomic write.
    if let Err(e) = atomic_write(path, &bytes) {
        return Converted::Failed(format!("convert: write {abs_path} atomically: {e}"));
    }
    // 6) Single-page reindex (`build_page` + `upsert_page`).
    match reindex_single(conn, abs_path) {
        Ok(Some(id)) => Converted::Done { id },
        Ok(None) => Converted::Failed(format!(
            "convert: wrote {abs_path} but could not index (page not under any registered root)"
        )),
        Err(msg) => Converted::Failed(format!("convert: wrote {abs_path}; DB upsert: {msg}")),
    }
}

/// The outcome of a `ConvertPage`.
#[derive(Debug)]
enum Converted {
    Done { id: String },
    Conflict,
    Failed(String),
}

/// A short human form of an `FmOp` for the status-bar / warn line (keeps the
/// enum's shape out of the message).
fn summarize_op(op: &FmOp) -> String {
    match op {
        FmOp::SetScalar { key, value } => format!("set {key}={value:?}"),
        FmOp::Remove { key } => format!("remove {key}"),
        FmOp::AppendItem { key, value } => format!("add {key}+{value:?}"),
        FmOp::RemoveItem { key, value } => format!("drop {key}-{value:?}"),
    }
}

/// The outcome of a `CreatePage`. `Done` = the scaffold was written *and*
/// indexed (the row is in the DB). `Skipped` = the file was written but
/// indexing was skipped (no roots / not under a root / not indexable) — a
/// warn, never an error, matching the CLI `create` contract. `Failed` = a
/// real I/O or validation error.
enum Created {
    Done { abs_path: String, id: String },
    Skipped(String),
    Failed(String),
}

/// `CreatePage` → resolve target → scaffold → atomic write → upsert.
///
/// Scaffolds the exact byte-shape `kb create` writes (PLAN §4): a YAML
/// frontmatter block (`type:` + `title:` + `description:` + `tags:` +
/// `search_required_phrase:`) followed by `# <title>`. Gate terms and extra
/// tags are folded into `search_required_phrase:` / `tags:` lists. The
/// `--type` value must be one of `wiki|note|label` (the CLI's `NoteType`
/// domain) — anything else is a `Failed`.
#[allow(clippy::too_many_arguments)] // the full `kb create` parameter set (CLI parity)
fn create_page(
    conn: &DbGuard,
    directory: &str,
    title: &str,
    page_type: &str,
    tags: &[String],
    gates: &[String],
    description: Option<&str>,
    language: Option<&str>,
) -> Created {
    // Validate the type (the CLI's domain, PLAN §4 "wiki|note|label").
    if !matches!(page_type, "wiki" | "note" | "label") {
        return Created::Failed(format!(
            "--type must be wiki|note|label (got {page_type:?})"
        ));
    }
    // Derive a clean filename stem from the title (no NUL / newline chars,
    // `.md` always present).
    let title_clean = title.trim();
    if title_clean.is_empty() {
        return Created::Failed("create: NAME must not be empty".into());
    }
    if title_clean
        .chars()
        .any(|c| c == '\0' || c == '\n' || c == '\r')
    {
        return Created::Failed("create: NAME must not contain newlines or NUL bytes".into());
    }
    let stem = title_clean
        .trim()
        .replace(|c: char| c.is_whitespace() || c == '\t', "-")
        .replace(['/', '\\', ':', '"', '<', '>', '|', '?', '*'], "-");
    if stem.is_empty() {
        return Created::Failed("create: NAME must yield a non-empty page title".into());
    }
    // Resolve the target path against the directory (append `.md` when absent).
    let dir = Path::new(directory);
    let target = dir.join(format!("{stem}.md"));
    if let Some(ext) = target.extension().and_then(|e| e.to_str()) {
        if ext != "md" {
            return Created::Failed(format!(
                "create: {stem:?} is not a .md page (extension {ext:?}) — indexes .md only"
            ));
        }
    }
    if target.exists() {
        return Created::Failed(format!("create: {} already exists", target.display()));
    }
    // Make sure the directory exists (a root's sub-folder may not yet).
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Created::Failed(format!("create {}: {e}", parent.display()));
            }
        }
    }
    // Scaffold (PLAN §4). The description / tags / gates / language are
    // folded in as YAML list / scalar fields; the body starts with `# title`.
    let desc = description.unwrap_or_default();
    let tags_yaml = if tags.is_empty() {
        "tags: []".to_string()
    } else {
        let mut s = "tags:".to_string();
        for t in tags {
            s.push_str(&format!("\n  - {t}"));
        }
        s
    };
    let gates_yaml = if gates.is_empty() {
        "search_required_phrase: []".to_string()
    } else {
        let mut s = "search_required_phrase:".to_string();
        for g in gates {
            s.push_str(&format!("\n  - {g}"));
        }
        s
    };
    let lang = language.filter(|l| !l.is_empty());
    let lang_yaml = lang.map(|l| format!("\nlanguage: {l}")).unwrap_or_default();
    let desc_yaml = if desc.is_empty() {
        "description:".to_string()
    } else {
        format!("description: {desc}")
    };
    let scaffold = format!(
        "---\ntype: {page_type}\ntitle: {title_clean}\n{desc_yaml}\n{tags_yaml}\n{gates_yaml}{lang_yaml}\n---\n\n# {title_clean}\n\n"
    );
    // Atomic write.
    if let Err(e) = atomic_write(&target, scaffold.as_bytes()) {
        return Created::Failed(format!("write {}: {e}", target.display()));
    }
    let abs_target = std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
    let abs_s = abs_target.to_string_lossy().to_string();
    // Index (best-effort: a skip is a note, not an error, matching the CLI).
    let outcome = match reindex_single(conn, &abs_s) {
        Ok(r) => r,
        Err(msg) => return Created::Failed(format!("indexed write failed: {msg}")),
    };
    match outcome {
        Some(id) => Created::Done {
            abs_path: abs_s,
            id,
        },
        None => Created::Skipped(format!(
            "{} created ({} bytes) — skipped indexing (run `kb add <dir>` first, then `kb refresh`)",
            abs_s,
            scaffold.len()
        )),
    }
}

/// Reindex a single on-disk page (the same `build_page` + `upsert_page`
/// pair the CLI `refresh`/`create`/`commit` use). Returns:
/// * `Ok(Some(id))` — the page indexed; the new row's display id.
/// * `Ok(None)`     — the page is on disk but not under any registered root
///   (the CLI's "no roots / not under a root" skip; a warn, never an error).
/// * `Err(msg)`     — `build_page`/`upsert_page` failed (frontmatter / DB).
///
/// `include_language` is always `true`: the page's `language:` must survive
/// (the historical `false` TUI path silently nulled it back to the DB's NULL
/// on every commit — see the TUI's comment at the `build_page` call site).
fn reindex_single(conn: &DbGuard, abs_path: &str) -> std::result::Result<Option<String>, String> {
    let roots = roots_with_seq(&conn.db)?;
    let (root, rel) = match kb_db::walk_snapshot::winning_root(&roots, abs_path) {
        Some(r) => r,
        None => return Ok(None),
    };
    match kb_db::index::build_page(Path::new(abs_path), &root, &rel, true) {
        Ok(rec) => match kb_db::index::upsert_page(&conn.db, &rec) {
            Ok(_) => Ok(Some(rec.id)),
            Err(e) => Err(format!("DB upsert: {e}")),
        },
        Err(e) => Err(format!("frontmatter: {e}")),
    }
}

/// The registered roots as `(path, seq)` pairs, for `winning_root`
/// (port of `kb` CLI's `roots_with_seq`, `kb/main.rs:1147` — the same SELECT,
/// so the GUI never desyncs from the CLI's root-resolution rule).
fn roots_with_seq(db: &kb_db::Db) -> std::result::Result<Vec<(String, i64)>, String> {
    let stmt = db
        .conn
        .prepare("SELECT path, seq FROM roots ORDER BY seq")
        .map_err(|e| format!("prepare roots: {e}"))?;
    let rows = stmt
        .query_map((), |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| format!("query roots: {e}"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| format!("read roots: {e}"))?;
    Ok(rows)
}

/// Atomic file write: same-directory temp file + `sync_all` + `rename` over
/// the target (a port of `kb::atomic_write`'s *semantics*, `kb/main.rs:1738` —
/// this workspace has no shared helper; the GUI keeps its own so the binary
/// stays independent of the CLI's crate tree).
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kb-gui.tmp".to_string());
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!("{name}.tmp.{pid}.{nanos}"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-contained end-to-end `convert_page` over a real temp dir + real
    /// temp DB: takes a *pure-body* Markdown file (no frontmatter, the generic
    /// case the banner's Convert flow is for), runs the full op pipeline, and
    /// asserts the written file is a valid indexed `note` and the page row is
    /// in the corpus. This is the deterministic stand-in for the X11 visual
    /// check of the dialog's **Convert and save** button (PLAN-GUI §6.15).
    #[test]
    fn convert_a_pure_body_file_into_an_indexed_note() {
        let dir = std::env::temp_dir().join(format!(
            "kb-gui-conv-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("generic.md");
        // No frontmatter at all — just a body with a leading H1.
        let body = "# My generic doc\n\nJust **plain** prose, no frontmatter.\n";
        std::fs::write(&file, body).unwrap();
        let abs = canonical(&file);
        let expected = std::fs::read(&file).unwrap();

        let db_file = dir.join("conv.db");
        let conn = DbGuard::open(&Some(db_file.clone())).unwrap();

        let tags = vec!["alpha".to_string(), "beta".to_string()];
        let res = convert_page(
            &conn,
            &abs,
            &expected,
            "note",
            "Converted title",
            &tags,
            &[],
        );

        let written = std::fs::read(&file).unwrap();

        let id = match res {
            Converted::Done { id } => id,
            other => panic!("expected Done, got {other:?}"),
        };
        assert!(
            id.starts_with("note:"),
            "id carries the note: prefix, got {id}"
        );

        // The written file now carries a valid frontmatter frame with
        // type:note, the title, and both tags — §6.15 "add / set only".
        let text = String::from_utf8(written).unwrap();
        assert!(text.starts_with("---\n"), "starts with frontmatter frame");
        assert!(text.contains("type: note"), "type set to note: {text}");
        assert!(text.contains("title: Converted title"), "title set: {text}");
        assert!(
            text.contains("alpha") && text.contains("beta"),
            "both tags appended: {text}"
        );
        assert!(
            text.contains("# My generic doc"),
            "body preserved verbatim: {text}"
        );
        // Round-trips to a valid, indexable note.
        let fm = kb_core::frontmatter::parse(&text);
        assert!(fm.is_indexable());
        assert!(fm.body.contains("# My generic doc"));

        // And the page row is actually in the corpus under a real root.
        let page_row = {
            let stmt = conn
                .db
                .conn
                .prepare("SELECT id, root FROM pages WHERE id = ?1")
                .unwrap();
            stmt.query_map([id.as_str()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .unwrap()
            .next()
        };
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            page_row.is_some(),
            "page row {id} should be in the corpus after conversion"
        );
    }

    /// Self-contained end-to-end `save_page` conflict-guard test (the §6.10
    /// "Conflict" dialog's *Reload / Keep-mine* decision points):
    ///
    /// - first save (no external edit) writes cleanly;
    /// - after an external edit, a non-forced save is refused (Conflict);
    /// - a *forced* save ("Keep mine") overwrites and lands on disk.
    ///
    /// The reindex step is an `Ok(None)` no-op (the page isn't under any
    /// registered root here) — the test focuses on the *write* guarantee.
    #[test]
    fn save_page_force_overrides_external_change_and_refuses_without_force() {
        let dir = std::env::temp_dir().join(format!(
            "kb-gui-save-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("page.md");
        let init_body = "---\ntype: note\ntitle: T\n---\n\n# T\n\nv1 content.\n";
        std::fs::write(&file, init_body).unwrap();
        let abs = canonical(&file);
        let init_bytes = std::fs::read(&file).unwrap();
        let edited_bytes = b"---\ntype: note\ntitle: T\n---\n\n# T\n\nv2 (mine).\n";

        let db_file = dir.join("save.db");
        let conn = DbGuard::open(&Some(db_file)).unwrap();

        // 1) First save: on_disk == expected_disk, so the guard passes and
        //    the write lands cleanly.
        let res1 = save_page(
            &conn,
            &abs,
            &init_bytes,
            edited_bytes,
            /* force = */ false,
        );
        assert!(matches!(res1, PageWrite::Saved), "first save: {res1:?}");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            edited_bytes,
            "first save wrote v2"
        );

        // 2) An *external* edit lands on disk between save attempts.
        let external =
            b"---\ntype: note\ntitle: T\n---\n\n# T\n\nv1 content.\n\n(external change)\n";
        std::fs::write(&file, external).unwrap();

        // 3) A non-forced save (the user's normal Ctrl+S) is now a Conflict —
        //    the on-disk bytes no longer match the baseline captured at open.
        let res2 = save_page(
            &conn,
            &abs,
            &init_bytes,
            edited_bytes,
            /* force = */ false,
        );
        assert!(
            matches!(res2, PageWrite::Conflict),
            "external edit must be refused: {res2:?}"
        );

        // 4) "Keep mine" (§6.10 resolve step): a forced save skips the guard
        //    and overwrites the external change.
        let res3 = save_page(
            &conn,
            &abs,
            &init_bytes,
            edited_bytes,
            /* force = */ true,
        );
        assert!(
            matches!(res3, PageWrite::Saved),
            "force save (Keep mine) must land: {res3:?}"
        );
        assert_eq!(
            std::fs::read(&file).unwrap(),
            edited_bytes,
            "Keep mine wrote my buffer over the external change"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A no-op save (bytes unchanged) is reported as `Saved` *and* performs
    /// no write artifact (the §3.3 "a save never rewrites bytes it did not
    /// edit" rule): the mtime of an unchanged file is preserved.
    #[test]
    fn save_page_noop_preserves_file_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "kb-gui-save-noop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("noop.md");
        let body = b"no frontmatter, just a body.\n";
        std::fs::write(&file, body).unwrap();
        let abs = canonical(&file);
        let before = std::fs::read(&file).unwrap();
        let db_file = dir.join("noop.db");
        let conn = DbGuard::open(&Some(db_file)).unwrap();
        let res = save_page(&conn, &abs, &before, &before, /* force = */ false);
        assert!(matches!(res, PageWrite::Saved), "no-op save: {res:?}");
        assert_eq!(std::fs::read(&file).unwrap(), before, "file unchanged");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `save_page` refuses to save when the file is unreadable (a mid-session
    /// delete) — the user sees a `Failed` warning, not a phantom `Saved`.
    #[test]
    fn save_page_missing_file_is_failed() {
        let dir = std::env::temp_dir().join(format!(
            "kb-gui-save-miss-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Create then remove the file so we have a canonical (existing) path
        // that the save's re-read must fail on (NotFound).
        let probe = dir.join("never-was.md");
        std::fs::write(&probe, b"tmp").unwrap();
        let nonexist = canonical(&probe);
        std::fs::remove_file(&probe).unwrap();
        let db_file = dir.join("miss.db");
        let conn = DbGuard::open(&Some(db_file)).unwrap();
        let res = save_page(&conn, &nonexist, &[], b"new body", /* force = */ false);
        assert!(
            matches!(res, PageWrite::Failed(_)),
            "unreadable file: {res:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn canonical(p: &Path) -> String {
        std::fs::canonicalize(p)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }
}
