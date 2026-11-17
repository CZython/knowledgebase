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
use crate::sql::{params, OptionalExtension, Result};
use crate::walk_snapshot;
use crate::Db;
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
    /// GC was skipped because at least one root failed to walk (its pages
    /// were left in place rather than GC'd against an incomplete candidate
    /// set.
    pub gc_skipped: bool,
    /// A human-readable one-line summary (the CLI shows this verbatim).
    pub note: String,
}

impl RefreshSummary {
    fn finish(mut self) -> Self {
        let base = format!(
            "{} root(s) · {} page(s) indexed · {} skipped (no-type:{} / no-slug:{}) · {} GC'd",
            self.roots_walked,
            self.indexed,
            self.skipped_no_type + self.skipped_missing_slug,
            self.skipped_no_type,
            self.skipped_missing_slug,
            self.gc,
        );
        self.note = if self.gc_skipped {
            format!("{base} · GC skipped (a root failed to walk — its pages were left in place)")
        } else {
            base
        };
        self
    }
}

/// List the currently-registered roots in (seq DESC) presentation order.
fn load_roots(db: &Db) -> Result<Vec<RootRow>> {
    let stmt = db
        .conn
        .prepare("SELECT path, extra_exts, include_hidden, seq FROM roots ORDER BY seq DESC")?;
    let rows = stmt
        .query_map((), |r| {
            Ok(RootRow {
                path: r.get::<_, String>(0)?,
                extra_exts: r
                    .get::<_, Option<String>>(1)?
                    .map(|s| {
                        s.lines()
                            .map(|l| l.to_string())
                            .filter(|l| !l.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
                include_hidden: r.get::<_, i32>(2)? != 0,
                seq: r.get::<_, i64>(3)?,
            })
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// The *candidate* set of files to index, as the union over all roots of
/// their `indexed_files`, together with the roots whose walk *failed*.
/// The dedupe on `abs_path` is implicit (a `BTreeSet` of canonical paths).
///
/// A root whose `build_walk` returns `Err` (a transient `EACCES`, a mid-`rename`
/// file, a flapping network mount) is reported in the second element and its
/// files are *absent* from the candidate set. Callers must not GC against that
/// set (a page under a failed root would look orphaned even though the file is
/// still on disk) — that is the data-loss window.
fn candidate_files(roots: &[RootRow]) -> (std::collections::BTreeSet<String>, Vec<String>) {
    let mut out: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut failed: Vec<String> = Vec::new();
    for r in roots {
        let root = Path::new(&r.path);
        let walk = match walk_snapshot::build_walk(root, r.extra_exts.clone()) {
            Ok(w) => w,
            Err(_) => {
                failed.push(r.path.clone()); // root gone / unreadable — `kb drop-root` surfaces the gone case cleanly later
                continue;
            }
        };
        for f in walk_snapshot::indexed_files(&walk, r.include_hidden) {
            out.insert(f);
        }
    }
    (out, failed)
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

    let (candidates, failed_roots) = candidate_files(&roots);
    let mut outcome = IndexOutcome::new();
    let roots_for_pick: Vec<(String, i64)> =
        roots.iter().map(|r| (r.path.clone(), r.seq)).collect();
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
    //
    // if ANY root failed to walk, its files are absent
    // from `candidates` even though they are still on disk — GC'ing against
    // that set would delete those pages (silent, partial-index loss). Skip
    // the GC this run so a transient `EACCES` / mid-rename / flapping mount
    // never drops a page whose file is still present. The next clean refresh
    // will reconcile and GC normally.
    let mut gc = 0usize;
    if failed_roots.is_empty() {
        gc = gc_orphaned_pages(db, &kept)?;
    } else {
        summ.gc_skipped = true;
    }

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
    // Collect the page rows *and* their section-row keys (uuids) up front.
    // `sections` is keyed on `page_uuid`, but the GC walks `abs_path` — so
    // each orphan's uuid is resolved *before* the page row is deleted (a
    // `SELECT` and a `DELETE` must not hold a `conn` borrow across each other).
    let rows: Vec<(String, Option<String>)> = {
        let stmt = conn.prepare("SELECT abs_path, uuid FROM pages")?;
        let it = stmt.query_map((), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        it.collect::<Result<Vec<_>>>()?
    };

    let tx = conn.unchecked_transaction()?;
    let mut n: usize = 0;
    for (abs, uuid) in &rows {
        if keep.contains(abs) {
            continue;
        }
        if let Some(uuid) = uuid {
            tx.execute("DELETE FROM sections   WHERE page_uuid = ?1", params![uuid])?;
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

    // Wipe everything first (the row-tables, then pages). Turso's FTS index
    // follows the `pages` DELETEs, so the index rebuilds clean.
    let tx = db.conn.unchecked_transaction()?;
    // §v3: the `sections` rows (keyed on `page_uuid`, not `abs_path`) before
    // the `pages` they reference; same "explicit delete, no CASCADE" contract.
    tx.execute("DELETE FROM sections", ())?;
    tx.execute("DELETE FROM page_gates", ())?;
    tx.execute("DELETE FROM page_tags", ())?;
    tx.execute("DELETE FROM links", ())?;
    let wiped = tx.execute("DELETE FROM pages", ())?;
    tx.commit()?;
    summ.gc = wiped;

    // Only re-index if there are roots to walk; an empty-root DB stays empty
    // (the wipe above is the "full rebuild" of an empty corpus).
    if !roots.is_empty() {
        // a partial wipe + partial re-index must not exit
        // 0 — a thinner corpus than we started with is a *failure*, so any
        // per-page upsert error is propagated (non-zero CLI exit).
        candidates_to_records(db, &roots, &mut summ)?;
    }
    Ok(summ.finish())
}

/// Walk every root, build the candidate set, and upsert every candidate.
/// Used by `full_rebuild` (which wipes `pages` first, so this is a clean
/// "wipe + rewrite").
///
/// returns `Err` if any page failed to upsert (so the
/// partial-wipe + partial-re-index is surfaced as a non-zero exit rather than
/// a silent thinner corpus). All per-page errors are collected and reported
/// together.
fn candidates_to_records(db: &Db, roots: &[RootRow], summ: &mut RefreshSummary) -> Result<()> {
    let (candidates, failed_roots) = candidate_files(roots);
    if !failed_roots.is_empty() {
        // A root we could not walk contributes no candidates; in a full
        // rebuild (which wipes first) those pages are simply not re-indexed.
        // Surface it so the operator knows the rebuild was partial.
        eprintln!(
            "kb::full_rebuild: warning — {} root(s) failed to walk (their pages were left un-indexed): {:?}",
            failed_roots.len(),
            failed_roots
        );
    }
    let mut outcome = IndexOutcome::new();
    let roots_for_pick: Vec<(String, i64)> =
        roots.iter().map(|r| (r.path.clone(), r.seq)).collect();
    let mut errors: Vec<String> = Vec::new();
    for abs in candidates {
        let path = Path::new(&abs);
        if let Some((root, rel)) = walk_snapshot::winning_root(&roots_for_pick, &abs) {
            if let Err(e) = index::upsert_or_skip(db, path, &root, &rel, &mut outcome) {
                errors.push(format!("upsert {abs}: {e}"));
            }
        }
    }
    summ.indexed = outcome.upserted;
    summ.skipped_no_type = outcome.skipped_no_type;
    summ.skipped_missing_slug = outcome.skipped_missing_slug;
    if !errors.is_empty() {
        return Err(crate::sql::Error::SqliteFailure(
            crate::sql::ffi::Error::new(-1),
            Some(format!(
                "kb::full_rebuild: {} page(s) failed to index: {}",
                errors.len(),
                errors.join("; ")
            )),
        ));
    }
    Ok(())
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
            .query_row("SELECT COUNT(*) FROM pages", (), |r| r.get(0))
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
        fs::write(
            &p,
            "---\ntype: note\ntitle: N v2\ndescription: updated\n---\nbody v2\n",
        )
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

    #[test]
    fn gc_skipped_when_a_root_fails_to_walk() {
        // with two registered roots, if one becomes
        // unreadable (its path disappears) mid-session, the GC must be
        // SKIPPED — not run against a candidate set that is missing that
        // root's files. Otherwise the surviving root's pages are fine but the
        // failed root's pages look "orphaned" and get DELETEd even though the
        // files are still on disk.
        let base = std::env::temp_dir().join("kbdb_rr_d");
        let _ = fs::remove_dir_all(&base);
        let root_a = base.join("rootA");
        let root_b = base.join("rootB");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        fs::write(
            root_a.join("a.md"),
            "---\ntype: note\ntitle: A\n---\nbody a\n",
        )
        .unwrap();
        fs::write(
            root_b.join("b.md"),
            "---\ntype: note\ntitle: B\n---\nbody b\n",
        )
        .unwrap();

        let db = fresh_db();
        db.add_root(
            &fs::canonicalize(&root_a).unwrap().to_string_lossy(),
            None,
            false,
        )
        .unwrap();
        db.add_root(
            &fs::canonicalize(&root_b).unwrap().to_string_lossy(),
            None,
            false,
        )
        .unwrap();

        let s1 = refresh(&db).unwrap();
        assert_eq!(s1.indexed, 2, "both pages indexed");
        assert!(!s1.gc_skipped);

        // Deterministic "transient walk error": rootB's directory vanishes
        // (a permission flip / mid-rename / flapping mount). rootB's
        // `build_walk` now returns Err → it is a failed root.
        let root_b_renamed = base.join("rootB.away");
        fs::rename(&root_b, &root_b_renamed).unwrap();

        let s2 = refresh(&db).unwrap();
        assert!(
            s2.gc_skipped,
            "GC must be skipped when a root failed to walk"
        );
        assert_eq!(
            s2.gc, 0,
            "no pages GC'd against an incomplete candidate set"
        );

        // Both pages survive (a.md under the clean root, b.md under the failed
        // root). b.md's file is still on disk (now at rootB.away), so it must
        // NOT have been deleted from the index.
        let b_present: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM pages WHERE title = 'B'", (), |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            b_present, 1,
            "the failed root's page must survive a skipped GC"
        );
        let a_present: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM pages WHERE title = 'A'", (), |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(a_present, 1);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn full_rebuild_propagates_per_page_errors() {
        // a partial wipe + partial re-index (e.g. one
        // file unreadable mid-run) must return `Err` so the CLI exits
        // non-zero — not silently succeed with a thinner corpus.
        let dir = std::env::temp_dir().join("kbdb_rr_e");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let ok_file = dir.join("ok.md");
        let bad_file = dir.join("bad.md");
        fs::write(&ok_file, "---\ntype: note\ntitle: OK\n---\nbody ok\n").unwrap();
        fs::write(&bad_file, "---\ntype: note\ntitle: BAD\n---\nbody bad\n").unwrap();

        let db = fresh_db();
        db.add_root(
            &fs::canonicalize(&dir).unwrap().to_string_lossy(),
            None,
            false,
        )
        .unwrap();

        // Make bad_file unreadable (deterministic upsert failure).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bad_file, fs::Permissions::from_mode(0o000)).unwrap();
        }

        // full_rebuild must now surface the per-page error.
        let res = full_rebuild(&db);
        #[cfg(unix)]
        {
            let _ = res.expect_err("an unreadable file should make full_rebuild fail");
        }
        #[cfg(not(unix))]
        let _ = res;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bad_file, fs::Permissions::from_mode(0o644)).unwrap();
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_rebuild_surviving_root_reindexed_failed_root_surfaced() {
        // (full_rebuild half): `full_rebuild` wipes *all*
        // pages up-front and then re-indexes, so a root that fails to walk
        // has its pages wiped and simply not re-indexed — the rebuild is
        // partial. The contract: (a) full_rebuild still returns Ok (a walk
        // failure is surfaced as a warning, not a per-page `Err`), (b) the
        // surviving root's page is present, (c) the failed root's page is
        // absent (wiped, not re-indexed). This pins the asymmetry so a future
        // change can't silently let a partial rebuild exit as if it were whole.
        let base = std::env::temp_dir().join("kbdb_rr_f");
        let _ = fs::remove_dir_all(&base);
        let root_a = base.join("rootA");
        let root_b = base.join("rootB");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        fs::write(
            root_a.join("a.md"),
            "---\ntype: note\ntitle: A\n---\nbody a\n",
        )
        .unwrap();
        fs::write(
            root_b.join("b.md"),
            "---\ntype: note\ntitle: B\n---\nbody b\n",
        )
        .unwrap();

        let db = fresh_db();
        db.add_root(
            &fs::canonicalize(&root_a).unwrap().to_string_lossy(),
            None,
            false,
        )
        .unwrap();
        db.add_root(
            &fs::canonicalize(&root_b).unwrap().to_string_lossy(),
            None,
            false,
        )
        .unwrap();

        let s1 = full_rebuild(&db).unwrap();
        assert_eq!(s1.indexed, 2, "both pages indexed on a clean rebuild");

        // Turn rootB into a failed root (path vanishes → `build_walk` Err).
        let root_b_renamed = base.join("rootB.away");
        fs::rename(&root_b, &root_b_renamed).unwrap();

        let s2 = full_rebuild(&db).unwrap();
        // The surviving root re-indexed exactly one page…
        assert_eq!(s2.indexed, 1, "surviving root re-indexed");
        let a_present: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM pages WHERE title = 'A'", (), |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(a_present, 1, "surviving root's page present");
        // …while the failed root's page was wiped and not re-indexed.
        let b_present: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM pages WHERE title = 'B'", (), |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            b_present, 0,
            "full_rebuild wipes first, so the failed root's page is absent (partial rebuild)"
        );

        let _ = fs::remove_dir_all(&base);
    }
}
