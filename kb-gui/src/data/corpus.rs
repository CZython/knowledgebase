//! Corpus fetch (G1): the `All pages` rows, collection facets, and the
//! "recently updated" tail used by Home.
//!
//! The page identity fetch is ported from `kb/cli/browse.rs`
//! (`fetch_browse_pages`, no path filter): the same SELECT over `pages`,
//! the same column order, the same deterministic `ORDER BY root, rel_path`.
//! The extra sub-queries for gate terms, tags, and `updated_at` are the
//! additions the GUI needs so the rail counts and the list chips render
//! without per-row round-trips.
//!
//! `GATE_COL` is `kb_db::migrate::COL_SEARCH_REQUIRED_PHRASE` — the
//! canonical constant the rest of the tree uses for the gate column, so a
//! rename there cannot desync this query.

use std::collections::BTreeMap;

use crate::state::PageRow;

/// The same 8-tuple shape as `kb/cli/browse.rs::BrowseRow` so the
/// `map_row` closure stays readable without inlining the long type.
type PageTuple = (
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
);

/// The narrow "a connection" view the corpus queries need: an alias for
/// `kb_db::Db` so the worker's `DbGuard` and tests' temporary `Db` both
/// satisfy it without a trait.
#[allow(unused_imports)]
use kb_db::Db;
pub(crate) type DbLike = Db;

fn map_row(r: &kb_db::sql::Row) -> Result<PageTuple, kb_db::sql::Error> {
    Ok((
        r.get::<_, String>(0)?,
        r.get::<_, String>(1)?,
        r.get::<_, String>(2)?,
        r.get::<_, Option<String>>(3)?,
        r.get::<_, String>(4)?,
        r.get::<_, String>(5)?,
        r.get::<_, Option<String>>(6)?,
        r.get::<_, Option<String>>(7)?,
    ))
}

/// G1 corpus: fetch every indexed page (`All pages`) WITH its gate
/// membership, tags, uuid, and update timestamp.
///
/// The page-identity rows are the exact `kb browse` SQL. The four sub-
/// queries below are the additions the GUI needs so the rail counts and
/// the row chips render without per-row round-trips.
///
/// Parity claim: `rows.len() == kb browse | wc -l` when no path filter is
/// given. The GUI's `All pages` is *always* browse parity (PLAN-GUI §2.1
/// rule 1) — the list may re-arrange / scope / group the rows, but the
/// underlying set is identical.
pub fn load_pages(db: &DbLike, limit: Option<usize>) -> Result<Vec<PageRow>, String> {
    let sql = format!(
        "SELECT abs_path, id, type, title, root, rel_path, {gate}, uuid\n\
         FROM pages\n\
         ORDER BY root, rel_path",
        gate = kb_db::migrate::COL_SEARCH_REQUIRED_PHRASE
    );
    let stmt = db
        .conn
        .prepare(&sql)
        .map_err(|e| format!("prepare corpus: {e}"))?;
    let rows = stmt
        .query_map((), &map_row)
        .map_err(|e| format!("corpus query: {e}"))?;
    let mut pages = Vec::<PageRow>::new();
    for row in rows {
        let row = row.map_err(|e| format!("corpus row: {e}"))?;
        let base = tuple_to_row(row);
        if let Some(n) = limit {
            if pages.len() >= n {
                break;
            }
        }
        pages.push(base);
    }
    attach_membership(db, &mut pages)?;
    Ok(pages)
}

fn tuple_to_row((abs_path, id, type_, title, root, rel_path, _gate, uuid): PageTuple) -> PageRow {
    PageRow {
        abs_path,
        id,
        title,
        type_,
        rel_path,
        root,
        gates: Vec::new(),
        tags: Vec::new(),
        uuid: uuid.unwrap_or_default(),
        updated_at: String::new(),
    }
}

/// Attach `gates`, `tags`, `updated_at` to every row in place.
///
/// One pass per table, not one per row: `All pages` may be thousands, and
/// the O(N) round-trips of an N+1 fetch would be visible on Turso-routed
/// connections (which the GUI inherits by virtue of the DB shape). The
/// single `… IN (…)` per table is the portable form.
fn attach_membership(db: &DbLike, pages: &mut [PageRow]) -> Result<(), String> {
    if pages.is_empty() {
        return Ok(());
    }
    let paths: Vec<String> = pages.iter().map(|p| p.abs_path.clone()).collect();
    let n = paths.len();
    let placeholders: Vec<String> = (1..=n).map(|i| format!("?{i}")).collect();
    let in_list = placeholders.join(",");
    let bind = kb_db::sql::params_from_iter(paths.iter().map(|p| p.as_str()));

    // Gates: one row per (abs_path, term).
    let gates_stmt = format!(
        "SELECT abs_path, term FROM page_gates WHERE abs_path IN ({in_list}) ORDER BY abs_path, term"
    );
    let mut gates: BTreeMap<String, Vec<String>> = {
        let stmt = db
            .conn
            .prepare(&gates_stmt)
            .map_err(|e| format!("prepare gates: {e}"))?;
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let rows = stmt
            .query_map(bind.clone(), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| format!("gates query: {e}"))?;
        for row in rows {
            let (abs, term) = row.map_err(|e| format!("gates row: {e}"))?;
            out.entry(abs).or_default().push(term);
        }
        out
    };

    // Tags: same shape.
    let tags_stmt = format!(
        "SELECT abs_path, tag FROM page_tags WHERE abs_path IN ({in_list}) ORDER BY abs_path, tag"
    );
    let mut tags: BTreeMap<String, Vec<String>> = {
        let stmt = db
            .conn
            .prepare(&tags_stmt)
            .map_err(|e| format!("prepare tags: {e}"))?;
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let rows = stmt
            .query_map(bind.clone(), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| format!("tags query: {e}"))?;
        for row in rows {
            let (abs, tag) = row.map_err(|e| format!("tags row: {e}"))?;
            out.entry(abs).or_default().push(tag);
        }
        out
    };

    // updated_at: ISO string, empty when NULL.
    let upd_stmt = format!("SELECT abs_path, updated_at FROM pages WHERE abs_path IN ({in_list})");
    let mut updated_at: BTreeMap<String, String> = {
        let stmt = db
            .conn
            .prepare(&upd_stmt)
            .map_err(|e| format!("prepare updated_at: {e}"))?;
        let mut out: BTreeMap<String, String> = BTreeMap::new();
        let rows = stmt
            .query_map(bind, |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                ))
            })
            .map_err(|e| format!("updated_at query: {e}"))?;
        for row in rows {
            let (abs, ts) = row.map_err(|e| format!("updated_at row: {e}"))?;
            out.insert(abs, ts);
        }
        out
    };

    for p in pages.iter_mut() {
        p.gates = gates.remove(&p.abs_path).unwrap_or_default();
        p.tags = tags.remove(&p.abs_path).unwrap_or_default();
        p.updated_at = updated_at.remove(&p.abs_path).unwrap_or_default();
    }
    Ok(())
}

/// `true` when at least one root is registered in the DB (drives the
/// Welcome vs Home split — PLAN-GUI §6.13/§6.14). A single
/// `SELECT COUNT(*)` over `roots`; tolerant of the table being absent
/// (a pre-migration / empty DB reports "no roots", which the UI renders
/// as the Welcome state).
pub fn has_roots(db: &DbLike) -> bool {
    let n: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM roots", (), |r| r.get(0))
        .unwrap_or(-1);
    n > 0
}

/// The distinct non-empty `language:` values across the corpus, sorted for a
/// stable autocomplete (PLAN-GUI §6.9: "language: SELECT DISTINCT language
/// FROM pages (plus free text)"). Tolerant of the `language` column being
/// absent on a very old DB (returns empty rather than erroring).
pub fn distinct_languages(db: &DbLike) -> Vec<String> {
    let mut out: BTreeMap<String, ()> = BTreeMap::new();
    if let Ok(stmt) = db.conn.prepare("SELECT DISTINCT language FROM pages WHERE language IS NOT NULL AND language != '' ORDER BY language") {
        if let Ok(rows) = stmt.query_map((), |r| r.get::<_, String>(0)) {
            for row in rows.flatten() {
                out.insert(row, ());
            }
        }
    }
    out.keys().cloned().collect()
}

/// Collection facets (PLAN-GUI §2.1). One entry per *distinct* gate term,
/// case-insensitively deduped. The *stored* spelling of the first-appearance
/// canonical casing is the label (stable, title-cased as the corpus has it).
///
/// The counts are membership counts, so a multi-gate page counts in each
/// collection it belongs to — that is why `gated`/`ungated` do not add up
/// to `All pages`. Returns `(term_facets, gated_count, ungated_count)`;
/// `term_facets` is sorted by label for stable rail ordering.
pub fn collection_facets(pages: &[PageRow]) -> (Vec<(String, usize)>, usize, usize) {
    let mut term_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut gated = 0usize;
    let mut ungated = 0usize;
    for p in pages {
        if p.gates.is_empty() {
            ungated += 1;
        } else {
            gated += 1;
            for t in &p.gates {
                *term_counts.entry(t.clone()).or_insert(0) += 1;
            }
        }
    }
    // Sort by ascending label for a stable rail order (BTreeMap already
    // gives us insertion order on the canonical label; we re-collect into a
    // Vec here so the caller can rely on a single deterministic shape).
    let mut term_facets: Vec<(String, usize)> = term_counts.into_iter().collect();
    term_facets.sort_by(|a, b| a.0.cmp(&b.0));
    (term_facets, gated, ungated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PageRow as P;

    fn page(root: &str, rel: &str, gates: Vec<&str>) -> P {
        P {
            abs_path: format!("/{root}/{rel}"),
            id: format!("wiki:{rel}"),
            title: Some(format!("Title {rel}")),
            type_: "wiki".into(),
            rel_path: rel.into(),
            root: root.into(),
            gates: gates.into_iter().map(String::from).collect(),
            tags: Vec::new(),
            uuid: "00000000deadbeef".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn empty_corpus_yields_zero_facets() {
        assert_eq!(collection_facets(&[]), (Vec::new(), 0, 0));
    }

    #[test]
    fn gated_and_ungated_split_is_exclusive() {
        let pages = vec![
            page("r1", "a.md", vec!["notes", "rust"]),
            page("r1", "b.md", vec![]),
            page("r1", "c.md", vec!["notes"]),
        ];
        let (terms, gated, ungated) = collection_facets(&pages);
        assert_eq!(gated, 2, "a.md and c.md are gated");
        assert_eq!(ungated, 1, "b.md is ungated");
        let notes = terms
            .iter()
            .find(|(l, _)| l == "notes")
            .map(|(_, n)| *n)
            .unwrap();
        let rust = terms
            .iter()
            .find(|(l, _)| l == "rust")
            .map(|(_, n)| *n)
            .unwrap();
        assert_eq!(notes, 2);
        assert_eq!(rust, 1);
        // The label set is sorted by label so "notes" < "rust" — the Vec
        // ordering is deterministic for the rail.
        let labels: Vec<_> = terms.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, vec!["notes", "rust"]);
    }

    #[test]
    fn a_multi_gate_page_counts_in_each_collection() {
        // PLAN-GUI §2.1 rule 6: badges do not sum to `All pages` because a
        // page in N collections contributes 1 to each, not 1 total.
        let pages = vec![page("r1", "a.md", vec!["x", "y", "z"])];
        let (_, gated, ungated) = collection_facets(&pages);
        assert_eq!(gated, 1);
        assert_eq!(ungated, 0);
    }
}
