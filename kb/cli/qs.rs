//! `kb quick-search` / `kb qs` — section-first full-text search (QUICK_SEARCH.md).
//!
//! One verb, three entry names (`quick-search`, `qs`, and the `section
//! search` subverb) → `cmd_qs`. The content FTS runs over the `sections`
//! index (`idx_sections_fts`); the `type:` / `language:` / `tag:` /
//! `Null` / gate-phrase **filters** run over `pages` and restrict
//! *which pages' sections* are searchable. `search_grammar` is reused
//! verbatim — only the table the content FTS targets changes.
//!
//! Output: a **single** (the section's `own_body`, or `full_body` under
//! `--full`) / **ambiguous** (a narrowing candidate list) / **empty**
//! shape, plus a `--json` full-record envelope the consumer above filters.

use crate::*;
use std::path::Path;

/// A section row as returned by the index, ready to render.
#[derive(Clone, Debug)]
pub(crate) struct QsRow {
    pub(crate) page_uuid: String,
    /// Shortest-unique uuid prefix (paste target; the uuid is salted by the
    /// owning page's `abs_path`, so the full-16-hex probe is unique —
    /// `uuid_short_for` resolves it against `pages.abs_path` at bind time).
    pub(crate) page_uuid_short: String,
    pub(crate) page_display_id: String,
    pub(crate) page_title: Option<String>,
    pub(crate) ref_: String,
    pub(crate) level: i64,
    pub(crate) ordinal: i64,
    pub(crate) section_title: String,
    pub(crate) own_body: String,
    pub(crate) full_body: Option<String>,
    pub(crate) own_end_line: i64,
    pub(crate) own_hash: String,
    pub(crate) score: f64,
}

/// Build the `qs` SQL + its positional params from the parsed query.
///
/// The content FTS renders with the *same* `render_fts()` as `kb search`
/// (prefix terms, OR-group, NOT-clauses), but targets the `sections`
/// index. The `type:` / `language:` / `tag:` / `Null` / gate-phrase
/// **filters** are re-issued as `EXISTS` subqueries correlated on
/// `pages.uuid = sections.page_uuid` (the base query runs over
/// `sections`, so the page-side columns need a join per predicate).
///
/// `params_vec` is filled in the exact order the `?N` placeholders appear
/// (`params_vec.len()` is the live counter at each `push`), so
/// `params_from_iter(params_vec.iter())` binds them in one call.
fn section_search_sql(
    parsed: &search_grammar::ParsedQuery,
    gate_text: &str,
    tag_match_sets: &[Vec<String>],
    has_match: bool,
    has_not: bool,
    limit: usize,
) -> (String, Vec<String>) {
    let mut params_vec: Vec<String> = Vec::new(); // all params are TEXT
    let mut wheres: Vec<String> = Vec::new();

    // ---- the content FTS, over the sections index --------------------------
    let fts_render = parsed.render_fts();
    let fts_cols = "sections.title, sections.own_body";
    let not_only = !has_match && has_not;
    let mut fts_idx: Option<usize> = None;
    if has_match && !fts_render.is_empty() {
        params_vec.push(fts_render);
        fts_idx = Some(params_vec.len());
        wheres.push(format!("fts_match({fts_cols}, ?{})", params_vec.len()));
    }
    if not_only {
        for t in &parsed.not_terms {
            // Same Turso-planner restriction as `kb search`: a bare `NOT`
            // clause is rejected with no positive FTS term to anchor it, so
            // a NOT-only query phrases each exclusion as a correlated
            // `NOT EXISTS` (the form the engine accepts).
            params_vec.push(search_grammar::render_term(t));
            wheres.push(format!(
                "NOT EXISTS (SELECT 1 FROM sections s2 WHERE s2.rowid = sections.rowid \
                 AND fts_match({fts_cols}, ?{}))",
                params_vec.len()
            ));
        }
    }

    // ---- the page-side filters (the same grammar as `kb search`) ----------
    for f in &parsed.filters {
        if !f.is_tag() {
            if f.is_null() {
                // `type:Null` / `language:Null` — the *page* must lack the
                // column (the section is searchable, the page is what's gated).
                wheres.push(format!(
                    "EXISTS (SELECT 1 FROM pages p WHERE p.uuid = sections.page_uuid \
                     AND p.{k} IS NULL)",
                    k = f.key
                ));
            } else {
                params_vec.push(f.value.clone());
                wheres.push(format!(
                    "EXISTS (SELECT 1 FROM pages p WHERE p.uuid = sections.page_uuid \
                     AND lower(p.{col}) = lower(?{n}))",
                    col = f.key,
                    n = params_vec.len()
                ));
            }
        } else if f.is_null() {
            // `tag:Null` — the page has no tag rows at all (the section is
            // still a live row; the *page* is the filter target).
            wheres.push(
                "EXISTS (SELECT 1 FROM pages p WHERE p.uuid = sections.page_uuid \
                 AND NOT EXISTS (SELECT 1 FROM page_tags pt WHERE pt.abs_path = p.abs_path))"
                    .into(),
            );
        }
    }

    for tag_set in tag_match_sets.iter() {
        let in_list: Vec<String> = tag_set.iter().map(|t| sql_quote_lower(t)).collect();
        wheres.push(format!(
            "EXISTS (SELECT 1 FROM pages p WHERE p.uuid = sections.page_uuid \
             AND EXISTS (SELECT 1 FROM page_tags pt WHERE pt.abs_path = p.abs_path \
             AND lower(pt.tag) IN ({})))",
            in_list.join(",")
        ));
    }

    if gate_text.is_empty() {
        wheres.push(
            "EXISTS (SELECT 1 FROM pages p WHERE p.uuid = sections.page_uuid \
             AND NOT EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = p.abs_path))"
                .into(),
        );
    } else {
        params_vec.push(gate_text.to_string());
        wheres.push(format!(
            "EXISTS (SELECT 1 FROM pages p WHERE p.uuid = sections.page_uuid \
             AND (NOT EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = p.abs_path) \
             OR EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = p.abs_path \
             AND lower(?{}) LIKE '%' || lower(g.term) || '%')))",
            params_vec.len()
        ));
    }

    let score_expr = match fts_idx {
        Some(idx) => format!("fts_score({fts_cols}, ?{idx}) AS score"),
        None => "0.0 AS score".to_string(),
    };

    let mut sql = String::new();
    sql.push_str(&format!(
        "SELECT sections.page_uuid, sections.ref, sections.level, sections.ordinal,\n\
         sections.title AS section_title, sections.own_body, sections.full_body,\n\
         sections.own_end_line, sections.own_hash, pages.id AS page_display_id,\n\
         pages.title AS page_title, pages.abs_path, {score_expr}\n\
         FROM sections\n\
         JOIN pages ON pages.uuid = sections.page_uuid\n"));
    if !wheres.is_empty() {
        sql.push_str(&format!("WHERE {}\n", wheres.join("\nAND ")));
    }
    if fts_idx.is_some() {
        sql.push_str("ORDER BY score DESC, sections.ordinal ASC\n");
    } else {
        sql.push_str("ORDER BY pages.title COLLATE NOCASE, sections.ordinal ASC\n");
    }
    sql.push_str(&format!("LIMIT {limit}\n"));

    (sql, params_vec)
}

/// Fetch the matching section rows (joined with their page) for a query.
pub(crate) fn qs_rows(
    db: &Db,
    query: &str,
    limit: usize,
) -> Result<(Vec<QsRow>, Option<String>), (String, i32)> {
    let parsed = search_grammar::parse(query);
    let has_and = !parsed.and_terms.is_empty();
    let has_or = !parsed.or_group.is_empty();
    let has_not = !parsed.not_terms.is_empty();
    let has_match = has_and || has_or;

    let gate_text: String = {
        let mut parts: Vec<String> = Vec::new();
        for t in &parsed.and_terms {
            parts.push(match_grammar_term_text(t));
        }
        for t in &parsed.or_group {
            parts.push(match_grammar_term_text(t));
        }
        for t in &parsed.not_terms {
            parts.push(match_grammar_term_text(t));
        }
        parts.join(" ")
    };

    // Tag filters: expand aliases + canonicalize; short-circuit if a tag
    // filter has no match in the corpus (same as `kb search`).
    let has_tag_filter = parsed.filters.iter().any(|f| f.is_tag());
    let (aliases, all_tags) = if has_tag_filter {
        let aliases: Vec<(String, String)> = {
            let stmt = db
                .conn
                .prepare("SELECT alias, canonical FROM tag_aliases")
                .map_err(|e| (format!("prepare aliases: {e}"), EXIT_ERR))?;
            stmt.query_map((), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| (format!("aliases: {e}"), EXIT_ERR))?
                .collect::<Result<_, _>>()
                .map_err(|e| (format!("aliases rows: {e}"), EXIT_ERR))?
        };
        let all_tags: Vec<String> = {
            let stmt = db
                .conn
                .prepare("SELECT DISTINCT tag FROM page_tags")
                .map_err(|e| (format!("prepare tags: {e}"), EXIT_ERR))?;
            stmt.query_map((), |r| r.get::<_, String>(0))
                .map_err(|e| (format!("tags: {e}"), EXIT_ERR))?
                .collect::<Result<_, _>>()
                .map_err(|e| (format!("tags rows: {e}"), EXIT_ERR))?
        };
        (aliases, all_tags)
    } else {
        (Vec::new(), Vec::new())
    };

    let mut tag_match_sets: Vec<Vec<String>> = Vec::new();
    for f in &parsed.filters {
        if f.is_tag() && !f.is_null() {
            let tag_val = f.value.clone();
            let match_set: Vec<String> = kbtags::canonicalize(&tag_val, &all_tags, &aliases)
                .into_iter()
                .map(|s: &str| s.to_string())
                .collect();
            if match_set.is_empty() {
                return Ok((Vec::new(), Some(tag_val)));
            }
            tag_match_sets.push(match_set);
        }
    }

    let (sql, params_vec) =
        section_search_sql(&parsed, &gate_text, &tag_match_sets, has_match, has_not, limit);

    let stmt = db.conn.prepare(&sql).map_err(|e| {
        let msg = if std::env::var_os("KB_DEBUG_SQL").is_some() {
            format!("prepare quick-search: {e}\nSQL: {sql}")
        } else {
            format!("prepare quick-search: {e}")
        };
        (msg, EXIT_ERR)
    })?;
    let rows: Vec<QsRow> = stmt
        .query_map(rusqlite::params_from_iter(params_vec.iter()), |r| {
            let page_uuid: String = r.get(0)?;
            let ref_: String = r.get(1)?;
            let level: i64 = r.get(2)?;
            let ordinal: i64 = r.get(3)?;
            let section_title: String = r.get(4)?;
            let own_body: String = r.get(5)?;
            let full_body: Option<String> = r.get(6)?;
            let own_end_line: i64 = r.get(7)?;
            let own_hash: String = r.get(8)?;
            let page_display_id: String = r.get(9)?;
            let page_title: Option<String> = r.get(10)?;
            let page_abs_path: String = r.get(11)?;
            let score: f64 = r.get::<_, f64>(12)?;

            let page_uuid_short = match uuid_short_for(db, &page_uuid, &page_abs_path) {
                Ok(s) => s,
                Err(_) => page_uuid.clone(),
            };
            Ok(QsRow {
                page_uuid,
                page_uuid_short,
                page_display_id,
                page_title,
                ref_,
                level,
                ordinal,
                section_title,
                own_body,
                full_body,
                own_end_line,
                own_hash,
                score,
            })
        })
        .map_err(|e| (format!("quick-search query: {e}"), EXIT_ERR))?
        .collect::<Result<_, _>>()
        .map_err(|e| (format!("quick-search rows: {e}"), EXIT_ERR))?;

    Ok((rows, None))
}

/// The raw text of a grammar term (gate matching is by substring, not FTS).
fn match_grammar_term_text(t: &search_grammar::Term) -> String {
    match t {
        search_grammar::Term::Token { text, .. } => text.clone(),
    }
}

/// Render a single section match (the "single" / forced top-1 human shape).
fn render_single(row: &QsRow, full: bool) -> String {
    if full {
        row.full_body.clone().unwrap_or_else(|| row.own_body.clone())
    } else {
        row.own_body.clone()
    }
}

/// Render the ambiguous narrowing list (no per-row body truncation — the
/// consumer above does that).
fn render_ambiguous(rows: &[QsRow], query: &str, _full: bool) -> String {
    let mut o = String::new();
    o.push_str(&format!(
        "⚠  ambiguous — {} sections matched {query:?}:\n\n",
        rows.len()
    ));
    for (i, r) in rows.iter().enumerate() {
        o.push_str(&format!(
            "   {}.  {}  ·  {}  {:?}\n",
            i + 1,
            r.page_display_id,
            r.ref_,
            r.section_title
        ));
    }
    o.push('\n');
    o.push_str(&format!(
        "  to fetch a full body:   kb show <page-uuid> <ref>\n  to force top-1:         kb qs {query} --force\n"
    ));
    o
}

/// The `--json` full-record envelope (status + match/candidates + hint).
fn render_json(
    rows: &[QsRow],
    query: &str,
    full: bool,
    force: bool,
) -> String {
    let (status, match_row, ambiguous) = {
        if rows.is_empty() {
            ("empty", None, false)
        } else if rows.len() == 1 || force {
            ("single", Some(&rows[0]), false)
        } else {
            ("ambiguous", None, true)
        }
    };

    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"status\": {},\n", jstr(status)));
    out.push_str(&format!("  \"query\": {},\n", jstr(query)));
    if full {
        out.push_str("  \"full\": true,\n");
    }

    if let Some(r) = match_row {
        out.push_str("  \"match\": ");
        out.push_str(&render_match_obj(r, full));
        out.push_str(",\n");
        out.push_str("  \"candidates\": [],\n");
        out.push_str(&format!("  \"hint\": {}\n", jstr(&hint_str(r))));
    } else if ambiguous {
        out.push_str("  \"match\": null,\n");
        let cands: Vec<String> = rows.iter().map(render_candidate_obj).collect();
        out.push_str(&format!("  \"candidates\": {},\n", block_array(&cands)));
        out.push_str("  \"hint\": null\n");
    } else {
        out.push_str("  \"match\": null,\n");
        out.push_str("  \"candidates\": [],\n");
        out.push_str("  \"hint\": null\n");
    }

    out.push('}');
    out
}

/// One `match` object (the full record for a single / forced row).
fn render_match_obj(r: &QsRow, full: bool) -> String {
    let mut o = JObject::new();
    o.s("page_uuid", &r.page_uuid);
    if !r.page_uuid_short.is_empty() && r.page_uuid_short != r.page_uuid {
        o.s("page_uuid_short", &r.page_uuid_short);
    }
    o.s("page_display_id", &r.page_display_id);
    if let Some(t) = &r.page_title {
        if !t.is_empty() {
            o.s("page_title", t);
        }
    }
    o.s("ref", &r.ref_);
    o.i("level", r.level);
    o.i("ordinal", r.ordinal);
    o.s("section_title", &r.section_title);
    o.s("own_body", &r.own_body);
    if full {
        if let Some(fb) = &r.full_body {
            o.s("full_body", fb);
        }
    }
    o.i("own_end_line", r.own_end_line);
    o.s("own_hash", &r.own_hash);
    o.raw("score", format!("{:.6}", r.score));
    o.render()
}

/// One `candidates` entry (a narrower handle + score, no body).
fn render_candidate_obj(r: &QsRow) -> String {
    let mut o = JObject::new();
    o.s("page_uuid", &r.page_uuid);
    if !r.page_uuid_short.is_empty() && r.page_uuid_short != r.page_uuid {
        o.s("page_uuid_short", &r.page_uuid_short);
    }
    o.s("page_display_id", &r.page_display_id);
    o.s("ref", &r.ref_);
    o.i("level", r.level);
    o.i("ordinal", r.ordinal);
    o.s("section_title", &r.section_title);
    o.raw("score", format!("{:.6}", r.score));
    o.render_compact()
}

/// The copy-pasteable `kb show` target for a candidate row.
fn hint_str(r: &QsRow) -> String {
    if r.ref_ == "*" {
        format!("kb show {}", r.page_uuid)
    } else {
        format!("kb show {} {}", r.page_uuid, r.ref_)
    }
}

/// The verb body: `db` is already open + migrated.
pub(crate) fn cmd_qs(
    dbpath: Option<&Path>,
    query: &str,
    json: bool,
    force: bool,
    full: bool,
    limit: Option<usize>,
) -> Kb {
    let db = open_db(dbpath)?;
    let limit = limit.unwrap_or(20);
    let (rows, missing_tag) = qs_rows(&db, query, limit)?;

    if let Some(tag) = missing_tag {
        if !json {
            println!("0 results (no pages tagged {tag:?} in the corpus)");
        } else {
            println!("{{ \"status\": \"empty\", \"query\": {} }}", jstr(query));
        }
        return Ok(EXIT_OK);
    }

    if json {
        println!("{}", render_json(&rows, query, full, force));
        return Ok(EXIT_OK);
    }

    if rows.is_empty() {
        println!("×  no section matched {query:?}  (page matches: 0)");
        println!("   did you mean:  kb qs with a narrower tag/ type: filter");
        return Ok(EXIT_OK);
    }

    if rows.len() == 1 || force {
        // The single match is *the answer* — the content, verbatim, no
        // page handle / section reference. `--full` switches the span to
        // the section's full body (incl. subsections).
        print!("{}", render_single(&rows[0], full));
    } else {
        print!("{}", render_ambiguous(&rows, query, full));
    }
    Ok(EXIT_OK)
}
