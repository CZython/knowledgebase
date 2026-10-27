//! index — upsert one page into `pages` + `page_gates` + `page_tags` +
//! `links` (PLAN §10 `index.rs`).
//!
//! `pages_fts` is kept in lockstep **by the triggers installed in
//! `migrate`** — the indexer never touches `pages_fts` directly (§13a
//! "M5 index/refresh code generates both" is satisfied automatically).
//!
//! Per-page pipeline (drives exactly the pure `kb-core` surface):
//!
//! 1. read the file bytes;
//! 2. `frontmatter::parse` → `Frontmatter` (title?, description?, tags,
//!    search_required_phrase, frontmatter_text, body);
//! 3. **indexable?** — `fm.page_type.is_some()`. If not, the page is
//!    *skipped-and-counted* (the §3 warning, not an error);
//! 4. `identity::resolve_slug(fm.title, identity::find_h1(body),
//!    identity::filename_minus_extension(abs_path))` → the slug;
//! 5. `identity::display_id(kind, slug)` → the display id (`label:` /
//!    `wiki:` / `note:` + slug);
//! 6. gate (`page_gates`) + tags (`page_tags`) + links (`links`) rows per
//!    their PK contracts;
//! 7. `etag_of(bytes)` (FNV-1a 64, 16-hex) + `fs::metadata::modified` →
//!    `content_hash` / `mtime_ns`;
//! 8. `INSERT ... ON CONFLICT(abs_path) DO UPDATE` (single-statement
//!    upsert; the triggers keep `pages_fts` in sync).

use crate::Db;
use kb_core::frontmatter;
use kb_core::identity;
use kb_core::links::{self, Link, LinkStyle};
use kb_core::meta::NoteType;
use kb_core::tags as kb_tags;
use rusqlite::{params, Result};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single page, fully rendered, ready to upsert. All `Option` fields are
/// NULLs in the DB when `None`.
#[derive(Debug, Clone)]
pub struct PageRecord {
    pub abs_path: String,
    pub id: String,
    pub page_type: String,
    pub root: String,
    pub rel_path: String,
    pub title: Option<String>,
    pub language: Option<String>,
    pub description: Option<String>,
    /// Gate entries verbatim ('\n'-joined) — quotes **retained**; this is
    /// the display/FM view (PLAN §8 page.search_required_phrase).
    pub search_required_phrase_verbatim: Option<String>,
    pub frontmatter_text: Option<String>,
    pub body: String,
    pub mtime_ns: i64,
    pub content_hash: String,
    /// Per-row: the *unquoted* gate terms (PLAN §8 page_gates: "exact
    /// entry as written, minus surrounding double quotes").
    pub gate_terms: Vec<String>,
    /// Per-row: the tag strings (raw casing as written, already normalized
    /// by `kb_tags::normalize`).
    pub tag_rows: Vec<String>,
    /// Per-row: extracted links (deduped).
    pub link_rows: Vec<(String, LinkStyle)>, // (target, style)
}

/// The result of indexing one page (used by `IndexOutcome::SkippedNoType`
/// warnings downstream in the CLI).
pub struct IndexOutcome {
    pub upserted: usize,
    pub skipped_no_type: usize,
    pub skipped_missing_slug: usize,
    pub links_written: usize,
}

impl Default for IndexOutcome {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexOutcome {
    pub fn new() -> Self {
        IndexOutcome {
            upserted: 0,
            skipped_no_type: 0,
            skipped_missing_slug: 0,
            links_written: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.upserted + self.skipped_no_type + self.skipped_missing_slug == 0
    }
}

/// FNV-1a 64-bit, rendered as 16 lowercase hex chars (the `etag`).
/// Delegates to the single canonical implementation in `kb-core` so the
/// indexer and the CLI's optimistic-concurrency check can never drift.
pub fn etag_of(bytes: &[u8]) -> String {
    kb_core::etag::of(bytes)
}

/// The `page_type` string (lowercase — the §8 `pages.type` contract and the
/// `type:` query filter both assume this).
fn page_type_str(kind: &NoteType) -> &'static str {
    kind.prefix().strip_suffix(':').unwrap_or(kind.prefix())
}

/// Extract a `language:` scalar from the raw `frontmatter_text` block, if
/// present. The pure `kb-core::frontmatter::Frontmatter` struct does not
/// expose `language` (M1 focused on the identity pipeline); the field is
/// queryable per `search_grammar::FILTER_KEYS` (`type|language|tag`), so
/// the indexer surfaces it here with a minimal line scan rather than
/// touching kb-core.
///
/// Returns the trimmed, unquoted value, or `None` when the key is absent
/// or its value is empty.
fn extract_language(frontmatter_text: &str) -> Option<String> {
    for line in frontmatter_text.lines() {
        let t = line.trim();
        // Key at start of line, colon-terminated, value rest of line.
        // SECURITY_REPORT 3.10: require the character *immediately after* an
        // optional run of whitespace to be `:`. A bare `language` prefix
        // would otherwise also accept `languages: en, fr` (leaving value `s`)
        // and `language_x: …` (leaving value `x: …`), polluting the
        // `language:` search-filter column. The required boundary rejects
        // both.
        let rest = match t.strip_prefix("language") {
            Some(r) => r,
            None => continue,
        };
        let rest = rest.trim_start();
        let rest = match rest.strip_prefix(':') {
            Some(v) => v,
            None => {
                // `languages:` / `language_x:` — key boundary not met.
                continue;
            }
        };
        let v = rest.trim();
        let v = v
            .trim_start_matches('"')
            .trim_start_matches('\'')
            .trim_end_matches('"')
            .trim_end_matches('\'')
            .trim();
        if v.is_empty() {
            return None;
        }
        return Some(v.to_string());
    }
    None
}

/// Build a `PageRecord` for a file on disk. Returns `Err(IndexError::NoType)`
/// or `Err(IndexError::MissingSlug)` when the page cannot be indexed; `Ok`
/// carries a fully-populated record ready for `upsert_page`.
///
/// # Warnings the caller should surface
///
/// - `NoType` — the §3 warning ("report the count of skipped (no-type)
///   files as a warning, not an error").
/// - `MissingSlug` — the identity fallback chain (`title → H1 → filename`)
///   produced no non-empty slug. Logged by the caller.
/// - `description_too_long` — `fm.description_too_long`; the *drop* is
///   applied here (per §3), the caller just counts it.
pub fn build_page(
    abs_path: &Path,
    root: &str,
    rel_path: &str,
    include_language: bool,
) -> std::result::Result<PageRecord, IndexError> {
    let bytes = std::fs::read(abs_path).map_err(|e| IndexError::Read(e.to_string()))?;
    let text = String::from_utf8_lossy(&bytes);

    let fm = frontmatter::parse(&text);
    if !fm.is_indexable() {
        return Err(IndexError::NoType {
            type_raw: fm.type_raw.clone().unwrap_or_default(),
            unknown: fm.unknown_type_present,
        });
    }
    let kind = match fm.page_type {
        Some(k) => k,
        None => unreachable!("is_indexable ⇒ page_type.is_some()"),
    };
    let h1 = identity::find_h1(&fm.body);
    let filestem = identity::filename_minus_extension(abs_path.to_string_lossy().as_ref());
    let slug = identity::resolve_slug(fm.title.as_deref(), h1, filestem.as_deref())
        .map(|s| s.to_string())
        .ok_or(IndexError::MissingSlug)?;
    let page_type = page_type_str(&kind).to_string();
    let id = identity::display_id(kind.clone(), &slug);

    // Gate phrases: `frontmatter::search_required_phrase` carries verbatim
    // (quotes retained); the `page_gates` column requires the *unquoted* form
    // (PLAN §8 comment + §4 gate matching).
    let gate_terms: Vec<String> =
        kb_core_search_required_phrase_unquoted(&fm.search_required_phrase);
    let search_required_phrase_verbatim: Option<String> = {
        if fm.search_required_phrase.is_empty() {
            None
        } else {
            Some(fm.search_required_phrase.join("\n"))
        }
    };

    // description: the pure `Frontmatter::description_too_long` flag means
    // the indexer drops the *value* here (PLAN §3 "violations are warned
    // and the field is dropped; the page still indexes").
    let description = if fm.description_too_long {
        None
    } else {
        fm.description.clone()
    };

    let tags = kb_tags::normalize(&fm.tags);

    // Link extraction over the *body* (not frontmatter). `extract_links`
    // dedupes internally via `links::dedup` (called by the caller for the
    // DB's PK contract); we dedupe here so `upsert_page`'s
    // `INSERT ... ON CONFLICT` never fights a duplicate PK.
    let raw_links = links::extract_links(&fm.body);
    let link_rows: Vec<(String, LinkStyle)> = links::dedup(raw_links)
        .into_iter()
        .map(|l: Link| (l.target, l.style))
        .collect();

    // fs metadata (mtime_ns / content_hash). `std::fs` here, not
    // `fs::symlink_metadata` — the file itself is already loaded above so
    // `metadata` is fine (if it were a symlink, `std::fs::read` would
    // already have followed it; the walker classifies symlinks as
    // non-indexable via `NodeKind::Symlink`, so we only get here for real
    // files).
    let meta = abs_path
        .metadata()
        .map_err(|e| IndexError::Read(e.to_string()))?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let content_hash = etag_of(&bytes);

    let language = if include_language {
        extract_language(&fm.frontmatter_text)
    } else {
        None
    };

    Ok(PageRecord {
        abs_path: abs_path.to_string_lossy().to_string(),
        id,
        page_type,
        root: root.to_string(),
        rel_path: rel_path.to_string(),
        title: Some(slug),
        language,
        description,
        search_required_phrase_verbatim,
        frontmatter_text: if fm.frontmatter_text.is_empty() {
            None
        } else {
            Some(fm.frontmatter_text.clone())
        },
        body: fm.body.clone(),
        mtime_ns,
        content_hash,
        gate_terms,
        tag_rows: tags,
        link_rows,
    })
}

/// Strip a single layer of surrounding double/single quotes from each
/// gate phrase (PLAN §8 page_gates: "exact entry as written, minus
/// surrounding double quotes"). Non-quoted entries are unchanged.
fn kb_core_search_required_phrase_unquoted(terms: &[String]) -> Vec<String> {
    terms
        .iter()
        .map(|t| {
            let b = t.trim();
            let r = b
                .strip_prefix('"')
                .or_else(|| b.strip_prefix('\''))
                .unwrap_or(b);
            let r = r
                .strip_suffix('"')
                .or_else(|| r.strip_suffix('\''))
                .unwrap_or(r);
            r.trim().to_string()
        })
        .collect()
}

/// Why a page could not be indexed (returned by `build_page`).
pub enum IndexError {
    NoType { type_raw: String, unknown: bool },
    MissingSlug,
    Read(String),
}

impl std::fmt::Debug for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::NoType { type_raw, unknown } => f
                .debug_struct("NoType")
                .field("type_raw", type_raw)
                .field("unknown", unknown)
                .finish(),
            IndexError::MissingSlug => f.debug_struct("MissingSlug").finish(),
            IndexError::Read(e) => f.debug_tuple("Read").field(e).finish(),
        }
    }
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::NoType { type_raw, unknown } => {
                if *unknown {
                    write!(f, "unknown type:{type_raw}")
                } else {
                    write!(f, "no type: in frontmatter")
                }
            }
            IndexError::MissingSlug => {
                write!(f, "no resolvable slug (title/H1/filename all empty)")
            }
            IndexError::Read(e) => write!(f, "read failed: {e}"),
        }
    }
}

impl std::error::Error for IndexError {}

/// Upsert a single page + its row-tables. A single transaction, so a
/// failure mid-write (e.g. an FK violation we didn't anticipate) does not
/// leave the DB half-committed.
///
/// **Order of writes** (all inside one `BEGIN…COMMIT`):
///
/// 1. `DELETE FROM page_gates WHERE abs_path = ?` — the gate set is
///    *replaced wholesale* each upsert (cheaper than a row-by-row diff, and
///    a gate the user *removed* from a page has to leave the DB too).
/// 2. `DELETE FROM page_tags WHERE abs_path = ?` — same rationale.
/// 3. `DELETE FROM links WHERE from_path = ?` — same.
/// 4. `INSERT INTO pages ... ON CONFLICT(abs_path) DO UPDATE SET ...` — the
///    page row; the FTS5 triggers fire on the UPDATE/INSERT.
/// 5. `INSERT OR IGNORE` the gate rows.
/// 6. `INSERT OR IGNORE` the tag rows.
/// 7. `INSERT OR IGNORE` the link rows.
pub fn upsert_page(db: &Db, rec: &PageRecord) -> Result<usize> {
    let conn = &db.conn;
    let tx = conn.unchecked_transaction()?;

    // 1–3. Replace the row-table entries wholesale.
    tx.execute(
        "DELETE FROM page_gates WHERE abs_path = ?1",
        rusqlite::params![rec.abs_path],
    )?;
    tx.execute(
        "DELETE FROM page_tags WHERE abs_path = ?1",
        rusqlite::params![rec.abs_path],
    )?;
    tx.execute(
        "DELETE FROM links WHERE from_path = ?1",
        rusqlite::params![rec.abs_path],
    )?;

    // 4. The page row. The FTS5 triggers (migrate) keep `pages_fts` in
    //    lockstep — we do not touch it directly.
    let now = now_iso8601();
    let gate = super::migrate::COL_SEARCH_REQUIRED_PHRASE;
    let res = tx.execute(
        &format!(
            r#"
         INSERT INTO pages (
           abs_path, id, type, root, rel_path, title, language,
           description, {gate}, frontmatter_text, body,
           mtime_ns, content_hash, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)
         ON CONFLICT (abs_path) DO UPDATE SET
           id = excluded.id,
           type = excluded.type,
           root = excluded.root,
           rel_path = excluded.rel_path,
           title = excluded.title,
           language = excluded.language,
           description = excluded.description,
           {gate} = excluded.{gate},
           frontmatter_text = excluded.frontmatter_text,
           body = excluded.body,
           mtime_ns = excluded.mtime_ns,
           content_hash = excluded.content_hash,
           updated_at = excluded.updated_at
         "#
        ),
        params![
            rec.abs_path,
            rec.id,
            rec.page_type,
            rec.root,
            rec.rel_path,
            rec.title,
            rec.language,
            rec.description,
            rec.search_required_phrase_verbatim,
            rec.frontmatter_text,
            rec.body,
            { rec.mtime_ns },
            rec.content_hash,
            now,
        ],
    )?;

    // 5. Gate rows.
    let mut ins_gate =
        tx.prepare_cached("INSERT OR IGNORE INTO page_gates (abs_path, term) VALUES (?1, ?2)")?;
    for t in &rec.gate_terms {
        ins_gate.execute(params![rec.abs_path, t])?;
    }
    drop(ins_gate);

    // 6. Tag rows.
    let mut ins_tag =
        tx.prepare_cached("INSERT OR IGNORE INTO page_tags (abs_path, tag) VALUES (?1, ?2)")?;
    for t in &rec.tag_rows {
        ins_tag.execute(params![rec.abs_path, t])?;
    }
    drop(ins_tag);

    // 7. Link rows.
    let mut ins_link = tx.prepare_cached(
        "INSERT OR IGNORE INTO links (from_path, to_path, style) VALUES (?1, ?2, ?3)",
    )?;
    for (target, style) in &rec.link_rows {
        ins_link.execute(params![rec.abs_path, target, style.as_str()])?;
    }
    drop(ins_link);

    tx.commit()?;
    Ok(res)
}

/// Delete a page (and its row-table entries) from the DB. Called by the
/// refresh GC when a file is no longer under any registered root.
pub fn delete_page(db: &Db, abs_path: &str) -> Result<usize> {
    let tx = db.conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM page_gates WHERE abs_path = ?1",
        params![abs_path],
    )?;
    tx.execute(
        "DELETE FROM page_tags  WHERE abs_path = ?1",
        params![abs_path],
    )?;
    tx.execute(
        "DELETE FROM links     WHERE from_path = ?1",
        params![abs_path],
    )?;
    let n = tx.execute("DELETE FROM pages WHERE abs_path = ?1", params![abs_path])?;
    tx.commit()?;
    Ok(n)
}

/// The `now_iso8601` helper we need (no chrono dep yet — matches the
/// `migrate` module's choice). Returns e.g. `"1735920000"` (secs since
/// epoch; ISO-8601 UTC is a nicer human shape and the column is TEXT,
/// which lets either representation persist — the CLI can format).
fn now_iso8601() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// A convenience wrapper so the refresh module can drive the indexer in
/// bulk without re-deriving the "upsert-or-skip" decision per page.
pub fn upsert_or_skip(
    db: &Db,
    abs_path: &Path,
    root: &str,
    rel_path: &str,
    out: &mut IndexOutcome,
) -> Result<()> {
    match build_page(abs_path, root, rel_path, true) {
        Ok(rec) => {
            let n = upsert_page(db, &rec)?;
            out.upserted += n;
            Ok(())
        }
        Err(IndexError::NoType { .. }) => {
            out.skipped_no_type += 1;
            Ok(())
        }
        Err(IndexError::MissingSlug) => {
            out.skipped_missing_slug += 1;
            Ok(())
        }
        Err(IndexError::Read(msg)) => Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(-1),
            Some(format!("index: {msg}")),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn etag_matches_known_fnv1a() {
        // FNV-1a 64 of the empty string is 0xcbf29ce484222325.
        assert_eq!(etag_of(b""), "cbf29ce484222325");
    }

    #[test]
    fn gate_terms_strip_quotes() {
        let src = vec![
            "mTLS".to_string(),
            "\"token bucket\"".to_string(),
            "'a b'".to_string(),
        ];
        let out = kb_core_search_required_phrase_unquoted(&src);
        assert_eq!(
            out,
            vec![
                "mTLS".to_string(),
                "token bucket".to_string(),
                "a b".to_string()
            ]
        );
    }

    #[test]
    fn extract_language_reads_scalar() {
        assert_eq!(
            extract_language("type: note\nlanguage: en\ntitle: x\n"),
            Some("en".to_string())
        );
        assert_eq!(
            extract_language("type: note\nlanguage: 'en'\ntitle: x\n"),
            Some("en".to_string())
        );
        assert_eq!(
            extract_language("type: note\ntitle: x\nlanguage:   \n"),
            None,
            "empty value → None"
        );
        assert_eq!(extract_language("type: note\ntitle: x\n"), None);
    }

    #[test]
    fn extract_language_requires_key_boundary() {
        // SECURITY_REPORT 3.10: the `language` prefix must be followed by the
        // `:` boundary (after optional whitespace). Sibling keys that merely
        // *share* the prefix must not pollute the `language:` filter column.
        // Positive forms.
        assert_eq!(
            extract_language("type: note\nlanguage: en\n"),
            Some("en".to_string())
        );
        assert_eq!(
            extract_language("type: note\nlanguage : en\n"),
            Some("en".to_string()),
            "a space before the colon is still a valid key boundary"
        );
        // Negative forms (sibling keys, not the `language` key).
        assert_eq!(
            extract_language("type: note\nlanguages: en, fr\n"),
            None,
            "`languages:` is a sibling key, must not set language"
        );
        assert_eq!(
            extract_language("type: note\nlanguage_x: yz\n"),
            None,
            "`language_x:` is a sibling key, must not set language"
        );
    }

    #[test]
    fn build_page_indexes_a_wellformed_note() {
        let dir = std::env::temp_dir().join("kbdb_idx_a");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("note.md");
        let txt = concat!(
            "---\n",
            "type: note\n",
            "title: My Note\n",
            "language: en\n",
            "description: the note\n",
            "tags:\n  - rust\n  - http\n",
            "search_required_phrase:\n  - mTLS\n  - \"token bucket\"\n",
            "---\n",
            "\n# My Note — H1\n\n[[Some Wiki]]  papers/final.pdf\n",
        );
        fs::write(&p, txt).unwrap();

        let rec = build_page(&p, "/r", "note.md", true).unwrap();
        assert_eq!(rec.id, "note:My Note"); // title won
        assert_eq!(rec.page_type, "note");
        assert_eq!(rec.tag_rows, vec!["rust".to_string(), "http".to_string()]);
        assert_eq!(
            rec.gate_terms,
            vec!["mTLS".to_string(), "token bucket".to_string()]
        );
        assert!(rec.gate_terms.iter().all(|t| !t.starts_with('"')));
        // verbatim search_required_phrase keeps the quotes (the pages column contract)
        assert_eq!(
            rec.search_required_phrase_verbatim.as_deref(),
            Some("mTLS\n\"token bucket\"")
        );
        // language extracted
        assert_eq!(rec.language.as_deref(), Some("en"));
        // links: one wikilink + one relative path
        assert_eq!(rec.link_rows.len(), 2);
        let wikis: Vec<_> = rec
            .link_rows
            .iter()
            .filter(|(_, s)| *s == LinkStyle::Wikilink)
            .collect();
        let rels: Vec<_> = rec
            .link_rows
            .iter()
            .filter(|(_, s)| *s == LinkStyle::Relative)
            .collect();
        assert_eq!(wikis.len(), 1);
        assert_eq!(rels.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_page_skips_no_type() {
        let dir = std::env::temp_dir().join("kbdb_idx_b");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("untitled.md");
        fs::write(&p, "just prose, no frontmatter\n").unwrap();
        match build_page(&p, "/r", "untitled.md", true) {
            Err(IndexError::NoType { .. }) => {}
            other => panic!("expected NoType, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
