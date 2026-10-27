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

/// The `pages` columns the schema guarantees to exist. `migrate` reconciles
/// a *pre-existing* `pages` table to this column set. `CREATE TABLE
/// IF NOT EXISTS` only creates the table when it's absent — it never touches
/// the *columns* of a table an older version already created, which is why a
/// DB built before a column rename/addition fails with `table pages has no
/// column named search_required_phrase` on the first write. This list is the
/// single source of truth for "what columns must be present"; adding a
/// column to the schema means adding it to the DDL *and* to this list (the
/// second is what migrates live databases).
const PAGES_COLUMNS: &[&str] = &[
    "abs_path",
    "id",
    "type",
    "root",
    "rel_path",
    "title",
    "language",
    "description",
    "search_required_phrase",
    "frontmatter_text",
    "body",
    "mtime_ns",
    "content_hash",
    "created_at",
    "updated_at",
];

/// Column *renames* to apply, `(old_name, new_name)`, in order. A rename is
/// distinct from an addition: the underlying column already exists carrying
/// live data, so it must be **`ALTER TABLE … RENAME COLUMN`**'d (data
/// preserved) rather than dropped and re-added. SQLite ≥ 3.25 rewrites the
/// references in any triggers/views/indexes that name the column, so the
/// FTS5 sync triggers survive it. The one rename in this codebase's history
/// is `search_terms` → `search_required_phrase` (`d38340e`, SCHEMA_VERSION
/// 1→2 — the authors assumed "no on-disk DBs to migrate"; a live DB proves
/// otherwise). A live DB on that column is exactly your `refresh` /
/// `full-rebuild` "table pages has no column named search_required_phrase"
/// failure.
const PAGES_COLUMN_RENAMES: &[(&str, &str)] = &[("search_terms", COL_SEARCH_REQUIRED_PHRASE)];

/// Reconcile a pre-existing `pages` table to the current column set:
/// 1. **Rename** each `(old, new)` in [`PAGES_COLUMN_RENAMES`] when the old
///    name is present and the new name is not — data-preserving.
/// 2. **Add** any column in [`PAGES_COLUMNS`] still absent after the renames
///    — for columns genuinely *added* (no prior column to rename into) and
///    to future-proof the reconcile.
///
/// Each added column is nullable `TEXT`, matching the DDL's convention for
/// the optional columns — a row created before the column existed simply gets
/// `NULL`, exactly as a fresh insert of an optional field would. No-op when
/// the table isn't present yet (the `SCHEMA_SQL` DDL creates it with the
/// full column set) or when it already matches.
fn reconcile_pages_columns(conn: &Connection) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='pages')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(());
    }
    let mut current: Vec<String> = conn
        .prepare("PRAGMA table_info(pages)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<_>>()?;
    // Phase 1: renames — old present, new absent.
    for (old, new) in PAGES_COLUMN_RENAMES {
        let has_old = current.iter().any(|c| c == old);
        let has_new = current.iter().any(|c| c == new);
        if has_old && !has_new {
            // RENAME COLUMN rewrites `pages.<old>` inside the FTS5 triggers
            // (SQLite ≥ 3.25), keeping them in lockstep.
            conn.execute(
                &format!("ALTER TABLE pages RENAME COLUMN {old} TO {new}"),
                [],
            )?;
            if let Some(slot) = current.iter_mut().find(|c| c == old) {
                *slot = new.to_string();
            }
        }
    }
    // Phase 2: additions — anything in the guaranteed set still absent.
    for col in PAGES_COLUMNS {
        if !current.iter().any(|c| c == col) {
            conn.execute(&format!("ALTER TABLE pages ADD COLUMN {col} TEXT"), [])?;
            current.push(col.to_string());
        }
    }
    Ok(())
}

/// Apply the schema to `conn`. Idempotent — safe to call on every open —
/// and *upward*: a database created by an older version (whose `pages`
/// table predates a column rename/addition) is reconciled to the current
/// column set — renaming in place, preserving data — rather than failing
/// on the first write.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL)?;
    reconcile_pages_columns(conn)?;
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

    fn column_names(conn: &rusqlite::Connection) -> Vec<String> {
        conn.prepare("PRAGMA table_info(pages)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap()
    }

    /// The regression behind `kb: refresh: table pages has no column named
    /// search_required_phrase`: a DB from an older version carries the pre-
    /// rename `search_terms` column. `migrate` must **rename it in place** —
    /// data preserved — not drop it (CREATE TABLE IF NOT EXISTS never
    /// reconciles an existing table's columns). Crucially, a gate phrase that
    /// was indexed into the old column must *survive* under the new name, or
    /// the user's hidden-phrase gating silently breaks.
    #[test]
    fn migrate_renames_search_terms_to_search_required_phrase_preserving_data() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // The v0/v1 `pages` shape: identity + text columns, gate column under
        // its ORIGINAL name `search_terms` (commit d38340e renamed it).
        conn.execute_batch(
            "CREATE TABLE pages (
               abs_path TEXT NOT NULL,
               id       TEXT NOT NULL,
               type     TEXT NOT NULL,
               root     TEXT NOT NULL,
               rel_path TEXT NOT NULL,
               title    TEXT,
               language TEXT,
               description TEXT,
               search_terms TEXT,
               frontmatter_text TEXT,
               body     TEXT,
               mtime_ns INTEGER NOT NULL,
               content_hash TEXT NOT NULL,
               created_at TEXT,
               updated_at TEXT,
               PRIMARY KEY (abs_path)
             );
             INSERT INTO pages
               (abs_path, id, type, root, rel_path,
                search_terms, mtime_ns, content_hash)
             VALUES ('/root/a.md', 'note:a', 'note', '/root', 'a.md',
                'fanfic daughter of neunmann', 1, 'h');",
        )
        .unwrap();
        // Sanity: starts with the OLD name and NOT the new one.
        let before = column_names(&conn);
        assert!(before.iter().any(|c| c == "search_terms"));
        assert!(!before.iter().any(|c| c == COL_SEARCH_REQUIRED_PHRASE));
        // The live value in the old column that must survive the migration.
        let old_val: String = conn
            .query_row(
                "SELECT search_terms FROM pages WHERE abs_path='/root/a.md'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_val, "fanfic daughter of neunmann");

        migrate(&conn).unwrap();

        let after = column_names(&conn);
        assert!(
            after.iter().any(|c| c == COL_SEARCH_REQUIRED_PHRASE),
            "rename must produce the new column name; got: {after:?}"
        );
        assert!(
            !after.iter().any(|c| c == "search_terms"),
            "old name must be gone after rename; got: {after:?}"
        );
        // The data moved with the column and is now queryable by the new name
        // (i.e. the gate phrase is preserved, not stranded).
        let new_val: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT {COL_SEARCH_REQUIRED_PHRASE} FROM pages WHERE abs_path='/root/a.md'"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            new_val.as_deref(),
            Some("fanfic daughter of neunmann"),
            "renamed column must retain its prior value"
        );
        // Still idempotent on top of the reconcile (no error, no double-rename).
        migrate(&conn).unwrap();
    }

    /// The general reconcile: a `pages` table that predates a *genuinely
    /// added* optional column (no prior column to rename it from) must gain
    /// that column, with pre-existing rows reading NULL in it — the exact
    /// semantics of a fresh insert of an optional field.
    #[test]
    fn migrate_adds_missing_optional_pages_column() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // A `pages` shape missing *both* the gate column and one more
        // optional column (`description`), to exercise the additive path.
        conn.execute_batch(
            "CREATE TABLE pages (
               abs_path TEXT NOT NULL,
               id       TEXT NOT NULL,
               type     TEXT NOT NULL,
               root     TEXT NOT NULL,
               rel_path TEXT NOT NULL,
               title    TEXT,
               language TEXT,
               frontmatter_text TEXT,
               body     TEXT,
               mtime_ns INTEGER NOT NULL,
               content_hash TEXT NOT NULL,
               created_at TEXT,
               updated_at TEXT,
               PRIMARY KEY (abs_path)
             );
             INSERT INTO pages
               (abs_path, id, type, root, rel_path, mtime_ns, content_hash)
             VALUES ('/root/a.md', 'note:a', 'note', '/root', 'a.md', 1, 'h');",
        )
        .unwrap();
        let before = column_names(&conn);
        assert!(!before.iter().any(|c| c == "description"));
        assert!(!before.iter().any(|c| c == COL_SEARCH_REQUIRED_PHRASE));

        migrate(&conn).unwrap();

        let after = column_names(&conn);
        assert!(
            after.iter().any(|c| c == "description"),
            "additive reconcile must add 'description'; got: {after:?}"
        );
        assert!(
            after.iter().any(|c| c == COL_SEARCH_REQUIRED_PHRASE),
            "additive reconcile must add the gate column; got: {after:?}"
        );
        // Pre-existing rows survive; the newly-added optional columns read NULL.
        let (title, desc): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT title, description FROM pages WHERE abs_path='/root/a.md'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(
            title.is_none(),
            "untouched column stays as it was: {title:?}"
        );
        assert!(
            desc.is_none(),
            "newly-added column is NULL on a pre-existing row: {desc:?}"
        );
        migrate(&conn).unwrap(); // idempotent on top
    }

    /// A DB that is *already* on the current schema must migrate as a no-op
    /// for `pages` — no spurious renames or additions, existing data intact.
    #[test]
    fn migrate_is_a_noop_already_current_pages() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap(); // fresh + reconciled
        conn.execute(
            &format!(
                "INSERT INTO pages
                 (abs_path, id, type, root, rel_path, {COL_SEARCH_REQUIRED_PHRASE}, mtime_ns, content_hash)
                 VALUES ('/root/a.md', 'note:a', 'note', '/root', 'a.md', 'token bucket', 1, 'h')"
            ),
            [],
        )
        .unwrap();
        // Second migrate must not error, duplicate, or drop the value.
        migrate(&conn).unwrap();
        let names = column_names(&conn);
        assert_eq!(
            names
                .iter()
                .filter(|c| c.as_str() == COL_SEARCH_REQUIRED_PHRASE)
                .count(),
            1,
            "gate column must appear exactly once: {names:?}"
        );
        let v: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT {COL_SEARCH_REQUIRED_PHRASE} FROM pages WHERE abs_path='/root/a.md'"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v.as_deref(), Some("token bucket"));
    }
}
