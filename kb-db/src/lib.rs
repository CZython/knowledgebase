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
    /// `:memory:` is a valid path — the connection is opened as an in-memory
    /// database. Note that `PRAGMA journal_mode=WAL` is **not** meaningful on
    /// in-memory DBs (SQLite reports `journal_mode="memory"` and refuses
    /// `WAL`), so it is skipped when the path is `:memory:`.
    pub fn open(path: &Path) -> Result<Self> {
        let is_memory = path.to_string_lossy() == ":memory:";
        let conn = if is_memory {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        if !is_memory {
            conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        Ok(Db {
            conn,
            owned_temp_db: None,
        })
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
        Ok(Db {
            conn,
            owned_temp_db: Some(path),
        })
    }

    /// The *projected* default DB location
    /// (`~/<data-local>/knowledgebase/kb.db`) — the one place the path is
    /// built. `open_default` and the CLI's `cron` verb both derive from this so
    /// the emitted crontab line can never disagree with the DB the tool actually
    /// opens (F9: previously `cron` printed a hardcoded `~/.local/share/…` that
    /// was wrong on macOS, where `data_local_dir()` is
    /// `~/Library/Application Support`).
    pub fn default_db_path() -> Option<PathBuf> {
        Some(dirs::data_local_dir()?.join("knowledgebase").join("kb.db"))
    }

    /// Open the DB at its *projected* location under
    /// `~/.local/share/knowledgebase/`. Exposed as a constructor so
    /// tests (M0) and the CLI (M7) share one code path.
    pub fn open_default() -> Result<Db> {
        let path = Self::default_db_path()
            .ok_or_else(|| rusqlite::Error::InvalidPath(PathBuf::from(".")))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(-1),
                    Some(format!("create_dir_all failed: {e}")),
                )
            })?;
        }
        Self::open(&path)
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
    pub fn add_root(
        &self,
        path: &str,
        extra_exts: Option<&str>,
        include_hidden: bool,
    ) -> Result<usize> {
        let next_seq: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM roots", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(1);
        self.conn.execute(
            "INSERT INTO roots (path, added_at, extra_exts, include_hidden, seq)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (path) DO UPDATE SET
                added_at = excluded.added_at,
                extra_exts = excluded.extra_exts,
                include_hidden = excluded.include_hidden,
                seq = excluded.seq",
            (
                path,
                chrono_now_iso8601(),
                extra_exts,
                include_hidden as i32,
                next_seq,
            ),
        )
    }

    /// M0 smoke: list the roots. Empty vec if the table is empty.
    pub fn list_roots(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM roots ORDER BY path")?;
        let out = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Drop a registered root (PLAN §5 `kb drop-root`). The *root row and
    /// its walk options* are removed; page rows registered under that root
    /// survive if they are still under another remaining root (the GC
    /// happens on the next `refresh`).
    pub fn drop_root(&self, path: &str) -> Result<usize> {
        self.conn
            .execute("DELETE FROM roots WHERE path = ?1", rusqlite::params![path])
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
        // SECURITY_REPORT 3.15: an *unmigrated* DB has no `meta` table, so the
        // `SELECT … FROM meta` below would `SqliteFailure("no such
        // table:meta")` — and `.optional()?` only swallows `NoRows` (an empty
        // `Ok(None)`), not a table-missing error. Probe the catalog first and
        // return `None` (the documented "unmigrated" answer) when `meta` is
        // absent, instead of propagating `Err`.
        let meta_exists: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |r| r.get(0),
        )?;
        if meta_exists == 0 {
            return Ok(None);
        }
        let v: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key='version'", [], |r| {
                r.get(0)
            })
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
                let _ = std::fs::remove_file(path.with_file_name(format!(
                    "{}.wal",
                    path.file_name().and_then(|f| f.to_str()).unwrap_or("x.db")
                )));
                let _ = std::fs::remove_file(path.with_file_name(format!(
                    "{}.shm",
                    path.file_name().and_then(|f| f.to_str()).unwrap_or("x.db")
                )));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// SECURITY_REPORT 3.14: `Db::open(":memory:")` used to succeed at
    /// `Connection::open` but then fail at `pragma_update(None,
    /// "journal_mode", "WAL")` because an in-memory DB rejects the `WAL`
    /// value (SQLite reports `journal_mode = memory`). Skip the pragma on
    /// the `:memory:` path; the rest of the schema still works.
    #[test]
    fn open_memory_path_succeeds_and_migrates() {
        let db = Db::open(std::path::Path::new(":memory:"))
            .expect("`open(\":memory:\")` should succeed");
        db.migrate().expect("migrate on :memory: works");
        assert!(db.schema_version().is_ok());
    }

    /// SECURITY_REPORT 3.15: `schema_version()` on an *unmigrated* DB (no
    /// `meta` table) used to `Err` with `SqliteFailure("no such table:
    /// meta")`; the doc promises `Ok(None)` for "not-yet-migrated".
    #[test]
    fn schema_version_is_none_on_fresh_unmigrated_db() {
        let dir = std::env::temp_dir().join("kbdb_schema_version_3_15");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("fresh.db");
        let _ = std::fs::remove_file(&db_path);
        let db = Db::open(&db_path).unwrap();
        // `open` does NOT migrate automatically.
        assert_eq!(
            db.schema_version(),
            Ok(None),
            "unmigrated DB → Ok(None) (was Err before the fix)"
        );
        // And after `migrate`, it reports the version.
        db.migrate().unwrap();
        assert!(
            db.schema_version().unwrap().is_some(),
            "post-migrate schema_version should be Some(…)"
        );
    }
}
