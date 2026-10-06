//! # kb-db — the *assumed* shell over SQLite (PLAN.md §10)
//!
//! Talks to `rusqlite` (bundled, sync — **no tokio / no async**; see
//! PLAN.md §2.3 and §13.7). The boundary between "proven" (`kb-core`)
//! and "assumed" (`kb-db`) is *visible* in the dependency graph:
//! `kb-db` depends on `kb-core`, never the other way round.
//!
//! M0 contents: a `Db` newtype that opens the file, one tiny schema
//! (the `roots` table — the only *non-derivable* state we commit to
//! on disk), and a smoke test that opens an in-memory DB and writes a
//! row. FTS5 probes (`bm25` / `snippet` shape / multiprocess WAL) come
//! in M1 (PLAN.md §12) as focused integration tests.

pub mod index;
pub mod migrate;
pub mod refresh;
pub mod walk_snapshot;

use rusqlite::{Connection, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A handle to the knowledgebase SQLite file. Thin wrapper; the real
/// SQL lives in `kb/src` (the CLI) and `kb-db/src/query.rs` (M1+).
pub struct Db {
    pub conn: Connection,
    /// If this DB was opened via `open_in_memory_test`, the temp file we
    /// should clean up on `Drop`. `None` for `open` / `open_default`.
    #[allow(dead_code)]
    owned_temp_db: Option<PathBuf>,
}

// Counter so parallel tests (each calling `open_in_memory_test`) get
// distinct temp files and never share a database with one another.
static TEST_DB_COUNTER: AtomicU64 = AtomicU64::new(0);

impl Db {
    /// Open or create the SQLite DB at `path`.
    ///
    /// `:memory:` is a valid path — we use it in tests so they don't
    /// stomp on `~/.local/share/knowledgebase/kb.db`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Db { conn, owned_temp_db: None })
    }

    /// Absolute path of a temp-file-backed test DB, or `None` for
    /// `open` / `open_default`. Used by tests to open a *second*
    /// connection to the same database (simulating two `kb` processes
    /// for the WAL probe).
    pub fn path(&self) -> Option<&PathBuf> {
        self.owned_temp_db.as_ref()
    }

    /// Open a throwaway database in a temp file under the OS temp dir.
    ///
    /// Temp-file (not `:memory:`) so that a *second* connection opened to
    /// the same path shares the same database — required for the
    /// multi-client WAL probe. Each call gets a fresh file (`TEST_DB_COUNTER`),
    /// so parallel tests never interfere. The file is removed on `Drop`.
    ///
    /// `PRAGMA journal_mode=WAL` is applied, matching production.
    pub fn open_in_memory_test() -> Result<Self> {
        let n = TEST_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join("kb_db_tests");
        std::fs::create_dir_all(&dir).map_err(|e| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(-1),
                Some(format!("create_dir_all failed: {e}")),
            )
        })?;
        let path = dir.join(format!("kb-test-{n}.db"));
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Db { conn, owned_temp_db: Some(path) })
    }

    /// Open the DB at its *projected* location under
    /// `~/.local/share/knowledgebase/`. Exposed as a constructor so
    /// tests (M0) and the CLI (M7) share one code path.
    pub fn open_default() -> Result<Db> {
        let dir = dirs::data_local_dir()
            .ok_or_else(|| rusqlite::Error::InvalidPath(PathBuf::from(".")))?
            .join("knowledgebase");
        std::fs::create_dir_all(&dir).map_err(|e| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(-1),
                Some(format!("create_dir_all failed: {e}")),
            )
        })?;
        Self::open(&dir.join("kb.db"))
    }

    /// Minimal M0 schema: one non-derivable state table (`roots`).
    /// The rest (pages, page_tags, page_gates, links, tag_aliases)
    /// arrive in M1 along with their migration path.
    pub fn schema_v0(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS roots (
              path           TEXT PRIMARY KEY,   -- the registered root path (canonical)
              added_at       TEXT,               -- ISO-8601
              extra_exts     TEXT,               -- per-root `--ext` add-ons, '\\n'-joined
              include_hidden INTEGER DEFAULT 0   -- 0 = skip dot-directories (default)
            );
            ",
        )
    }

    /// M0 smoke: add a root to `roots`. Returns the row count so the
    /// caller can assert on it.
    ///
    /// Assigns a monotonically-increasing `seq` (most-recent-first) so the
    /// dedupe tiebreak in `walk_snapshot::winning_root` is well-defined:
    /// re-registering a root bumps its `seq` above the old one.
    pub fn add_root(&self, path: &str, extra_exts: Option<&str>, include_hidden: bool) -> Result<usize> {
        let next_seq: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM roots",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(1);
        self.conn.execute(
            "INSERT INTO roots (path, added_at, extra_exts, include_hidden, seq)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (path) DO UPDATE SET
                added_at = excluded.added_at,
                extra_exts = excluded.extra_exts,
                include_hidden = excluded.include_hidden,
                seq = excluded.seq",
            (path, chrono_now_iso8601(), extra_exts, include_hidden as i32, next_seq),
        )
    }

    /// M0 smoke: list the roots. Empty vec if the table is empty.
    pub fn list_roots(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM roots ORDER BY path")?;
        let out = stmt.query_map([], |r| r.get::<_, String>(0))?
                      .collect::<Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Drop a registered root (PLAN §5 `kb drop-root`). The *root row and
    /// its walk options* are removed; page rows registered under that root
    /// survive if they are still under another remaining root (the GC
    /// happens on the next `refresh`).
    pub fn drop_root(&self, path: &str) -> Result<usize> {
        self.conn.execute(
            "DELETE FROM roots WHERE path = ?1",
            rusqlite::params![path],
        )
    }

    /// Apply the full M5 schema (roots + pages + page_gates + page_tags +
    /// meta + tag_aliases + links + pages_fts + the FTS5 sync triggers),
    /// idempotently. Called once by the CLI on startup; `open` /
    /// `open_in_memory_test` do NOT call it automatically, so the M0
    /// smoke tests (which create their own minimal schema) stay green.
    pub fn migrate(&self) -> Result<()> {
        migrate::migrate(&self.conn)
    }

    /// The current schema version tag (from `meta.version`), or `None` if
    /// the DB has not been migrated yet.
    pub fn schema_version(&self) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        let v: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key='version'", [], |r| r.get(0))
            .optional()?;
        Ok(v)
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        // Drop the connection first (closes the file), then remove the
        // temp DB + its WAL/SHM siblings. `owned_temp_db.is_none()` keeps
        // production DBs intact.
        {
            let owned = self.owned_temp_db.clone();
            self.owned_temp_db = None;
            if let Some(path) = owned {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(path.with_file_name(
                    format!("{}.wal", path.file_name().and_then(|f| f.to_str()).unwrap_or("x.db")),
                ));
                let _ = std::fs::remove_file(path.with_file_name(
                    format!("{}.shm", path.file_name().and_then(|f| f.to_str()).unwrap_or("x.db")),
                ));
            }
        }
    }
}

#[inline]
fn chrono_now_iso8601() -> String {
    // M0 — no chrono dep yet. We just need a *monotonic-ish* time
    // string for the schema; real timestamp lands with the `chrono`
    // dep in M1 (same workspace — see Cargo.toml).
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}
