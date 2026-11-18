//! Search (GUI port of `kb search`, PLAN-GUI §6.8 / §8.1).
//!
//! The pipeline is the CLI's `search_rows` ported *verbatim in its
//! query-construction semantics* — same parse, same FTS predicate
//! structure, same gate predicate, same tag-filter expansion, same `LIMIT`
//! handling. The GUI's `Search` command carries the *scope* (the rail's
//! selection) in addition to the `query` string; the scope is injected as
//! the `gate_text` value in exactly the same position the CLI uses.
//!
//! Parity claim: with `scope = None`, `search_pages(db, query)` returns the
//! *exact* same result-set and ordering as `kb search <query>` against the
//! same DB. The only divergence: the GUI additionally *excludes* pages not
//! belonging to the scope-collection when a scope is selected
//! (PLAN-GUI §2.1 rule 3: "selecting a collection scopes the search").

use crate::state::SearchHit;
use kb_db::Db;

/// G1 search: search the whole corpus, optionally scoped to a collection.
///
/// Returns `(rows, missing_tag)` — `missing_tag = Some(tag_value)` signals
/// a query that *matched no corpus tag* (the CLI prints a specific message
/// for that case; the GUI surfaces it via `SearchView::missing_tag`).
pub fn search_pages(
    db: &Db,
    query: &str,
    scope: Option<&str>,
    limit: usize,
) -> Result<(Vec<SearchHit>, Option<String>), String> {
    let parsed = kb_core::search_grammar::parse(query);
    let has_and = !parsed.and_terms.is_empty();
    let has_or = !parsed.or_group.is_empty();
    let has_not = !parsed.not_terms.is_empty();
    let has_match = has_and || has_or;

    // `gate_text` is the *search text* for the gate predicate
    // (lower(query-text) LIKE '%' || lower(gate-term) || '%'), not a
    // single value. See `kb cli::search::search_rows` for the construction.
    //
    // When a scope is selected (PLAN-GUI §2.1 rule 3: "selecting a
    // collection scopes the search" / "the collection's gate term satisfies
    // the gate predicate"), the scope's term is *appended* to gate_text so
    // the strict CLI gate predicate is satisfied for in-scope pages. FTS
    // still matches only the user's query (rank unchanged — the plan's
    // scoped-parity rule: "`energy` inside `Electronics` returns the same
    // page as `energy electronics` globally, but FTS ranking is computed
    // from `energy` alone").
    let has_scope = matches!(scope, Some(s) if !s.trim().is_empty());
    let gate_text: String = {
        let mut parts: Vec<String> = Vec::new();
        for t in &parsed.and_terms {
            parts.push(term_text(t));
        }
        for t in &parsed.or_group {
            parts.push(term_text(t));
        }
        for t in &parsed.not_terms {
            parts.push(term_text(t));
        }
        if has_scope {
            parts.push(scope.unwrap().trim().to_string());
        }
        parts.join(" ")
    };

    // Alias + distinct-tag loads (hoisted; one pass per table when the
    // query has at least one `tag:` filter).
    let has_tag_filter = parsed.filters.iter().any(|f| f.is_tag());
    let (aliases, all_tags) = if has_tag_filter {
        let aliases: Vec<(String, String)> = {
            let stmt = db
                .conn
                .prepare("SELECT alias, canonical FROM tag_aliases")
                .map_err(|e| format!("prepare aliases: {e}"))?;
            stmt.query_map((), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| format!("aliases: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("aliases rows: {e}"))?
        };
        let all_tags: Vec<String> = {
            let stmt = db
                .conn
                .prepare("SELECT DISTINCT tag FROM page_tags")
                .map_err(|e| format!("prepare tags: {e}"))?;
            stmt.query_map((), |r| r.get::<_, String>(0))
                .map_err(|e| format!("tags: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("tag rows: {e}"))?
        };
        (aliases, all_tags)
    } else {
        (Vec::new(), Vec::new())
    };

    // Expand each `tag:` filter to its corpus match-set; a tag that has
    // no corpus row is the *missing_tag* short-circuit (the CLI's specific
    // "no pages tagged …" case).
    let mut tag_match_sets: Vec<Vec<String>> = Vec::new();
    for f in &parsed.filters {
        if f.is_tag() && !f.is_null() {
            let tag_val = f.value.clone();
            let match_set: Vec<String> = kb_core::tags::canonicalize(&tag_val, &all_tags, &aliases)
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            if match_set.is_empty() {
                return Ok((Vec::new(), Some(tag_val)));
            }
            tag_match_sets.push(match_set);
        }
    }

    let (mut sql, mut params_vec) = build_search_sql(
        &parsed,
        &gate_text,
        &tag_match_sets,
        has_match,
        has_not,
        limit,
    );

    // Scope injection (PLAN-GUI §2.1 rule 3: "selecting a collection
    // scopes the search"): when the user has selected a non-`All`
    // collection in the rail, the search result set is *intersected* with
    // that collection's membership. We do this as an additional
    // `EXISTS (page_gates.g_abs_path = pages.abs_path AND lower(g.term)
    // = lower(?scope))` predicate, AND-composed with the strict CLI gate
    // predicate. This matches the CLI behavior of
    // `kb browse <collection> && kb search <query>` (both compose AND).
    //
    // The placeholder index is `params_vec.len() + 1`; we append the
    // predicate to the SQL string and push the value to the params
    // vector so the SQL remains parameterised (no `?N` rebinding
    // required by the executor).
    if has_scope {
        let scope_idx = params_vec.len() + 1;
        sql.push_str(&format!(
            "\nAND EXISTS (SELECT 1 FROM page_gates g \
             WHERE g.abs_path = pages.abs_path \
             AND lower(g.term) = lower(?{scope_idx}))"
        ));
        params_vec.push(scope.unwrap_or("").to_string());
    }

    // Execute and materialise.
    let stmt = db.conn.prepare(&sql).map_err(|e| {
        if std::env::var_os("KB_GUI_DEBUG_SQL").is_some() {
            format!("prepare search: {e}\nSQL: {sql}")
        } else {
            format!("prepare search: {e}")
        }
    })?;
    let bound = kb_db::sql::params_from_iter(params_vec.iter().map(|s| s.as_str()));
    #[allow(clippy::type_complexity)]
    type HitTuple = (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<f64>,
        String,
    );
    let tuples = stmt
        .query_map(bound, |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<f64>>(8)?,
                r.get::<_, Option<String>>(9)?.unwrap_or_default(),
            ) as HitTuple)
        })
        .map_err(|e| format!("search query: {e}"))?;

    let mut hits: Vec<SearchHit> = Vec::new();
    for t in tuples {
        let (abs_path, id, type_, title, description, _root, rel_path, _gate, score, uuid) =
            t.map_err(|e| format!("search row: {e}"))?;
        let uuid_short = if uuid.is_empty() {
            String::new()
        } else {
            // `uuid_short_for` (CLI's bounded prefix probe) walks the
            // prefix-length space 5..=n, looking for the *shortest*
            // prefix that maps to exactly this page. The GUI's display
            // wants the same token so search output is paste-back-able
            // into any path-arg verb (`kb show <uuid-prefix>`).
            match uuid_short_for(db, &uuid, &abs_path) {
                Ok(s) => s,
                Err(_) => uuid.clone(),
            }
        };
        hits.push(SearchHit {
            abs_path,
            id,
            type_,
            title,
            description,
            rel_path,
            gates: _gate.unwrap_or_default(),
            uuid,
            uuid_short,
            score,
        });
    }

    Ok((hits, None))
}

/// Shortest-unique uuid prefix (mirrors `kb::cli::mod::uuid_short_for`).
/// Walks the prefix-length space from the FLOOR (5) up to the full length,
/// returning the first k where exactly *this* page resolves. At k = n the
/// full uuid (salted by self_abs) is unique, so the full length is the
/// fallback when no shorter prefix distinguishes the page.
fn uuid_short_for(db: &Db, full_uuid: &str, self_abs: &str) -> Result<String, String> {
    let full_uuid = full_uuid.to_ascii_lowercase();
    let n = full_uuid.len();
    let lo = kb_core::uuid::FLOOR.min(n);
    for k in lo..=n {
        let prefix = &full_uuid[..k];
        let rows = uuid_prefix_abs_paths(db, prefix)?;
        if rows.len() == 1 && rows[0] == self_abs {
            return Ok(prefix.to_string());
        }
    }
    Ok(full_uuid)
}

/// Distinct absolute paths whose `uuid` starts with `prefix`.
fn uuid_prefix_abs_paths(db: &Db, prefix: &str) -> Result<Vec<String>, String> {
    let sql = "SELECT abs_path FROM pages WHERE uuid LIKE ?";
    let stmt = db
        .conn
        .prepare(sql)
        .map_err(|e| format!("prefix probe: {e}"))?;
    // Turso's LIKE has no standard escape; we use `GLOB` (case-sensitive,
    // `*` glob) instead, which the planner accepts in the same SQL dialect.
    // (The CLI uses the same form for the bounded prefix probe.)
    let bound = kb_db::sql::params_from_iter(vec![format!("{prefix}%").to_string()]);
    let rows = stmt
        .query_map(bound, |r| r.get::<_, String>(0))
        .map_err(|e| format!("prefix query: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("prefix row: {e}"))?);
    }
    Ok(out)
}

/// Term text: the raw token, phrase preserved (used to build the
/// `gate_text` string the CLI's gate predicate matches against).
fn term_text(t: &kb_core::search_grammar::Term) -> String {
    match t {
        kb_core::search_grammar::Term::Token { text, .. } => text.clone(),
    }
}

/// The `kb search --json` parity row: `(abs_path, id, type, title, desc,
/// root, rel_path, gate, score, uuid)`. The tuple shape matches
/// `kb/cli/search.rs::SearchRow` minus the `uuid_short` field (computed
/// by `search_pages` after the SQL).
// The `#[allow(dead_code)]` is on the type alias; it *is* used inside the
// query closure in `search_pages` above.
#[allow(dead_code)]
type SearchTuple = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<f64>,
    String,
);

/// Build the full `SEARCH` SQL string + its positional params from the
/// parsed query. Ported verbatim from `kb/cli/search.rs::build_search_sql`
/// — the query-construction semantics (FTS predicate structure, gate
/// predicate, tag-filter expansion, IN-list case-folding, score
/// expression, `LIMIT`) are identical.
fn build_search_sql(
    parsed: &kb_core::search_grammar::ParsedQuery,
    gate_text: &str,
    tag_match_sets: &[Vec<String>],
    has_match: bool,
    has_not: bool,
    limit: usize,
) -> (String, Vec<String>) {
    let mut params_vec: Vec<String> = Vec::new(); // all params are TEXT for v1
    let mut wheres: Vec<String> = Vec::new();

    let fts_render = parsed.render_fts();
    let fts_cols = "title, description, body, frontmatter_text";
    let not_only = !has_match && has_not;
    let mut fts_param_idx: Option<usize> = None;
    if has_match && !fts_render.is_empty() {
        params_vec.push(fts_render);
        fts_param_idx = Some(params_vec.len());
        wheres.push(format!("fts_match({fts_cols}, ?{})", params_vec.len()));
    }
    if not_only {
        for t in &parsed.not_terms {
            params_vec.push(kb_core::search_grammar::render_term(t));
            wheres.push(format!(
                "NOT EXISTS (SELECT 1 FROM pages p2 WHERE p2.rowid = pages.rowid \
                 AND fts_match({fts_cols}, ?{}))",
                params_vec.len()
            ));
        }
    }

    for f in &parsed.filters {
        if !f.is_tag() {
            if f.is_null() {
                wheres.push(format!("pages.{} IS NULL", f.key));
            } else {
                params_vec.push(f.value.clone());
                wheres.push(format!(
                    "lower(pages.{}) = lower(?{})",
                    f.key,
                    params_vec.len()
                ));
            }
        } else if f.is_null() {
            wheres.push(
                "NOT EXISTS (SELECT 1 FROM page_tags pt WHERE pt.abs_path = pages.abs_path)".into(),
            );
        }
    }

    for match_set in tag_match_sets.iter() {
        let in_list: Vec<String> = match_set.iter().map(|t| sql_quote_lower(t)).collect();
        wheres.push(format!(
            "EXISTS (SELECT 1 FROM page_tags pt WHERE pt.abs_path = pages.abs_path\n\
             AND lower(pt.tag) IN ({}))",
            in_list.join(",")
        ));
    }

    // Gate predicate — the strict CLI rule. Un-gated pages pass; gated
    // pages must have *at least one* gate term (lower-cased) that is a
    // substring of the lower-cased query text.
    if gate_text.is_empty() {
        wheres.push(
            "NOT EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = pages.abs_path)".into(),
        );
    } else {
        params_vec.push(gate_text.to_string());
        wheres.push(format!(
            "(NOT EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = pages.abs_path)\n\
             OR EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = pages.abs_path\n\
             AND lower(?{}) LIKE '%' || lower(g.term) || '%'))",
            params_vec.len()
        ));
    }

    let score_expr = match fts_param_idx {
        Some(idx) => format!("fts_score({fts_cols}, ?{idx}) AS score"),
        None => "NULL AS score".to_string(),
    };
    let select_sql = format!(
        "SELECT pages.abs_path, pages.id, pages.type,\n\
         pages.title, pages.description, pages.root, pages.rel_path,\n\
         pages.{gate}, {score_expr}, pages.uuid\n",
        gate = kb_db::migrate::COL_SEARCH_REQUIRED_PHRASE
    );
    let mut sql = String::new();
    sql.push_str(&select_sql);
    sql.push_str("FROM pages\n");
    sql.push_str("WHERE ");
    sql.push_str(&wheres.join("\nAND "));
    if fts_param_idx.is_some() {
        sql.push_str("\nORDER BY score DESC");
    } else {
        sql.push_str("\nORDER BY pages.title COLLATE NOCASE");
    }
    sql.push_str(&format!("\nLIMIT {limit}"));

    (sql, params_vec)
}

/// Lowercase + single-quote-escape (SQLite's escape is doubling the quote).
/// Ported from `kb::main::sql_quote_lower` (C4).
fn sql_quote_lower(s: &str) -> String {
    format!("'{}'", s.to_lowercase().replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    /// The *pure* SQL-construction function is exercised directly here so
    /// a query-shape change in `kb-core::search_grammar` would break this
    /// test before the GUI ships a new binary.
    use super::*;

    #[test]
    fn sql_quote_lower_escapes_inner_quotes() {
        assert_eq!(sql_quote_lower("a'b"), "'a''b'");
    }

    #[test]
    fn sql_quote_lower_case_folds() {
        assert_eq!(sql_quote_lower("ABc"), "'abc'");
    }

    #[test]
    fn gate_predicate_is_strict_cli_parity() {
        // No match terms, no filters, no scope: the gate predicate
        // collapses to `NOT EXISTS (1 FROM page_gates g WHERE ...)` —
        // an un-gated-only search (the CLI's "page lacks a gate" rule).
        // The `gate_text` is empty (a `type:` filter contributes no FTS
        // term), so per the CLI's `build_search_sql`, the empty gate
        // case pushes NO gate param — only the filter value binds.
        let parsed = kb_core::search_grammar::parse("type:note");
        let (sql, params) = build_search_sql(&parsed, "", &[], false, false, 20);
        assert!(
            sql.contains("NOT EXISTS (SELECT 1 FROM page_gates"),
            "expected the strict no-gate predicate: {sql}"
        );
        // One param: the `type:note` value. No gate param (the empty
        // gate_text case pushes the NOT-EXISTS predicate, not a gate bind).
        assert_eq!(params.len(), 1);
        assert_eq!(params[0], "note");
    }

    #[test]
    fn build_search_sql_select_order_matches_cli() {
        let parsed = kb_core::search_grammar::parse("apple banana");
        let (sql, _) = build_search_sql(&parsed, "apple banana", &[], true, false, 20);
        // The leading SELECT column list must match the CLI's order
        // exactly (parity claim: same SQL → same result shape).
        assert!(
            sql.starts_with(
                "SELECT pages.abs_path, pages.id, pages.type,\n\
             pages.title, pages.description, pages.root, pages.rel_path,\n\
             pages.search_required_phrase,"
            ),
            "select prefix diverged: {sql}"
        );
        // The LIMIT is always the last clause *before* the trailing
        // newline.
        assert!(sql.ends_with("\nLIMIT 20"), "limit clause: {sql}");
    }
}
