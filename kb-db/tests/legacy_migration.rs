//! End-to-end regression: a database created by the old SQLite/FTS5 build
//! must open, migrate to the native Turso FTS index, and keep its data.
//!
//! The fixture is built with the system `sqlite3` if it is available (tests
//! are skipped otherwise), because Turso cannot create an FTS5 virtual table.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use kb_db::Db;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn legacy_db_path() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("kb_legacy_migration_{}_{n}.db", std::process::id()))
}

fn make_legacy_db(path: &Path) -> bool {
    let sql = r#"
CREATE TABLE roots (
  path           TEXT PRIMARY KEY,
  added_at       TEXT,
  extra_exts     TEXT,
  include_hidden INTEGER DEFAULT 0,
  seq            INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE pages (
  abs_path         TEXT NOT NULL,
  id               TEXT NOT NULL,
  type             TEXT NOT NULL,
  root             TEXT NOT NULL,
  rel_path         TEXT NOT NULL,
  title            TEXT,
  language         TEXT,
  description      TEXT,
  search_required_phrase TEXT,
  frontmatter_text TEXT,
  body             TEXT,
  mtime_ns         INTEGER NOT NULL,
  content_hash     TEXT NOT NULL,
  created_at       TEXT,
  updated_at       TEXT,
  PRIMARY KEY (abs_path)
);
CREATE VIRTUAL TABLE pages_fts USING fts5(
  title, description, body, frontmatter_text,
  content='pages', content_rowid='rowid'
);
CREATE TRIGGER pages_ai AFTER INSERT ON pages BEGIN
  INSERT INTO pages_fts(rowid, title, description, body, frontmatter_text)
  VALUES (new.rowid, new.title, new.description, new.body, new.frontmatter_text);
END;
CREATE TRIGGER pages_au AFTER UPDATE ON pages BEGIN
  INSERT INTO pages_fts(pages_fts, rowid, title, description, body, frontmatter_text)
  VALUES ('delete', old.rowid, old.title, old.description, old.body, old.frontmatter_text);
  INSERT INTO pages_fts(rowid, title, description, body, frontmatter_text)
  VALUES (new.rowid, new.title, new.description, new.body, new.frontmatter_text);
END;
CREATE TRIGGER pages_ad AFTER DELETE ON pages BEGIN
  INSERT INTO pages_fts(pages_fts, rowid, title, description, body, frontmatter_text)
  VALUES ('delete', old.rowid, old.title, old.description, old.body, old.frontmatter_text);
END;
INSERT INTO roots(path, added_at, extra_exts, include_hidden, seq)
  VALUES ('/legacy/root', NULL, NULL, 0, 0);
INSERT INTO pages
  (abs_path, id, type, root, rel_path, title, language, description,
   search_required_phrase, frontmatter_text, body, mtime_ns, content_hash,
   created_at, updated_at)
  VALUES
  ('/legacy/root/a.md', 'note:a', 'note', '/legacy/root', 'a.md', 'Alpha',
   NULL, NULL, NULL, 'type: note', 'alpha body text', 1, 'hash', NULL, NULL);
"#;

    matches!(
        Command::new("sqlite3").arg(path).arg(sql).status(),
        Ok(status) if status.success()
    )
}

#[test]
fn legacy_fts5_db_opens_and_keeps_data() {
    let path = legacy_db_path();
    if !make_legacy_db(&path) {
        eprintln!("skipping legacy migration test: sqlite3 with FTS5 unavailable");
        let _ = std::fs::remove_file(&path);
        return;
    }

    let db = Db::open(&path).expect("legacy DB must open after repair");
    db.migrate().expect("migration must succeed");
    db.migrate().expect("migration must be idempotent");

    let roots: Vec<String> = db
        .conn
        .prepare("SELECT path FROM roots")
        .unwrap()
        .query_map((), |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(roots, ["/legacy/root"], "roots must be preserved");

    let pages: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM pages", (), |r| r.get(0))
        .unwrap();
    assert_eq!(pages, 1, "pages must be preserved");

    let legacy: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='pages_fts'",
            (),
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(legacy, 0, "legacy FTS5 virtual table must be removed");

    let hits: Vec<String> = db
        .conn
        .prepare(
            "SELECT id FROM pages
             WHERE fts_match(title, description, body, frontmatter_text, 'alpha')",
        )
        .unwrap()
        .query_map((), |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(hits, ["note:a"], "native FTS must see existing rows");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(sidecar(&path, "-wal"));
    let _ = std::fs::remove_file(sidecar(&path, "-shm"));
    let _ = std::fs::remove_file(sidecar(&path, "-tshm"));
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
