//! migrate — idempotent schema creation (PLAN §8 / §10 `conn`/`migrate`).
//!
//! One logical schema version is applied in a single transaction; every
//! statement is `IF NOT EXISTS`-guarded (or a no-op drop/create), so
//! calling it repeatedly against a live DB is a no-op once applied.
//!
//! The schema mirrors PLAN §8 exactly:
//!
//! ```sql
//! roots / pages / page_gates / pages_fts / page_tags / meta / tag_aliases / links
//! ```
//!
//! + **three FTS5 sync triggers** (the §13a "M5 index/refresh code generates
//!   both" guarantee, made automatic): any INSERT/UPDATE/DELETE against `pages`
//!   keeps `pages_fts` (an external-content FTS5 index) in lockstep. The
//!   indexer therefore only ever writes `pages` — never `pages_fts` directly.
//!
//! `pages_fts` is external-content (`content='pages',
//! content_rowid='rowid'`), so it stores **only the index**; the real text
//! stays in `pages`. §13a confirmed this shape empirically.

use rusqlite::{Connection, Result};

/// Current schema version (recorded in `meta.version` for future upgrades).
pub const SCHEMA_VERSION: &str = "2";

/// D2: the *single owner* of the `pages` gate-phrase column name. The SQL
/// column literal used to be hand-typed in 4+ queries across `kb/main.rs` —
/// a rename was a multi-file edit with no compiler help. Query sites now
/// `format!` this in, so a future schema rename touches `SCHEMA_SQL` (below)
/// and this one const, and every query follows automatically. (The DDL in
/// `SCHEMA_SQL` stays a literal: it is the definition site.)
pub const COL_SEARCH_REQUIRED_PHRASE: &str = "search_required_phrase";

/// The complete v0 schema (PLAN §8 + FTS5 sync triggers). Idempotent.
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS roots (
  path           TEXT PRIMARY KEY,   -- the registered root path (as given; canonicalised at walk time)
  added_at       TEXT,               -- ISO-8601 (UTC)
  extra_exts     TEXT,               -- per-root `--ext` add-ons, '\n'-joined (default = .md only)
  include_hidden INTEGER DEFAULT 0,  -- 0 = skip dot-directories (default)
  seq            INTEGER NOT NULL DEFAULT 0  -- most-recent-first tiebreak for dedupe
);

CREATE TABLE IF NOT EXISTS pages (
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
CREATE INDEX IF NOT EXISTS idx_pages_id  ON pages(id);
CREATE INDEX IF NOT EXISTS idx_pages_type     ON pages(type);
CREATE INDEX IF NOT EXISTS idx_pages_language ON pages(language);
CREATE INDEX IF NOT EXISTS idx_pages_root     ON pages(root);

CREATE TABLE IF NOT EXISTS page_gates (
  abs_path TEXT NOT NULL,
  term     TEXT NOT NULL,
  PRIMARY KEY (abs_path, term)
);

CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(
  title, description, body, frontmatter_text,
  content='pages', content_rowid='rowid'
);

CREATE TABLE IF NOT EXISTS page_tags (
  abs_path TEXT NOT NULL,
  tag      TEXT NOT NULL,
  PRIMARY KEY (abs_path, tag)
);
CREATE INDEX IF NOT EXISTS idx_page_tags_tag ON page_tags(tag);

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tag_aliases (
  alias     TEXT PRIMARY KEY,
  canonical TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS links (
  from_path TEXT,
  to_path   TEXT,
  style     TEXT,
  PRIMARY KEY (from_path, to_path, style)
);
CREATE INDEX IF NOT EXISTS idx_links_to ON links(to_path);

-- ==== FTS5 sync (external-content) — three triggers keep `pages_fts` in
--      lockstep with `pages`. `pages` is the *only* table the indexer
--      writes; these fire on every INSERT/UPDATE/DELETE and translate the
--      change into the `pages_fts` 'delete'/'insert' pair FTS5 requires.
DROP TRIGGER IF EXISTS pages_ai;
CREATE TRIGGER pages_ai AFTER INSERT ON pages BEGIN
  INSERT INTO pages_fts(rowid, title, description, body, frontmatter_text)
  VALUES (new.rowid, new.title, new.description, new.body, new.frontmatter_text);
END;
DROP TRIGGER IF EXISTS pages_au;
CREATE TRIGGER pages_au AFTER UPDATE ON pages BEGIN
  INSERT INTO pages_fts(pages_fts, rowid, title, description, body, frontmatter_text)
  VALUES ('delete', old.rowid, old.title, old.description, old.body, old.frontmatter_text);
  INSERT INTO pages_fts(rowid, title, description, body, frontmatter_text)
  VALUES (new.rowid, new.title, new.description, new.body, new.frontmatter_text);
END;
DROP TRIGGER IF EXISTS pages_ad;
CREATE TRIGGER pages_ad AFTER DELETE ON pages BEGIN
  INSERT INTO pages_fts(pages_fts, rowid, title, description, body, frontmatter_text)
  VALUES ('delete', old.rowid, old.title, old.description, old.body, old.frontmatter_text);
END;
"#;

/// Apply the schema to `conn`. Idempotent — safe to call on every open.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES ('version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![SCHEMA_VERSION],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_idempotent() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap(); // second apply must be a no-op
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(n >= 8, "expected full schema, got {n} tables");
        let v: String = conn
            .query_row("SELECT value FROM meta WHERE key='version'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }
}
