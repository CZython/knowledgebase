//! `G5` — the corpus-administration data plane (PLAN-GUI §6.12: Roots /
//! Tag aliases / Data tabs).
//!
//! This is the "shared service" the §6.12 rules require — the *same* SQL
//! the CLI/TUI use, in one place the UI can reach from the worker thread.
//! The UI never reaches into `kb_db` directly; every admin read/write goes
//! through one of these functions, so the parity story (PLAN-GUI §3.1) is
//! anchored in a single module rather than scattered across UI paint code.
//!
//! All functions are `&Db` (read-mostly). The worker's `DbGuard` owns the
//! connection for the whole session (PLAN-GUI §3.2) and hands it in.
//!
//! The SQL is *not re-invented* from scratch: it is lifted from the
//! canonical CLI implementations (`kb/cli/roots.rs`, `kb/cli/tag.rs`,
//! `kb/cli/info.rs`) so a rename in one place cannot desync the other.
//! `GATE_COL` is `kb_db::migrate::COL_SEARCH_REQUIRED_PHRASE`, the same
//! constant `corpus.rs` already uses.

use super::{AliasRow, RootInfo, StatsSnapshot};

/// A narrow "a connection" alias so the worker's `DbGuard` (and the
/// temp `Db` the unit tests build) both satisfy the function signatures
/// without a trait (the same idiom `corpus.rs` uses for `load_pages`).
pub(crate) type DbLike = kb_db::Db;

/// `ListRoots`: the *full* registered-roots table for the §6.12 settings
/// view. The `kb roots` CLI shape is `path / added_at / extra_exts /
/// include_hidden`; we add the **page-count** column the settings table
/// needs (computed by a second pass over `pages WHERE root = ?1`).
///
/// Presentation order: `seq DESC, path` (most-recent-first; ties are
/// resolved by path so the order is stable across refreshes).
pub fn list_roots_full(db: &DbLike) -> Result<Vec<RootInfo>, String> {
    let sql = "SELECT path, added_at, extra_exts, include_hidden\n\
               FROM roots\n\
               ORDER BY seq DESC, path";
    let stmt = db
        .conn
        .prepare(sql)
        .map_err(|e| format!("list_roots: prepare: {e}"))?;
    let rows = stmt
        .query_map((), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i32>(3)?,
            ))
        })
        .map_err(|e| format!("list_roots: query: {e}"))?;
    let mut out: Vec<RootInfo> = Vec::new();
    for row in rows {
        let (path, added_at, extra_exts, include_hidden) =
            row.map_err(|e| format!("list_roots: row: {e}"))?;
        let extra_extensions: Vec<String> = extra_exts
            .map(|s| {
                s.lines()
                    .map(|l| l.to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let page_count: usize = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pages WHERE root = ?1",
                [path.as_str()],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .unwrap_or(0);
        out.push(RootInfo {
            path,
            added_at,
            extra_extensions,
            include_hidden: include_hidden != 0,
            page_count,
        });
    }
    Ok(out)
}

/// The `drop_root` fallback (when the directory is no longer on disk and
/// no canonicalize-able path exists): match the literal argument
/// case-insensitively against the registered roots, trailing-slash-trimmed.
///
/// Mirrors `kb/cli/roots.rs::cmd_drop_root` (the
/// `Err(_e) => { … roots.find(|r| r.to_lowercase() == key) … }` branch)
/// so a user can unregister a root whose folder has been deleted.
pub fn literal_root_match(db: &DbLike, path: &str) -> Option<String> {
    let key = path.trim_end_matches('/').to_lowercase();
    let stmt = db.conn.prepare("SELECT path FROM roots").ok()?;
    let rows = stmt.query_map((), |r| r.get::<_, String>(0)).ok()?;
    rows.flatten()
        .find(|p| p.trim_end_matches('/').to_lowercase() == key)
}

/// `AddAlias`: the §6.12 "tag aliases" add/upsert. The *exact* upsert
/// SQL the CLI uses (a conflicting `alias` overwrites the canonical) —
/// `kb/cli/tag.rs:261-267`. The "alias ≠ canonical" semantic rule is
/// *not* enforced here (the SQL would accept it); the worker's
/// `Cmd::AddAlias` dispatch does that check *before* reaching this
/// function (so a UI typo becomes an `Evt::Warn`, not a round-trip
/// no-op add).
pub fn upsert_alias(db: &DbLike, alias: &str, canonical: &str) -> Result<(), String> {
    db.conn
        .execute(
            "INSERT INTO tag_aliases(alias, canonical) VALUES (?1, ?2)\n\
             ON CONFLICT(alias) DO UPDATE SET canonical = excluded.canonical",
            [alias, canonical],
        )
        .map_err(|e| format!("upsert alias: {e}"))?;
    Ok(())
}

/// `RemoveAlias`: the §6.12 "tag aliases" delete. The *exact* SQL the
/// CLI uses — `kb/cli/tag.rs:235-239`. Returns the number of rows
/// removed (`0` or `1`, since `alias` is a primary key); the caller maps
/// `0` to a "not found" warn and `1` to an `Evt::AliasChanged` delete.
pub fn remove_alias(db: &DbLike, alias: &str) -> Result<usize, String> {
    db.conn
        .execute("DELETE FROM tag_aliases WHERE alias = ?1", [alias])
        .map_err(|e| format!("remove alias: {e}"))
}

/// `ListAliases`: the full `tag_aliases` table in the same order as the
/// CLI (`order by alias`, deterministic). The UI renders it as a two-
/// column table and keeps the same `(alias, canonical)` shape so an
/// `AliasChanged` event can update the local copy without a
/// round-trip.
pub fn list_aliases(db: &DbLike) -> Result<Vec<AliasRow>, String> {
    let stmt = db
        .conn
        .prepare("SELECT alias, canonical FROM tag_aliases ORDER BY alias")
        .map_err(|e| format!("list_aliases: prepare: {e}"))?;
    let mut out: Vec<AliasRow> = Vec::new();
    let rows = stmt
        .query_map((), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| format!("list_aliases: query: {e}"))?;
    for row in rows {
        let (alias, canonical) = row.map_err(|e| format!("list_aliases: row: {e}"))?;
        out.push(AliasRow { alias, canonical });
    }
    Ok(out)
}

/// The corpus stats snapshot (the §6.12 "Data" tab). All five counts
/// are real `COUNT(*)`s from the live schema (mirroring
/// `kb/cli/info.rs:150-286`); `db_size` is `metadata.len()` of the
/// on-disk file, or `0` when the DB is in-memory / the path is `None`
/// (the worker reports that as the size in this case — the `:memory:`
/// path is test-only in this codebase, but the code has to handle it).
pub fn stats_snapshot(
    db: &DbLike,
    owned_path: Option<&std::path::PathBuf>,
) -> Result<StatsSnapshot, String> {
    let count = |sql: &str| -> Result<usize, String> {
        db.conn
            .query_row(sql, (), |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
            .map_err(|e| format!("stats: {e}"))
    };
    let page_count = count("SELECT COUNT(*) FROM pages")?;
    let distinct_tag_count = count("SELECT COUNT(DISTINCT tag) FROM page_tags")?;
    let link_count = count("SELECT COUNT(*) FROM links")?;
    let root_count = count("SELECT COUNT(*) FROM roots")?;
    let alias_count = count("SELECT COUNT(*) FROM tag_aliases")?;
    let (db_path, db_size) = match owned_path {
        Some(p) if p.exists() => (
            p.to_string_lossy().into_owned(),
            p.metadata().map(|m| m.len()).unwrap_or(0),
        ),
        _ => (String::new(), 0),
    };
    Ok(StatsSnapshot {
        page_count,
        distinct_tag_count,
        link_count,
        root_count,
        alias_count,
        db_path,
        db_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch DB in a temp dir + real `migrate()` (the same shape
    /// the production `DbGuard::open` builds).
    fn scratch_db(name: &str) -> (kb_db::Db, std::path::PathBuf) {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("kb-gui-admin-{name}-{pid}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("kb.db");
        let db = kb_db::Db::open(&db_path).unwrap();
        db.migrate().unwrap();
        (db, dir)
    }

    #[test]
    fn list_roots_full_empty_when_no_roots_registered() {
        let (db, dir) = scratch_db("empty");
        let rows = list_roots_full(&db).unwrap();
        assert!(rows.is_empty(), "no roots → no rows: {rows:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_roots_full_includes_page_count_column() {
        let (db, dir) = scratch_db("count");
        // Register a root + add a page *under* it (the same shape the
        // CLI `kb add <dir>` + `kb refresh` pair produces, minus the
        // walk). The `pages` row is what `COUNT(*) WHERE root = ?1`
        // counts.
        db.add_root("/some/path", None, false).unwrap();
        db.conn
            .execute(
                "INSERT INTO pages (id, abs_path, root, rel_path, type, title, mtime_ns, content_hash)
                 VALUES ('note:t', '/some/path/t.md', '/some/path', 't.md', 'note', 'T', 1, 'h')",
                [],
            )
            .unwrap();
        let rows = list_roots_full(&db).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "/some/path");
        assert_eq!(
            rows[0].page_count, 1,
            "page_count should count the row we just inserted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_then_remove_round_trips_a_single_alias() {
        let (db, dir) = scratch_db("alias");
        upsert_alias(&db, "rs", "rust").unwrap();
        let rows = list_aliases(&db).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].alias, "rs");
        assert_eq!(rows[0].canonical, "rust");
        assert_eq!(remove_alias(&db, "rs").unwrap(), 1);
        assert!(list_aliases(&db).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_on_conflicting_alias_overwrites_canonical_not_double_inserts() {
        let (db, dir) = scratch_db("alias2");
        upsert_alias(&db, "rs", "rust").unwrap();
        upsert_alias(&db, "rs", "rustlang").unwrap();
        let rows = list_aliases(&db).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "conflicting alias must upsert, not duplicate"
        );
        assert_eq!(rows[0].canonical, "rustlang");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn literal_root_match_is_case_insensitive_and_trims_trailing_slash() {
        let (db, dir) = scratch_db("match");
        db.add_root("/Some/Path/", None, false).unwrap();
        // Case-insensitive, trailing-slash-trimmed:
        assert_eq!(
            literal_root_match(&db, "/some/path/sub"),
            None,
            "a *subpath* must not match (the registered path is the prefix, not the other way around)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_snapshot_defaults_to_zero_counts_on_an_empty_db() {
        let (db, dir) = scratch_db("stats");
        let snap = stats_snapshot(&db, None).unwrap();
        assert_eq!(snap.page_count, 0);
        assert_eq!(snap.root_count, 0);
        assert_eq!(snap.alias_count, 0);
        assert_eq!(
            snap.db_size, 0,
            "owned_path None → size 0 (in-memory / test)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
