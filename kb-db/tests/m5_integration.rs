//! M5 — `kb-db` indexer, end-to-end (PLAN §10 `walk_snapshot`/`index`/`refresh`).
//!
//! These are *feature* tests (not FTS5 probes — those live in `fts5.rs`):
//! a real temp tree is walked, indexed, searched, and reconciled. They pin
//! the M5 contract that M7 (the multi-file CLI) builds on:
//!
//! - `migrate` gives a full schema + working FTS5 *triggers* (so `INSERT`
//!   into `pages` is findable by `pages_fts MATCH` without manual index
//!   upserts — §13a "M5 index/refresh code generates both").
//! - `pages` / `page_gates` / `page_tags` / `links` rows are derived 1:1
//!   from the tree; pages with no `type:` are *skipped and counted*.
//! - nested/overlapping roots dedupe on `abs_path`; innermost / most-recent
//!   root wins `root`/`rel_path` (§4).
//! - `refresh` is incremental by `(mtime_ns, content_hash)`; deletes GC.
//! - `full_rebuild` is a clean wipe-and-rewrite.
//!
//! Run: `cargo test -p kb-db --test m5_integration`

use kb_db::index;
use kb_db::refresh::{full_rebuild, refresh};
use kb_db::walk_snapshot;
use kb_db::Db;
use rusqlite::OptionalExtension;
use std::fs;
use std::path::{Path, PathBuf};

/// A throwaway root + db. `Db::open_in_memory_test` gives a fresh temp-file
/// DB; `migrate` applies the full M5 schema.
struct Setup {
    db: Db,
    root: PathBuf,
}

impl Setup {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("kbdb_m5_{tag}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let db = Db::open_in_memory_test().unwrap();
        db.migrate().unwrap();
        Setup { db, root }
    }

    fn register(&self) {
        let p = fs::canonicalize(&self.root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        self.db.add_root(&p, None, false).expect("add_root");
    }

    fn root_str(&self) -> String {
        self.root.to_string_lossy().to_string()
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn count(db: &Db, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    db.conn.query_row(&sql, [], |r| r.get(0)).unwrap()
}

fn strings(db: &Db, sql: &str) -> Vec<String> {
    let mut stmt = db.conn.prepare(sql).unwrap();
    let it = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    it.collect::<Result<Vec<_>, _>>().unwrap()
}

/// Write a page at `root/rel` with the given frontmatter and body.
fn write_page(root: &Path, rel: &str, fm: &str, body: &str) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&p, format!("---\n{fm}---\n{body}")).unwrap();
}

#[test]
fn index_derives_all_row_tables_and_fts() {
    let s = Setup::new("derive");
    // A fully-specified note: title (slug source), language, description,
    // 2 tags, 2 search_required_phrase entries (a lone word + a quoted
    // phrase), body with a wikilink + a relative path ref.
    write_page(
        &s.root,
        "notes/note.md",
        "type: note\ntitle: Attention API\nlanguage: en\ndescription: how it works\ntags:\n  - llm\n  - mTLS\nsearch_required_phrase:\n  - mTLS\n  - \"token bucket\"\n",
        "# Attention API\n\nSee [[Transformer (architecture)]] and papers/attention.pdf for details.\n",
    );
    // A no-type page → skipped + counted.
    write_page(
        &s.root,
        "notes/loose.md",
        "",
        "just prose, no frontmatter type\n",
    );

    s.register();
    refresh(&s.db).unwrap();

    // 1 page indexed.
    assert_eq!(count(&s.db, "pages"), 1);

    // FTS is in sync via the *trigger* — search by description text.
    let hits = strings(
        &s.db,
        "SELECT p.id FROM pages_fts JOIN pages p ON p.rowid = pages_fts.rowid
           WHERE pages_fts MATCH 'attention' ORDER BY rank",
    );
    assert_eq!(
        hits,
        vec!["note:Attention API"],
        "trigger must keep FTS in sync"
    );

    // 2 tag rows.
    assert_eq!(count(&s.db, "page_tags"), 2);
    let tags = strings(&s.db, "SELECT tag FROM page_tags ORDER BY tag");
    assert_eq!(tags, vec!["llm".to_string(), "mTLS".to_string()]);

    // 2 gate rows, *unquoted* (the `"token bucket"` entry loses its quotes).
    assert_eq!(count(&s.db, "page_gates"), 2);
    let gates = strings(&s.db, "SELECT term FROM page_gates ORDER BY term");
    assert!(gates.contains(&"mTLS".to_string()));
    assert!(
        gates.contains(&"token bucket".to_string()),
        "quoted gate entry must be stored unquoted: {gates:?}"
    );

    // 2 link rows: 1 wikilink + 1 relative path.
    assert_eq!(count(&s.db, "links"), 2);
    let styles = strings(&s.db, "SELECT style FROM links ORDER BY style");
    assert_eq!(styles, vec!["relative".to_string(), "wikilink".to_string()]);
}

#[test]
fn no_type_pages_skipped_and_counted() {
    let s = Setup::new("skips");
    write_page(&s.root, "a.md", "", "no type here\n");
    write_page(
        &s.root,
        "b.md",
        "type: wiki\ntitle: Good\n",
        "# Good\nbody\n",
    );
    s.register();
    let summ = refresh(&s.db).unwrap();
    assert_eq!(summ.indexed, 1);
    assert_eq!(
        summ.skipped_no_type, 1,
        "no-type page must be counted as skipped"
    );
    assert_eq!(count(&s.db, "pages"), 1);
}

#[test]
fn nested_roots_dedupe_to_one_row_innermost_wins() {
    let s = Setup::new("nested");
    // `s.root` is the OUTER root; `s.root/sub` is the INNER (child) root.
    fs::create_dir_all(s.root.join("sub")).unwrap();
    // A page visible under *both* roots (in the shared `sub` subtree).
    write_page(
        &s.root,
        "sub/shared.md",
        "type: note\ntitle: Shared\n",
        "# Shared\nbody\n",
    );
    // A page only under outer.
    write_page(
        &s.root,
        "only_outer.md",
        "type: note\ntitle: Outer Only\n",
        "# O\nbody\n",
    );

    let outer_path = fs::canonicalize(&s.root)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let inner_path = fs::canonicalize(s.root.join("sub"))
        .unwrap()
        .to_string_lossy()
        .to_string();
    // Register outer first (lower seq), inner second (higher seq).
    s.db.add_root(&outer_path, None, false).unwrap();
    s.db.add_root(&inner_path, None, false).unwrap();

    refresh(&s.db).unwrap();

    // `shared.md` is under BOTH roots → exactly ONE row, and it points at the
    // *innermost* root (`sub`), NOT outer.
    assert_eq!(count(&s.db, "pages"), 2, "shared.md + only_outer.md");
    let (root, rel) =
        s.db.conn
            .query_row(
                "SELECT root, rel_path FROM pages WHERE title LIKE '%Shared%'",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .unwrap();
    let rootp = Path::new(&root);
    assert!(
        rootp.ends_with("sub"),
        "innermost root must win: got root={root}"
    );
    assert_eq!(rel, "shared.md");
}

#[test]
fn refresh_is_incremental_and_gc_reclaims_deletes() {
    let s = Setup::new("incr");
    write_page(&s.root, "a.md", "type: note\ntitle: A\n", "# A\nv1\n");
    write_page(
        &s.root,
        "to_die.md",
        "type: note\ntitle: Die\n",
        "# Die\nbody\n",
    );
    s.register();
    refresh(&s.db).unwrap();
    assert_eq!(count(&s.db, "pages"), 2);

    // 1. No change → nothing re-indexed.
    let same = refresh(&s.db).unwrap();
    assert_eq!(same.indexed, 0, "unchanged tree must index 0 rows");

    // 2. Change one page's content → exactly 1 re-indexed.
    write_page(
        &s.root,
        "a.md",
        "type: note\ntitle: A\ndescription: now updated\n",
        "# A\nv2 fresh\n",
    );
    let changed = refresh(&s.db).unwrap();
    assert_eq!(changed.indexed, 1, "changed page must be re-indexed");
    let desc: Option<String> =
        s.db.conn
            .query_row(
                "SELECT description FROM pages ORDER BY title LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
    assert_eq!(desc.as_deref(), Some("now updated"));

    // 3. Delete a page → GC'd on the next refresh.
    fs::remove_file(s.root.join("to_die.md")).unwrap();
    let gc = refresh(&s.db).unwrap();
    assert_eq!(gc.gc, 1, "deleted file's row must be GC'd");
    assert_eq!(count(&s.db, "pages"), 1);
}

#[test]
fn full_rebuild_wipes_and_rewrites() {
    let s = Setup::new("full");
    fs::create_dir_all(s.root.join("sub")).unwrap();
    write_page(&s.root, "a.md", "type: note\ntitle: A\n", "# A\nbody\n");
    write_page(&s.root, "sub/b.md", "type: wiki\ntitle: B\n", "# B\nbody\n");
    s.register();

    refresh(&s.db).unwrap();
    assert_eq!(count(&s.db, "pages"), 2);

    // Delete a file WITHOUT a refresh, then full-rebuild. The dead row must
    // be gone and the living one still present.
    fs::remove_file(s.root.join("sub/b.md")).unwrap();
    let summ = full_rebuild(&s.db).unwrap();
    assert_eq!(summ.indexed, 1, "…and re-indexes the survivor");
    assert_eq!(count(&s.db, "pages"), 1);
    let ids = strings(&s.db, "SELECT id FROM pages");
    assert_eq!(ids, vec!["note:A"], "only the surviving page remains");
}

#[test]
fn drop_root_surviving_under_another_root_keeps_the_page() {
    let outer = Setup::new("dropouter");
    // `outer.root/nested` is the inner root.
    fs::create_dir_all(outer.root.join("nested")).unwrap();
    write_page(
        &outer.root.join("nested"),
        "x.md",
        "type: note\ntitle: X\n",
        "# X\nbody\n",
    );

    let outer_path = fs::canonicalize(&outer.root)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let inner_path = fs::canonicalize(outer.root.join("nested"))
        .unwrap()
        .to_string_lossy()
        .to_string();

    outer.db.add_root(&outer_path, None, false).unwrap();
    outer.db.add_root(&inner_path, None, false).unwrap();
    refresh(&outer.db).unwrap();
    assert_eq!(count(&outer.db, "pages"), 1);

    // Drop the inner root; x.md is still under the OUTER root → survives.
    outer.db.drop_root(&inner_path).unwrap();
    let gc = refresh(&outer.db).unwrap();
    assert_eq!(gc.gc, 0, "page still under surviving root must NOT be GC'd");
    assert_eq!(count(&outer.db, "pages"), 1);

    // Drop the outer too → nothing covers x.md now → GC'd.
    outer.db.drop_root(&outer_path).unwrap();
    let gc2 = refresh(&outer.db).unwrap();
    assert_eq!(gc2.gc, 1, "no root covers the page now → GC'd");
    assert_eq!(count(&outer.db, "pages"), 0);
}

/// Regression: re-indexing a *single* page through `build_page` with placeholder
/// (empty) `root` / `rel_path` — exactly what the TUI `commit_edit` path does
/// (it re-reads the file to refresh the frontmatter columns, and does not know
/// the page's owning root) — must not clobber the row's stored `root`/`rel_path`.
/// Without the `upsert_page` empty-sentinel guard, `DO UPDATE SET root =
/// excluded.root` would overwrite the real value with `""`, breaking
/// `kb browse`/`kb tag` (which read `rel_path` and sort by `root, rel_path`)
/// and `kb stats`'s per-root page counts.
#[test]
fn reindex_single_page_preserves_root_and_rel_path() {
    let s = Setup::new("reindex_preserve");
    let abs = s.root.join("sub/alpha.md");
    fs::create_dir_all(s.root.join("sub")).unwrap();
    fs::write(&abs, "---\ntype: note\ntitle: Alpha\n---\n# Alpha\nbody\n").unwrap();

    // Register the root so the page is under it in the DB.
    let canon_root = fs::canonicalize(&s.root)
        .unwrap()
        .to_string_lossy()
        .to_string();
    s.db.add_root(&canon_root, None, false).unwrap();
    refresh(&s.db).unwrap();
    let abs_path = fs::canonicalize(&abs)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let (root, rel) = (
        strings(&s.db, &format!("SELECT root FROM pages WHERE abs_path = '{abs_path}'"))
            .pop()
            .expect("page row present"),
        strings(&s.db, &format!("SELECT rel_path FROM pages WHERE abs_path = '{abs_path}'"))
            .pop()
            .expect("rel_path present"),
    );
    assert!(!root.is_empty() && !rel.is_empty(), "precondition: row has root/rel");

    // The TUI commit path: build_page with empty placeholders, then upsert.
    let rec = index::build_page(&fs::canonicalize(&abs).unwrap(), "", "", false).unwrap();
    index::upsert_page(&s.db, &rec).unwrap();

    let (root2, rel2) = (
        strings(&s.db, &format!("SELECT root FROM pages WHERE abs_path = '{abs_path}'"))
            .pop()
            .expect("page row present"),
        strings(&s.db, &format!("SELECT rel_path FROM pages WHERE abs_path = '{abs_path}'"))
            .pop()
            .expect("rel_path present"),
    );
    assert_eq!(root2, root, "root must be preserved, not clobbered to ''");
    assert_eq!(rel2, rel, "rel_path must be preserved, not clobbered to ''");
}

#[test]
fn walk_snapshot_surfaces_only_indexable_files() {
    let s = Setup::new("surfaces");
    fs::create_dir_all(s.root.join(".git")).unwrap();
    write_page(
        &s.root,
        "keep.md",
        "type: note\ntitle: Keep\n",
        "# K\nbody\n",
    );
    fs::write(s.root.join(".git/hidden.md"), "x").unwrap(); // dot-dir, skipped
    fs::write(s.root.join("plain.txt"), "x").unwrap(); // ext not in allow-list

    let canon = fs::canonicalize(&s.root).unwrap();
    let walk = walk_snapshot::build_walk(&canon, vec![]).unwrap();
    let set = walk_snapshot::indexed_files(&walk, false);
    assert_eq!(set.len(), 1, "only keep.md is indexable by default rules");
    let only = set.iter().next().unwrap();
    assert!(only.ends_with("keep.md"), "got {only}");

    // The page is indexable → build_page succeeds.
    let p = Path::new(only);
    let rec = index::build_page(p, &s.root_str(), "keep.md", true).unwrap();
    assert_eq!(rec.page_type, "note");
    assert_eq!(rec.id, "note:Keep");
}
