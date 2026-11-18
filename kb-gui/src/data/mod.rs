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
    /// Re-walk the registered roots and re-index. Reserved (G5).
    #[allow(dead_code)] // reserved for G5; constructed once the UI wires the action
    Refresh,
    /// Register a root directory + (optionally) re-index + reload the
    /// corpus. Wired in G1.
    AddRoot(PathBuf),
    /// Unregister a root directory. Reserved (G5).
    #[allow(dead_code)] // reserved for G5
    DropRoot(PathBuf),
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
    /// Save a page's body (conflict-check → atomic write → reindex). Reserved (G2).
    #[allow(dead_code)] // reserved for G2
    Save { abs_path: String, bytes: Vec<u8> },
    /// Stop the worker: finish the current operation, drop the connection,
    /// exit the loop.
    Quit,
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
    },
    /// A page body arrived after an `OpenPage`. Reserved (G2).
    #[allow(dead_code)]
    Page { abs_path: String, body: String },
    /// Search results arrived. `missing_tag` is `Some(tag_value)` when the
    /// query's `tag:` filter had no corpus row (the CLI's specific message).
    SearchResults {
        query: String,
        scope: Option<String>,
        rows: Vec<SearchHit>,
        missing_tag: Option<String>,
    },
    /// A save completed. Reserved (G2).
    #[allow(dead_code)]
    Saved { abs_path: String },
    /// A non-fatal error in a worker operation (the corpus remains usable).
    Warn(String),
    /// A fatal startup error (DB open / migration failed); the UI should
    /// switch to its Error phase.
    Fatal(String),
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
    /// Queue a command for the worker. `Err` means the worker is gone.
    pub fn send(&self, cmd: Cmd) -> Result<(), mpsc::SendError<Cmd>> {
        self.tx.send(cmd)
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
                    Cmd::DropRoot(_) => {
                        let _ = evt_tx.send(Evt::Warn("reserved for G5".into()));
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
                        // G1: read the file body (read-only). The write path,
                        // conflict guard, and single-page reindex land in G2.
                        match std::fs::read(&abs_path) {
                            Ok(bytes) => {
                                let body = String::from_utf8_lossy(&bytes).into_owned();
                                let _ = evt_tx.send(Evt::Page { abs_path, body });
                            }
                            Err(e) => {
                                let _ = evt_tx.send(Evt::Warn(format!("open {abs_path}: {e}")));
                            }
                        }
                    }
                    Cmd::Save { .. } => {
                        let _ = evt_tx.send(Evt::Warn("reserved for G2".into()));
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
    evt_tx
        .send(Evt::Corpus {
            pages,
            roots_known,
            collections,
            gated_count,
            ungated_count,
        })
        .map_err(|e| format!("emit corpus: {e}"))
}

fn open_at(path: &Path) -> Result<kb_db::Db, String> {
    let db = kb_db::Db::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
    Ok(db)
}
