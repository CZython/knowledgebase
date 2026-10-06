//! refresh — reconcile the DB against the on-disk tree (PLAN §3 + §10
//! `refresh.rs`).
//!
//! Two entry points:
//!
//! - `refresh --full` (alias `kb full-rebuild`) — wipe every page row and
//!   re-index from scratch. Simple, idempotent, no drift-prone diff
//!   comparison. Called when the DB ever looks suspect or after a mass
//!   rename / restructure.
//! - `refresh` (incremental) — the fast path used routinely. Walks every
//!   root, indexes each file that is *new or changed* (by
//!   `(mtime_ns, content_hash)`), and GCs pages whose `abs_path` is no
//!   longer under any registered root. Files that are neither new nor
//!   changed are skipped entirely (no `build_page`, no SQL).
//!
//! Both honour the same dedupe rule: `winning_root` in `walk_snapshot`
//! picks which `(root, rel_path)` wins for an overlapping tree. A page that
//! is no longer under *any* registered root is GC'd (the §4
//! "drop-root but page survives if still under another root" contract is
//! enforced by *which roots are in `roots`*).

use crate::index::{self, IndexOutcome};
use crate::walk_snapshot;
use crate::Db;
use rusqlite::{params, Result, OptionalExtension};
use std::fs;
use std::path::Path;

/// A single registered root, as used by the refresh walker.
struct RootRow {
    path: String,
    extra_exts: Vec<String>,
    include_hidden: bool,
    seq: i64,
}

/// The outcome of a refresh (for the CLI to surface as a summary).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RefreshSummary {
    /// Pages upserted (or re-indexed, for `--full`).
    pub indexed: usize,
    /// Pages skipped because they have no `type:` in frontmatter.
    pub skipped_no_type: usize,
    /// Pages skipped because the slug fallback chain produced nothing.
    pub skipped_missing_slug: usize,
    /// Pages GC'd (no longer under any root).
    pub gc: usize,
    /// Roots that were walked.
    pub roots_walked: usize,
    /// A human-readable one-line summary (the CLI shows this verbatim).
    pub note: String,
}

impl RefreshSummary {
    fn finish(mut self) -> Self {
        self.note = format!(
            "{} root(s) · {} page(s) indexed · {} skipped (no-type:{} / no-slug:{}) · {} GC'd",
            self.roots_walked,
            self.indexed,
            self.skipped_no_type + self.skipped_missing_slug,
            self.skipped_no_type,
            self.skipped_missing_slug,
            self.gc,
        );
        self
    }
}

/// List the currently-registered roots in (seq DESC) presentation order.
fn load_roots(db: &Db) -> Result<Vec<RootRow>> {
    let mut stmt = db
        .conn
        .prepare("SELECT path, extra_exts, include_hidden, seq FROM roots ORDER BY seq DESC")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(RootRow {
                path: r.get::<_, String>(0)?,
                extra_exts: r
                    .get::<_, Option<String>>(1)?
                    .map(|s| s.lines().map(|l| l.to_string()).filter(|l| !l.is_empty()).collect())
                    .unwrap_or_default(),
                include_hidden: r.get::<_, i32>(2)? != 0,
                seq: r.get::<_, i64>(3)?,
            })
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// The *candidate* set of files to index, as the union over all roots of
/// their `indexed_files`. The dedupe on `abs_path` is implicit (a `BTreeSet`
/// of canonical paths).
fn candidate_files(roots: &[RootRow]) -> std::collections::BTreeSet<String> {
    let mut out: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for r in roots {
        let root = Path::new(&r.path);
        let walk = match walk_snapshot::build_walk(root, r.extra_exts.clone()) {
            Ok(w) => w,
            Err(_) => continue, // root gone — `kb drop-root` surfaces this cleanly later
        };
        for f in walk_snapshot::indexed_files(&walk, r.include_hidden) {
            out.insert(f);
        }
    }
    out
}

/// Read a page's *last-indexed* fingerprint (`mtime_ns`, `content_hash`), if
/// it is already in the DB.
fn stored_fingerprint(db: &Db, abs_path: &str) -> Result<Option<(i64, String)>> {
    let row: Option<(i64, String)> = db
        .conn
        .query_row(
            "SELECT mtime_ns, content_hash FROM pages WHERE abs_path = ?1",
            params![abs_path],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    Ok(row)
}

/// Cheaply re-stat a file on disk for its current fingerprint. `None` if the
/// file has vanished since the walk (racy — treat as "needs indexing"; the
/// index step will produce `NoType` / `Read` errors and the outcome will
/// count the reason; if the file is truly gone, the GC step below catches
/// it on the *next* refresh).
fn disk_fingerprint(path: &Path) -> Option<(i64, String)> {
    let meta = fs::metadata(path).ok()?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let bytes = fs::read(path).ok()?;
    Some((mtime_ns, index::etag_of(&bytes)))
}

/// Incremental refresh: index new/changed pages, GC pages no longer under
/// any root. Fast path when the tree is stable.
pub fn refresh(db: &Db) -> Result<RefreshSummary> {
    let roots = load_roots(db)?;
    let mut summ = RefreshSummary {
        note: String::new(),
        ..Default::default()
    };
    if roots.is_empty() {
        // Nothing registered → GC whatever is in `pages` (defensive: a
        // fresh DB has none; a DB whose roots were all dropped clears).
        summ.gc = gc_orphaned_pages(db, &[])?;
        summ.roots_walked = 0;
        return Ok(summ.finish());
    }

    let candidates = candidate_files(&roots);
    let mut outcome = IndexOutcome::new();
    let roots_for_pick: Vec<(String, i64)> = roots
        .iter()
        .map(|r| (r.path.clone(), r.seq))
        .collect();
    let kept: Vec<String> = candidates.iter().cloned().collect();
    for abs in &candidates {
        let path = Path::new(abs);
        // Fast skip: unchanged (mtime_ns + content_hash match)? Don't
        // even read the file. The §3 contract is "reconcile by
        // (mtime_ns, content_hash)" — both must match.
        if let Ok(Some((ms, ch))) = stored_fingerprint(db, abs) {
            if let Some((ds, dh)) = disk_fingerprint(path) {
                if ds == ms && dh == ch {
                    continue;
                }
            }
        }
        // Slow path: determine the winning root and upsert. The walk above
        // already told us *that* the file is indexable given each root's
        // options; we still re-derive `(root, rel)` via `winning_root`
        // because the winner is a *function of the full registered-root
        // set* (not just the root whose walk admitted the file).
        match walk_snapshot::winning_root(&roots_for_pick, abs) {
            Some((root, rel)) => {
                index::upsert_or_skip(db, path, &root, &rel, &mut outcome)?;
            }
            None => {
                // The file is a *candidate* (some root's walk admitted it)
                // but no registered root contains it — impossible under
                // `candidate_files` (a candidate is only produced from a
                // walk of a registered root, and `is_under` holds).
                continue;
            }
        }
    }

    // GC: any `pages.abs_path` not in `candidates` is no longer under any
    // root (or the file was deleted but a stale row lingers). Clear it.
    let gc = gc_orphaned_pages(db, &kept)?;

    summ.indexed = outcome.upserted;
    summ.skipped_no_type = outcome.skipped_no_type;
    summ.skipped_missing_slug = outcome.skipped_missing_slug;
    summ.gc = gc;
    summ.roots_walked = roots.len();
    Ok(summ.finish())
}

/// Delete any page row whose `abs_path` is NOT in `keep`, along with its
/// row-table entries (gates / tags / links) — in a single transaction.
fn gc_orphaned_pages(db: &Db, keep: &[String]) -> Result<usize> {
    let conn = &db.conn;
    // Collect the page rows *before* entering the transaction, in a scoping
    // block so the `Statement`'s borrow of `conn` is fully ended when we
    // mutate the connection via `tx`.
    let rows: Vec<String> = {
        let mut stmt = conn.prepare("SELECT abs_path FROM pages")?;
        let it = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let collected: Result<Vec<String>> = it.collect();
        
        collected?
    };

    let tx = conn.unchecked_transaction()?;
    let mut n: usize = 0;
    for abs in &rows {
        if keep.contains(abs) {
            continue;
        }
        tx.execute("DELETE FROM page_gates WHERE abs_path = ?1", params![abs])?;
        tx.execute("DELETE FROM page_tags  WHERE abs_path = ?1", params![abs])?;
        tx.execute("DELETE FROM links     WHERE from_path = ?1", params![abs])?;
        tx.execute("DELETE FROM pages     WHERE abs_path = ?1", params![abs])?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// Full rebuild: wipe every page row, then re-index from scratch. Called by
/// `kb full-rebuild` (alias `kb refresh --full`).
pub fn full_rebuild(db: &Db) -> Result<RefreshSummary> {
    let roots = load_roots(db)?;
    let mut summ = RefreshSummary {
        ..Default::default()
    };
    summ.roots_walked = roots.len();

    // Wipe everything first (the row-tables, then pages). The FTS5 triggers
    // keep `pages_fts` in sync on the DELETEs, so the index rebuilds clean.
    let tx = db.conn.unchecked_transaction()?;
    tx.execute("DELETE FROM page_gates", [])?;
    tx.execute("DELETE FROM page_tags", [])?;
    tx.execute("DELETE FROM links", [])?;
    let wiped = tx.execute("DELETE FROM pages", [])?;
    tx.commit()?;
    summ.gc = wiped;

    // Only re-index if there are roots to walk; an empty-root DB stays empty
    // (the wipe above is the "full rebuild" of an empty corpus).
    if !roots.is_empty() {
        candidates_to_records(db, &roots, &mut summ);
    }
    Ok(summ.finish())
}

/// Walk every root, build the candidate set, and upsert every candidate.
/// Used by `full_rebuild` (which wipes `pages` first, so this is a clean
/// "wipe + rewrite").
fn candidates_to_records(db: &Db, roots: &[RootRow], summ: &mut RefreshSummary) {
    let candidates = candidate_files(roots);
    let mut outcome = IndexOutcome::new();
    let roots_for_pick: Vec<(String, i64)> = roots
        .iter()
        .map(|r| (r.path.clone(), r.seq))
        .collect();
    for abs in candidates {
        let path = Path::new(&abs);
        if let Some((root, rel)) = walk_snapshot::winning_root(&roots_for_pick, &abs) {
            if let Err(e) = index::upsert_or_skip(db, path, &root, &rel, &mut outcome) {
                eprintln!("kb::full_rebuild: upsert {abs}: {e}");
            }
        }
    }
    summ.indexed = outcome.upserted;
    summ.skipped_no_type = outcome.skipped_no_type;
    summ.skipped_missing_slug = outcome.skipped_missing_slug;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate;
    use std::fs;

    fn fresh_db() -> Db {
        let db = Db::open_in_memory_test().unwrap();
        migrate::migrate(&db.conn).unwrap();
        db
    }

    #[test]
    fn full_rebuild_indexes_a_tree() {
        let dir = std::env::temp_dir().join("kbdb_rr_a");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("papers")).unwrap();
        fs::write(
            dir.join("a.md"),
            "---\ntype: note\ntitle: A\n---\n# A\nbody a\n",
        )
        .unwrap();
        fs::write(
            dir.join("papers/p1.md"),
            "---\ntype: wiki\ntitle: P1\ntags:\n  - llm\n---\n# P1\nwiki p1 body\n",
        )
        .unwrap();
        fs::write(dir.join("no-type.md"), "no frontmatter at all\n").unwrap();

        let db = fresh_db();
        let n = db
            .add_root(
                &std::fs::canonicalize(&dir).unwrap().to_string_lossy(),
                None,
                false,
            )
            .unwrap();
        assert_eq!(n, 1);

        let summ = full_rebuild(&db).unwrap();
        assert_eq!(summ.indexed, 2, "two indexable pages");
        assert_eq!(summ.skipped_no_type, 1, "one no-type page skipped");
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn incremental_skips_unchanged() {
        let dir = std::env::temp_dir().join("kbdb_rr_b");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("n.md");
        fs::write(&p, "---\ntype: note\ntitle: N\n---\nbody\n").unwrap();

        let db = fresh_db();
        db.add_root(
            &std::fs::canonicalize(&dir).unwrap().to_string_lossy(),
            None,
            false,
        )
        .unwrap();
        let s1 = refresh(&db).unwrap();
        assert_eq!(s1.indexed, 1);

        // no change → nothing to do
        let s2 = refresh(&db).unwrap();
        assert_eq!(s2.indexed, 0, "unchanged content must be skipped entirely");

        // change the *content* → re-index
        fs::write(&p, "---\ntype: note\ntitle: N v2\ndescription: updated\n---\nbody v2\n")
            .unwrap();
        let s3 = refresh(&db).unwrap();
        assert_eq!(s3.indexed, 1, "changed content must be re-indexed");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gc_drops_deleted_files() {
        let dir = std::env::temp_dir().join("kbdb_rr_c");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("n.md");
        fs::write(&p, "---\ntype: note\ntitle: N\n---\nbody\n").unwrap();

        let db = fresh_db();
        db.add_root(
            &std::fs::canonicalize(&dir).unwrap().to_string_lossy(),
            None,
            false,
        )
        .unwrap();
        let _ = refresh(&db).unwrap();
        // Now remove the file and re-refresh.
        fs::remove_file(&p).unwrap();
        let s2 = refresh(&db).unwrap();
        assert_eq!(s2.gc, 1, "orphaned page must be GC'd");
        let _ = fs::remove_dir_all(&dir);
    }
}
