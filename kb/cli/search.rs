use crate::*;

pub(crate) fn search_term_text(t: &search_grammar::Term) -> String {
    match t {
        search_grammar::Term::Token { text, .. } => text.clone(),
    }
}


#[allow(dead_code)]
pub(crate) struct SearchRow {
    pub(crate) abs_path:String,
    pub(crate) id:String,
    pub(crate) pt:String,
    pub(crate) title:Option<String>,
    pub(crate) description:Option<String>,
    pub(crate) root:String,
    pub(crate) rel_path:String,
    /// The `pages.search_required_phrase` **corpus field** (a list of
    /// gate-phrase values, owned by `kb-core::frontmatter`). Not to be
    /// confused with the *TUI palette role* `gated` in `kb/tui/palette.rs`,
    /// which is the *role name* the TUI uses to style pages that *have* this
    /// field — a different concept that happens to share the identifier for
    /// discoverability (see the `EditField::SearchRequiredPhrase` note in
    /// `kb/tui/mod.rs`).
    pub(crate) search_required_phrase:Option<String>,
    pub(crate) score:Option<f64>,
}


/// Build the full `SEARCH` SQL string + its positional params from the parsed
/// query. Split out of `cmd_search` (A2): the 230-line verb mixed three
/// concerns (SQL construction, row binding, result rendering); `build_search_sql`
/// owns just the first. Pure over its inputs — no `Db`, no I/O — so it is cheap
/// to exercise directly and `cmd_search` stays a thin orchestrator.
///
/// `params_vec` is filled in the exact order the `?N` placeholders appear in the
/// returned SQL (`params_vec.len()` is the live counter at each `push`), so
/// `params_from_iter(params_vec)` binds them in one call.
fn build_search_sql(
    parsed: &search_grammar::ParsedQuery,
    gate_text: &str,
    tag_match_sets: &[Vec<String>],
    has_match: bool,
    has_not: bool,
    limit: usize,
) -> (String, Vec<String>) {
    let mut params_vec: Vec<String> = Vec::new(); // all params are TEXT for v1
    let mut wheres: Vec<String> = Vec::new();

    let fts_render = parsed.render_fts();
    // Turso's FTS is function-based: there is no `pages_fts` table and no
    // `MATCH` operator. `fts_match(cols, ?q)` filters against the Tantivy
    // index built over these columns, and `fts_score(cols, ?q)` yields the
    // BM25 relevance score.
    //
    // A NOT-only query (no AND, no OR) has no positive FTS string, so it
    // gets one `NOT fts_match(cols, ?term)` predicate per excluded term and
    // no relevance score (there is nothing to rank against).
    let fts_cols = "title, description, body, frontmatter_text";
    let not_only = !has_match && has_not;
    let _fts_active = (has_match && !fts_render.is_empty()) || not_only;
    let mut fts_param_idx: Option<usize> = None;
    if has_match && !fts_render.is_empty() {
        params_vec.push(fts_render);
        fts_param_idx = Some(params_vec.len());
        wheres.push(format!(
            "fts_match({fts_cols}, ?{})",
            params_vec.len()
        ));
    }
    if not_only {
        for t in &parsed.not_terms {
            // Each NOT term is a **prefix query** (via `render_term`'s `*`
            // suffix for non-phrase tokens), matching the AND-side bare-word
            // behavior: `-watch` excludes pages whose tokens start with
            // `watch` (e.g. `watchmaking`), while quoted phrases keep the
            // exact phrase semantics.
            // `NOT fts_match(...)` alone is not accepted by Turso's planner
            // (it insists on a positive FTS index query), so phrase the
            // exclusion as a correlated `NOT EXISTS` subquery — the form Turso
            // accepts for a lone NOT term with no positive FTS term.
            params_vec.push(search_grammar::render_term(t));
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
                // `type:Null` / `language:Null` — the absence filter: the
                // column must be NULL. `lower(NULL)` is NULL in SQLite, so a
                // simple `lower(pages.x) = lower(NULL)` never matches; the
                // absence intent needs an explicit IS NULL.
                wheres.push(format!("pages.{} IS NULL", f.key));
            } else {
                params_vec.push(f.value.clone());
                wheres.push(format!(
                    "lower(pages.{}) = lower(?{})",
                    f.key, params_vec.len()
                ));
            }
        } else if f.is_null() {
            // `tag:Null` — the page lacks the tag field entirely: no row in
            // `page_tags` for it (the list field is absent, per the §5
            // "page lacks this field" contract — not "no tag named null").
            wheres.push(
                "NOT EXISTS (SELECT 1 FROM page_tags pt WHERE pt.abs_path = pages.abs_path)"
                    .into(),
            );
        }
    }

    for match_set in tag_match_sets.iter() {
        // the left side of the comparison is
        // `lower(pt.tag)`, so the IN-list must be lowercased too. `canonicalize`
        // returns the *stored* strings in their original casing (that is how
        // case/alias folding is decided), but a stored mixed-case tag like
        // `Czech` would then fail `lower('Czech') IN ('Czech',…)` → `'czech'
        // IN ('Czech',…)` → no match. Folding the IN-list to lowercase makes
        // both sides of the comparison the same case-fold.
        let in_list: Vec<String> = match_set
            .iter()
            .map(|t| sql_quote_lower(t))
            .collect();
        wheres.push(format!(
            "EXISTS (SELECT 1 FROM page_tags pt WHERE pt.abs_path = pages.abs_path\nAND lower(pt.tag) IN ({}))",
            in_list.join(",")
        ));
    }

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
          pages.{gate}, {score_expr}\n",
        gate = GATE_COL
    );
    let mut sql = String::new();
    sql.push_str(&select_sql);
    sql.push_str("FROM pages\n");
    sql.push_str("WHERE ");
    sql.push_str(&wheres.join("\nAND "));
    if fts_param_idx.is_some() {
        // Tantivy/BM25: higher score = more relevant (best first).
        sql.push_str("\nORDER BY score DESC");
    } else {
        sql.push_str("\nORDER BY pages.title COLLATE NOCASE");
    }
    sql.push_str(&format!("\nLIMIT {limit}"));

    (sql, params_vec)
}


/// Render a fetched `SearchRow` batch to `stdout` in the requested format
/// (A2: extracted from `cmd_search`). No `Db` here — the rows are already
/// materialised, so this is a pure formatting step that the TUI could in
/// principle re-use against its own row source.
fn render_search(rows: &[SearchRow], json: bool, query: &str) {
    if json {
        let items: Vec<String> = rows.iter().map(|r| {
            let score = match r.score {
                Some(s) => format!("{s:.6}"),
                None => "null".to_string(),
            };
            let mut o = JObject::new();
            o.s("id", &r.id);
            o.s("type", &r.pt);
            o.opt("title", &r.title);
            o.opt("description", &r.description);
            o.s("abs_path", &r.abs_path);
            o.s("rel_path", &r.rel_path);
            o.raw("score", score);
            o.render_compact()
        }).collect();
        println!("{}", top_array(&items));
    } else {
        if rows.is_empty() {
            println!("0 results for {query:?} (the query was too strict, or the page is gated)");
        } else {
            for (i, r) in rows.iter().enumerate() {
                let title = r.title.as_deref().unwrap_or("");
                let desc = r.description.as_deref().unwrap_or("");
                let tail = if desc.is_empty() {
                    r.rel_path.clone()
                } else {
                    format!("{desc}  ({})", r.rel_path)
                };
                println!("{:>3}  {:<24} {}  {}", i + 1, r.id, title, tail);
            }
            println!("── {} result(s)", rows.len());
        }
    }
}


/// Core search pipeline: parse → load tag data → build SQL → execute →
/// return `Vec<SearchRow>`.
///
/// Returns `(rows, missing_tag)` where `missing_tag` is `Some(tag_value)`
/// if the query was short-circuited because a tag filter had no match in
/// the corpus (the CLI prints a specific message for this case).
///
/// `db` is already open + migrated; this function performs **all** the I/O
/// (alias loads, FTS query) but does **not** open or close the connection.
pub(crate) fn search_rows(
    db: &Db,
    query: &str,
    limit: usize,
) -> Result<(Vec<SearchRow>, Option<String>), (String, i32)> {
    let parsed = search_grammar::parse(query);
    let has_and = !parsed.and_terms.is_empty();
    let has_or = !parsed.or_group.is_empty();
    let has_not = !parsed.not_terms.is_empty();
    let has_match = has_and || has_or;

    let gate_text: String = {
        let mut parts: Vec<String> = Vec::new();
        for t in &parsed.and_terms { parts.push(search_term_text(t)); }
        for t in &parsed.or_group { parts.push(search_term_text(t)); }
        for t in &parsed.not_terms { parts.push(search_term_text(t)); }
        parts.join(" ")
    };

    // hoist alias + distinct-tag loads outside the
    // per-filter loop so N tag filters don't trigger N full table scans.
    let has_tag_filter = parsed.filters.iter().any(|f| f.is_tag());
    let (aliases, all_tags) = if has_tag_filter {
        let aliases: Vec<(String, String)> = {
            let stmt = db.conn.prepare("SELECT alias, canonical FROM tag_aliases")
                .map_err(|e| (format!("prepare aliases: {e}"), EXIT_ERR))?;
            let rows = stmt.query_map((), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| (format!("aliases: {e}"), EXIT_ERR))?
                .collect::<Result<_, _>>()
                .map_err(|e| (format!("aliases rows: {e}"), EXIT_ERR))?;
            rows
        };
        let all_tags: Vec<String> = {
            let stmt = db.conn.prepare("SELECT DISTINCT tag FROM page_tags")
                .map_err(|e| (format!("prepare tags: {e}"), EXIT_ERR))?;
            let rows = stmt.query_map((), |r| r.get::<_, String>(0))
                .map_err(|e| (format!("tags: {e}"), EXIT_ERR))?
                .collect::<Result<_, _>>()
                .map_err(|e| (format!("tag rows: {e}"), EXIT_ERR))?;
            rows
        };
        (aliases, all_tags)
    } else {
        (Vec::new(), Vec::new())
    };

    // For tag-filters that don't appear in the corpus, short-circuit with
    // a `(empty, Some(tag))` signal so the CLI can print a specific message.
    let mut tag_match_sets: Vec<Vec<String>> = Vec::new();
    for f in &parsed.filters {
        if f.is_tag() && !f.is_null() {
            let tag_val = f.value.clone();
            let match_set: Vec<String> = kbtags::canonicalize(&tag_val, &all_tags, &aliases)
                .into_iter().map(|s: &str| s.to_string()).collect();
            if match_set.is_empty() {
                return Ok((Vec::new(), Some(tag_val)));
            }
            tag_match_sets.push(match_set);
        }
    }

    let (sql, params_vec) =
        build_search_sql(&parsed, &gate_text, &tag_match_sets, has_match, has_not, limit);

    let stmt = db.conn.prepare(&sql).map_err(|e| {
        let msg = if std::env::var_os("KB_DEBUG_SQL").is_some() {
            format!("prepare search: {e}\nSQL: {sql}")
        } else {
            format!("prepare search: {e}")
        };
        (msg, EXIT_ERR)
    })?;
    let rows: Vec<SearchRow> = stmt
        .query_map(rusqlite::params_from_iter(params_vec.iter()), |r| {
            Ok(SearchRow {
                abs_path: r.get::<_, String>(0)?,
                id: r.get::<_, String>(1)?,
                pt: r.get::<_, String>(2)?,
                title: r.get::<_, Option<String>>(3)?,
                description: r.get::<_, Option<String>>(4)?,
                root: r.get::<_, String>(5)?,
                rel_path: r.get::<_, String>(6)?,
                search_required_phrase: r.get::<_, Option<String>>(7)?,
                score: r.get::<_, Option<f64>>(8)?,
            })
        })
        .map_err(|e| (format!("search query: {e}"), EXIT_ERR))?
        .collect::<Result<_, _>>()
        .map_err(|e| (format!("search rows: {e}"), EXIT_ERR))?;

    Ok((rows, None))
}


pub(crate) fn cmd_search(dbpath: Option<&Path>, query: &str, json: bool, limit: usize) -> Kb {
    let db = open_db(dbpath)?;
    let (rows, missing_tag) = search_rows(&db, query, limit)?;
    if let Some(tag) = missing_tag {
        if !json {
            println!("0 results (no pages tagged {tag:?} in the corpus)");
        } else {
            println!("[]");
        }
        return Ok(EXIT_OK);
    }
    render_search(&rows, json, query);
    Ok(EXIT_OK)
}


pub(crate) fn page_tags_str(db: &Db, abs_path: &str) -> String {
    let stmt = match db.conn.prepare("SELECT tag FROM page_tags WHERE abs_path = ?1 ORDER BY tag") {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let mut out = Vec::new();
    if let Ok(it) = stmt.query_map(params![abs_path], |r| r.get::<_, String>(0)) {
        for t in it.flatten() {
            out.push(t);
        }
    }
    out.join(",")
}


pub(crate) fn page_link_count(db: &Db, abs_path: &str) -> i64 {
    db.conn
        .query_row(
            "SELECT COUNT(*) FROM links WHERE from_path = ?1",
            params![abs_path],
            |r| r.get(0),
        )
        .unwrap_or(0)
}


pub(crate) fn label_expand(db: &Db, abs_path: &str) -> Vec<String> {
    let body: Option<String> = db
        .conn
        .query_row("SELECT body FROM pages WHERE abs_path = ?1", params![abs_path], |r| r.get(0))
        .ok()
        .flatten();
    let Some(body) = body else { return vec![abs_path.to_string()] };
    match labels::classify(&body) {
        labels::LabelRef::Files(refs) => {
            let dir = Path::new(abs_path)
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_string_lossy()
                .to_string();
            refs.iter().map(|r| {
                let p = Path::new(&dir).join(r);
                p.to_string_lossy().to_string()
            }).collect()
        }
        labels::LabelRef::Folder => {
            let dir = Path::new(abs_path)
                .parent()
                .unwrap_or_else(|| Path::new("."));
            enumerate_non_recursive(dir)
        }
    }
}

