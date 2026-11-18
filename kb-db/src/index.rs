//! index — upsert one page into `pages` + `page_gates` + `page_tags` +
//! `links` (PLAN §10 `index.rs`).
//!
//! `pages_fts` is kept in lockstep **by the engine** —
//! Turso's native FTS auto-maintains the index on `pages` (no triggers,
//! no manual upsert; §13a "M5 index/refresh code generates both" holds
//! automatically).
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
//!    upsert; the FTS index auto-maintains).

use crate::sql::{params, Result};
use crate::Db;
use kb_core::frontmatter;
use kb_core::identity;
use kb_core::links;
use kb_core::meta::NoteType;
use kb_core::section as kb_section;
use kb_core::tags as kb_tags;
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
    /// Stable page-identity token: `etag_of(id \0 abs_path)` (16 hex, salted
    /// with the PK). Computed next to `content_hash` and stored in
    /// `pages.uuid`; the resolver prefix-matches it (the "uuid") so an agent
    /// can paste a short token back instead of a path.
    pub uuid: String,
    /// Per-row: the *unquoted* gate terms (PLAN §8 page_gates: "exact
    /// entry as written, minus surrounding double quotes").
    pub gate_terms: Vec<String>,
    /// Per-row: the tag strings (raw casing as written, already normalized
    /// by `kb_tags::normalize`).
    pub tag_rows: Vec<String>,
    /// Per-row: extracted links (deduped). The `style` is the v2 classification
    /// string (`wikilink` / `relative` / `absolute`) written into the
    /// `links.style` column (v2 retired `LinkStyle` as a typed enum — the
    /// indexer now writes the raw `&'static str` from
    /// `LinkKind::style_str`).
    pub link_rows: Vec<(String, String)>, // (target, style)
    /// §v3 `sections` rows (section-search). One `'*'` preamble row followed
    /// by one row per heading, in document order (the `Vec` position *is*
    /// the `ordinal` column; `ref` is already canonical — dotted number or
    /// `'*'`). Empty only for an empty page body.
    pub section_rows: Vec<kb_section::SectionSpan>,
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
        // require the character *immediately after* an
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

    // Link extraction over the *body* (not frontmatter). The v2 scanner
    // (`extract_ref_rows` = `detect_refs` + `classify_target`) recognizes
    // `[[…]]`, `[ … ]`, `[ label ]( … )`, and `< … >`; bare-token paths are
    // retired under v2. `External` URIs and `Current`-page refs are skipped
    // (not page→page edges); trailing `#section` is stripped; the result is
    // deduped on `(target, style)` so `upsert_page`'s `INSERT ... ON CONFLICT`
    // never fights a duplicate PK.
    let link_rows: Vec<(String, String)> = links::extract_ref_rows(&fm.body);

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

    // §v3: the `sections` rows — the page-level `'*'` preamble row + one per
    // heading, exported over the post-frontmatter body (the sections the
    // user reads, minus the frontmatter the parser would otherwise fence).
    let body_bytes = fm.body.as_bytes().to_vec();
    let section_rows = kb_section::Doc::new(body_bytes).export_rows(&slug);

    let abs_str = abs_path.to_string_lossy().to_string();
    let uuid = kb_core::uuid::uuid_of(&id, &abs_str);

    Ok(PageRecord {
        abs_path: abs_str,
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
        uuid,
        gate_terms,
        tag_rows: tags,
        link_rows,
        section_rows,
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
///    page row; the FTS index auto-maintains on the UPDATE/INSERT.
/// 5. `INSERT OR IGNORE` the gate rows.
/// 6. `INSERT OR IGNORE` the tag rows.
/// 7. `INSERT OR IGNORE` the link rows.
pub fn upsert_page(db: &Db, rec: &PageRecord) -> Result<usize> {
    let conn = &db.conn;
    let tx = conn.unchecked_transaction()?;

    // 1–3. Replace the row-table entries wholesale.
    tx.execute(
        "DELETE FROM page_gates WHERE abs_path = ?1",
        crate::sql::params![rec.abs_path],
    )?;
    tx.execute(
        "DELETE FROM page_tags WHERE abs_path = ?1",
        crate::sql::params![rec.abs_path],
    )?;
    tx.execute(
        "DELETE FROM links WHERE from_path = ?1",
        crate::sql::params![rec.abs_path],
    )?;

    // 4. The page row. The FTS index (Turso, auto-maintained on INSERT
    //    /UPDATE) keeps `idx_pages_fts` in sync — we do not touch it directly.
    let now = now_iso8601();
    let gate = super::migrate::COL_SEARCH_REQUIRED_PHRASE;
    let res = tx.execute(
        format!(
            r#"
         INSERT INTO pages (
           abs_path, id, type, root, rel_path, title, language,
            description, {gate}, frontmatter_text, body,
            mtime_ns, content_hash, uuid, created_at, updated_at
          ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)
          ON CONFLICT (abs_path) DO UPDATE SET
            id = excluded.id,
            type = excluded.type,
            -- `root`/`rel_path` are the page's *ownership* (which registered
            -- root it sits under), owned by `refresh`'s walk — not by
            -- frontmatter. A caller that re-indexes a single page without
            -- knowing its root/rel (the TUI `commit_edit` pass, which only
            -- re-derives the frontmatter columns) passes `""` as a
            -- "keep existing" sentinel; preserve the row's current value in
            -- that case. `refresh`/`create` always pass non-empty values, so
            -- they win as before.
            root = COALESCE(NULLIF(excluded.root, ''), root),
            rel_path = COALESCE(NULLIF(excluded.rel_path, ''), rel_path),
           title = excluded.title,
           language = excluded.language,
           description = excluded.description,
           {gate} = excluded.{gate},
           frontmatter_text = excluded.frontmatter_text,
           body = excluded.body,
            mtime_ns = excluded.mtime_ns,
            content_hash = excluded.content_hash,
            uuid = excluded.uuid,
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
            rec.uuid,
            now,
        ],
    )?;

    // 5. §v3 section rows — replaced wholesale (same rationale as
    //    gates/tags/links: a heading the page *removed* must leave the
    //    index too). Keyed on `page_uuid` (the row's identity), not the
    //    page's abs_path. `ordinal` is the row's position in
    //    `rec.section_rows` (`'*'` = 0, headings in document order).
    tx.execute(
        "DELETE FROM sections WHERE page_uuid = ?1",
        params![rec.uuid],
    )?;
    let ins_sec = tx.prepare_cached(
        "INSERT OR IGNORE INTO sections (page_uuid, ref, ordinal, level, title,\
         own_body, full_body, own_end_line, mtime_ns, own_hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for (ordinal, s) in rec.section_rows.iter().enumerate() {
        let own_hash = etag_of(format!("{}\0{}\0{}", s.ref_, s.title, s.own_body).as_bytes());
        ins_sec.execute(params![
            rec.uuid,
            s.ref_,
            ordinal as i64,
            s.level as i64,
            s.title,
            s.own_body,
            s.full_body,
            s.own_end_line as i64,
            rec.mtime_ns,
            own_hash,
        ])?;
    }
    drop(ins_sec);

    // 6. Gate rows.
    let ins_gate =
        tx.prepare_cached("INSERT OR IGNORE INTO page_gates (abs_path, term) VALUES (?1, ?2)")?;
    for t in &rec.gate_terms {
        ins_gate.execute(params![rec.abs_path, t])?;
    }
    drop(ins_gate);

    // 6. Tag rows.
    let ins_tag =
        tx.prepare_cached("INSERT OR IGNORE INTO page_tags (abs_path, tag) VALUES (?1, ?2)")?;
    for t in &rec.tag_rows {
        ins_tag.execute(params![rec.abs_path, t])?;
    }
    drop(ins_tag);

    // 7. Link rows.
    let ins_link = tx.prepare_cached(
        "INSERT OR IGNORE INTO links (from_path, to_path, style) VALUES (?1, ?2, ?3)",
    )?;
    for (target, style) in &rec.link_rows {
        ins_link.execute(params![rec.abs_path, target, style])?;
    }
    drop(ins_link);

    tx.commit()?;
    Ok(res)
}

/// One-time, idempotent backfill of `pages.uuid` for a DB that *preceded* the
/// column's introduction. Rows written by `upsert_page` (on any `refresh` /
/// `create` / TUI commit) already carry their uuid; only rows from an older
/// build (or a DB that was open across the upgrade) are still `NULL`/empty.
///
/// **Bounded memory** (the memory rule that killed the old O(N)
/// `uuid_prefix_match`): we never materialise the whole table. The loop runs
/// `SELECT … WHERE uuid IS NULL OR uuid = '' LIMIT {batch}` and, per batch,
/// streams the ≤`batch` rows into a `Vec` (the façade buffers anyway), then
/// `UPDATE`s each by PK and re-queries. The largest live allocation is one
/// batch of `(abs_path, id)` pairs — O(batch), not O(N). Each iteration both
/// rewrites its rows (clearing the sentinel) and re-selects the remainder, so
/// the loop is a fixed-point that terminates when no sentinel row remains.
/// Re-running after a full backfill is a single empty `SELECT` (O(1)).
///
/// Returns the number of rows filled (0 when nothing to do).
pub fn backfill_uuid(conn: &crate::sql::Connection) -> Result<usize> {
    const BATCH: i64 = 50_000;
    let mut filled = 0usize;
    loop {
        // The façade buffers the (bounded) batch into a Vec; `LIMIT 50000`
        // caps both memory and the number of UPDATEs this iteration performs.
        let stmt = conn.prepare(
            "SELECT abs_path, id FROM pages
             WHERE uuid IS NULL OR uuid = ''
             LIMIT ?1",
        )?;
        let batch: Vec<(String, String)> = stmt
            .query_map(params![BATCH], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>>>()?;
        if batch.is_empty() {
            break;
        }
        let up = conn.prepare_cached("UPDATE pages SET uuid = ?1 WHERE abs_path = ?2")?;
        for (abs, id) in batch {
            let u = kb_core::uuid::uuid_of(&id, &abs);
            up.execute(params![u, abs])?;
            filled += 1;
        }
        drop(up);
    }
    Ok(filled)
}

/// One-time, idempotent backfill of the `sections` index for pages that
/// predate the `'*'`-preamble / per-heading rows (introduced at §v3). `migrate`
/// creates the `sections` table, but a `pages` row written before that — or a
/// page never re-indexed since — has *no* `sections` rows, so `kb quick-search`
/// returns nothing for it. This derives each page's rows from the `body` it
/// already stores (the very input `build_page` feeds `Doc::export_rows`), so a
/// live database back-fills *without* touching the filesystem.
///
/// The section rows are re-exported per page and re-inserted wholesale (mirror
/// of `upsert_page` step 5): `own_hash` is derived identically there, so a
/// re-run is content-stable (`INSERT OR IGNORE` keeps the first). Bounded
/// memory as in [`backfill_uuid`] — a fixed `LIMIT` per iteration, streamed
/// into a `Vec`, then written and re-selected — so the largest live
/// allocation is one batch of page bodies, not the whole `pages` table.
///
/// Returns the number of pages back-filled (0 when every page already has
/// rows — the steady state this leaves the database in).
pub fn backfill_sections(conn: &crate::sql::Connection) -> Result<usize> {
    const BATCH: i64 = 1000;
    let mut filled = 0usize;
    loop {
        let stmt = conn.prepare(
            "SELECT uuid, title, body, mtime_ns
             FROM pages
             WHERE uuid IS NOT NULL AND uuid <> ''
               AND NOT EXISTS (SELECT 1 FROM sections s WHERE s.page_uuid = pages.uuid)
             LIMIT ?1",
        )?;
        let batch: Vec<(String, Option<String>, Option<String>, i64)> = stmt
            .query_map(params![BATCH], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>>>()?;
        if batch.is_empty() {
            break;
        }
        let del = conn.prepare_cached("DELETE FROM sections WHERE page_uuid = ?1")?;
        let ins = conn.prepare_cached(
            "INSERT OR IGNORE INTO sections (page_uuid, ref, ordinal, level, title,\
             own_body, full_body, own_end_line, mtime_ns, own_hash) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for (uuid, title, body, mtime_ns) in batch {
            let body = body.unwrap_or_default();
            let page_title = title.unwrap_or_default();
            let doc = kb_section::Doc::new(body.as_bytes().to_vec());
            let rows = doc.export_rows(&page_title);
            del.execute(params![uuid])?;
            for (ordinal, s) in rows.iter().enumerate() {
                let own_hash =
                    etag_of(format!("{}\0{}\0{}", s.ref_, s.title, s.own_body).as_bytes());
                ins.execute(params![
                    uuid,
                    s.ref_,
                    ordinal as i64,
                    s.level as i64,
                    s.title,
                    s.own_body,
                    s.full_body,
                    s.own_end_line as i64,
                    mtime_ns,
                    own_hash,
                ])?;
            }
            filled += 1;
        }
        drop(del);
        drop(ins);
    }
    Ok(filled)
}

/// Delete a page (and its row-table entries) from the DB. Called by the
/// refresh GC when a file is no longer under any registered root.
pub fn delete_page(db: &Db, abs_path: &str) -> Result<usize> {
    // §v3: resolve the page's section-row key (its uuid) *before* the
    // transaction — the `SELECT` and the `DELETE`s must not hold a `conn`
    // borrow across each other.
    let uuid: Option<String> = db
        .conn
        .query_row(
            "SELECT uuid FROM pages WHERE abs_path = ?1",
            params![abs_path],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let tx = db.conn.unchecked_transaction()?;
    if let Some(uuid) = &uuid {
        tx.execute("DELETE FROM sections WHERE page_uuid = ?1", params![uuid])?;
    }
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
        Err(IndexError::Read(msg)) => Err(crate::sql::Error::SqliteFailure(
            crate::sql::ffi::Error::new(-1),
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
        // the `language` prefix must be followed by the
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
            "\n# My Note — H1\n\n[[Some Wiki]] and [papers/final.pdf]\n",
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
        // links (v2): `[[Some Wiki]]` (Db→wikilink) + `[papers/final.pdf]` (Path→relative);
        // bare-prose paths like `papers/final.pdf` *outside* brackets are retired in v2.
        assert_eq!(rec.link_rows.len(), 2);
        let wikis: Vec<_> = rec
            .link_rows
            .iter()
            .filter(|(_, s)| *s == "wikilink")
            .collect();
        let rels: Vec<_> = rec
            .link_rows
            .iter()
            .filter(|(_, s)| *s == "relative")
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

    /// Backfill fills `uuid` for pre-existing rows (the "old DB" case) using
    /// the same `kb_core::uuid::uuid_of(id \0 abs_path)` expression that
    /// `build_page` uses, and is idempotent (a re-run reports 0 new rows).
    /// The salt distinguishes two pages that share an `id` but live under
    /// different folders.
    #[test]
    fn backfill_uuid_fills_null_rows_and_is_idempotent() {
        let conn = crate::sql::Connection::open_in_memory().unwrap();
        crate::migrate::migrate(&conn).unwrap(); // schema + uuid column + empty backfill

        // Simulate two page rows from an *older* build (no uuid) — distinct
        // paths. We write them directly, bypassing `upsert_page`.
        let ins = "INSERT INTO pages (abs_path, id, type, root, rel_path, mtime_ns, content_hash)
                   VALUES (?1,?2,'note','/r',?3,1,'h')";
        for (abs, id, rel) in [
            ("/r/a.md", "note:alpha", "a.md"),
            ("/other/a.md", "note:alpha", "a.md"),
        ] {
            conn.execute(ins, params![abs, id, rel]).unwrap();
        }

        // Before: both rows have a NULL uuid.
        let before: Vec<Option<String>> = {
            let s = conn
                .prepare("SELECT uuid FROM pages ORDER BY abs_path")
                .unwrap();
            s.query_map((), |r| r.get::<_, Option<String>>(0))
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap()
        };
        assert!(before.iter().all(Option::is_none), "start: {before:?}");

        let filled = backfill_uuid(&conn).unwrap();
        assert_eq!(filled, 2, "must fill both sentinel rows; got {filled}");

        // Select each row by its own path (avoids depending on ORDER BY
        // across two directories with a shared rel_path).
        let uuid_for = |path: &str| -> String {
            conn.query_row(
                "SELECT uuid FROM pages WHERE abs_path = ?1",
                params![path],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
        };
        let expected_a = kb_core::uuid::uuid_of("note:alpha", "/r/a.md");
        let expected_o = kb_core::uuid::uuid_of("note:alpha", "/other/a.md");
        assert_ne!(
            &expected_a, &expected_o,
            "salt (abs_path) must distinguish them"
        );
        assert_eq!(
            uuid_for("/r/a.md"),
            expected_a,
            "row /r/a.md must carry its own salted uuid"
        );
        assert_eq!(
            uuid_for("/other/a.md"),
            expected_o,
            "row /other/a.md must carry its own salted uuid"
        );

        // Idempotent: a re-run finds no sentinel rows at all (both already set).
        let again = backfill_uuid(&conn).unwrap();
        assert_eq!(again, 0, "second backfill must be a no-op; got {again}");
    }

    /// §v3 backfill, the "nothing in the DB" case: a `pages` row written before
    /// the `sections` table exists (or never re-indexed since) has *no*
    /// section rows, so `kb quick-search` misses it. `backfill_sections` must
    /// derive that page's rows from its stored `body` — the exact input
    /// `build_page` feeds `Doc::export_rows` — so the result is content
    /// identical to a fresh index: the `'*'` preamble row (title from the
    /// stored `pages.title`) + one row per heading. And it must be idempotent
    /// (a re-run reports 0: the `NOT EXISTS` predicate no longer matches).
    #[test]
    fn backfill_sections_derives_rows_from_stored_body_and_is_idempotent() {
        let conn = crate::sql::Connection::open_in_memory().unwrap();
        crate::migrate::migrate(&conn).unwrap(); // §v3 schema, `sections` empty

        // A legacy page: it carries a body but, from an older build, no
        // section rows. The `'*'` row's `title` must be the page's slug
        // (matching `build_page`'s `export_rows(&slug)`). A *preamble* page
        // (text before the first heading) is the interesting case: the `'*'`
        // row must carry that intro span, and each heading its own body.
        let id = "note:Backfill";
        let abs = "/r/legacy.md";
        let body = "introduction text\n\n## One\none body\n\n## Two\ntwo body\n";
        conn.execute(
            "INSERT INTO pages (abs_path, id, type, root, rel_path, title, mtime_ns, content_hash, uuid)
             VALUES (?1,?2,'note','/r','legacy.md',?3,9999,'h',?4)",
            params![abs, id, id, kb_core::uuid::uuid_of(id, abs)],
        )
        .unwrap();
        conn.execute(
            "UPDATE pages SET body = ?1 WHERE abs_path = ?2",
            params![body, abs],
        )
        .unwrap();

        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM sections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, 0, "start: sections is empty");

        let filled = backfill_sections(&conn).unwrap();
        assert_eq!(
            filled, 1,
            "must derive the legacy page's rows; got {filled}"
        );

        // `'*'` preamble row: the text before the first heading, titled by slug.
        let (star_level, star_title, star_body): (i64, String, String) = conn
            .query_row(
                "SELECT level, title, own_body FROM sections WHERE ref = '*'",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .expect("'*' row present");
        assert_eq!(star_level, 0, "the '*' row is page-level; got {star_level}");
        assert_eq!(
            star_title, id,
            "'*' row titled by the page slug; got {star_title:?}"
        );
        assert!(
            star_body.contains("introduction text"),
            "'*' row must be the preamble; got {star_body:?}"
        );

        // Headings: One + Two, each with its own body.
        let one_body: String = conn
            .query_row(
                "SELECT own_body FROM sections WHERE title = 'One'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(one_body.contains("one body"), "One body: {one_body:?}");
        let two_body: String = conn
            .query_row(
                "SELECT own_body FROM sections WHERE title = 'Two'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(two_body.contains("two body"), "Two body: {two_body:?}");

        // Total rows: '*' + One + Two = 3.
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM sections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 3, "'*' + 2 headings; got {total}");

        // Idempotent: the `NOT EXISTS` predicate no longer matches.
        let again = backfill_sections(&conn).unwrap();
        assert_eq!(again, 0, "second backfill must be a no-op; got {again}");
        let total2: i64 = conn
            .query_row("SELECT COUNT(*) FROM sections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total2, 3, "no duplicated rows after re-run; got {total2}");
    }
}
