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


pub(crate) fn cmd_search(dbpath: Option<&Path>, query: &str, json: bool, limit: usize) -> Kb {
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

    let db = open_db(dbpath)?;

    // tag filters need corpus + alias data (loaded outside the main query)
    // SECURITY_REPORT 3.17: hoist the alias + distinct-tag loads outside the
    // per-filter loop — N tag filters used to trigger N full table scans of
    // the same data.
    let has_tag_filter = parsed.filters.iter().any(|f| f.is_tag());
    let (aliases, all_tags) = if has_tag_filter {
        let aliases: Vec<(String, String)> = {
            let mut stmt = db.conn.prepare("SELECT alias, canonical FROM tag_aliases")
                .map_err(|e| (format!("prepare aliases: {e}"), EXIT_ERR))?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| (format!("aliases: {e}"), EXIT_ERR))?
                .collect::<Result<_, _>>()
                .map_err(|e| (format!("aliases rows: {e}"), EXIT_ERR))?;
            rows
        };
        let all_tags: Vec<String> = {
            let mut stmt = db.conn.prepare("SELECT DISTINCT tag FROM page_tags")
                .map_err(|e| (format!("prepare tags: {e}"), EXIT_ERR))?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| (format!("tags: {e}"), EXIT_ERR))?
                .collect::<Result<_, _>>()
                .map_err(|e| (format!("tag rows: {e}"), EXIT_ERR))?;
            rows
        };
        (aliases, all_tags)
    } else {
        (Vec::new(), Vec::new())
    };
    let mut tag_match_sets: Vec<Vec<String>> = Vec::new();
    for f in &parsed.filters {
        if f.is_tag() {
            let tag_val = f.value.clone();
            let match_set: Vec<String> = kbtags::canonicalize(&tag_val, &all_tags, &aliases)
                .into_iter().map(|s: &str| s.to_string()).collect();
            if match_set.is_empty() {
                if !json {
                    println!("0 results (no pages tagged {tag_val:?} in the corpus)");
                } else {
                    println!("[]");
                }
                return Ok(EXIT_OK);
            }
            tag_match_sets.push(match_set);
        }
    }

    let mut params_vec: Vec<String> = Vec::new(); // all params are TEXT for v1
    let mut wheres: Vec<String> = Vec::new();

    let fts_render = parsed.render_fts();
    // A NOT-only query (no AND, no OR) can't be expressed as a single FTS5
    // MATCH string (leading NOT is rejected). SECURITY_REPORT 3.5: this was
    // previously a *silent no-op* — the query returned the whole corpus up to
    // `LIMIT` with a reassuring note. We keep the FTS join active and add one
    // `NOT (pages_fts MATCH <term>)` clause per NOT term instead, the same
    // exclusion FTS5's own NOT gives when a positive term is present.
    let not_only = !has_match && has_not;
    let fts_active = has_match && !fts_render.is_empty() || not_only;
    if has_match && !fts_render.is_empty() {
        params_vec.push(fts_render);
        wheres.push(format!("pages_fts MATCH ?{}", params_vec.len()));
    }
    if not_only {
        for t in &parsed.not_terms {
            // FTS5 `MATCH` must appear as a top-level WHERE predicate of a
            // SELECT that directly queries the FTS5 table — `NOT (pages_fts
            // MATCH ?)` in an outer WHERE on a non-FTS5 main table is rejected
            // ("unable to use function MATCH in the requested context"). The
            // subquery satisfies the requirement: its FROM is the FTS5 table.
            params_vec.push(search_term_text(t));
            wheres.push(format!(
                "NOT EXISTS (SELECT 1 FROM pages_fts WHERE rowid = pages.rowid \
                 AND pages_fts MATCH ?{})",
                params_vec.len()
            ));
        }
    }

    for f in &parsed.filters {
        if !f.is_tag() {
            params_vec.push(f.value.clone());
            wheres.push(format!(
                "lower(pages.{}) = lower(?{})",
                f.key, params_vec.len()
            ));
        }
    }

    for match_set in tag_match_sets.iter() {
        // SECURITY_REPORT 3.4: the left side of the comparison is
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
        params_vec.push(gate_text.clone());
        wheres.push(format!(
            "(NOT EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = pages.abs_path)\n\
             OR EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = pages.abs_path\n\
             AND lower(?{}) LIKE '%' || lower(g.term) || '%'))",
            params_vec.len()
        ));
    }

    let mut sql = String::new();
    if fts_active {
        let search_fts_sql = format!(
            "SELECT pages.abs_path, pages.id, pages.type,\n\
              pages.title, pages.description, pages.root, pages.rel_path,\n\
              pages.{gate}, bm25(pages_fts) AS score\n",
            gate = GATE_COL
        );
        sql.push_str(&search_fts_sql);
        sql.push_str(
            "FROM pages\n\
              JOIN pages_fts ON pages.rowid = pages_fts.rowid\n"
        );
    } else {
        let search_nofts_sql = format!(
            "SELECT pages.abs_path, pages.id, pages.type,\n\
              pages.title, pages.description, pages.root, pages.rel_path,\n\
              pages.{gate}, NULL AS score\n",
            gate = GATE_COL
        );
        sql.push_str(&search_nofts_sql);
        sql.push_str("FROM pages\n");
    }
    sql.push_str("WHERE ");
    sql.push_str(&wheres.join("\nAND "));
    if fts_active {
        sql.push_str("\nORDER BY score ASC");
    } else {
        sql.push_str("\nORDER BY pages.title COLLATE NOCASE");
    }
    sql.push_str(&format!("\nLIMIT {limit}"));

    let db_ref = &db;
    let mut stmt = db_ref.conn.prepare(&sql).map_err(|e| {
        // F5: the full query is a side-channel of the error message and is
        // far louder than the failure warrants; keep it gated behind an
        // explicit debug switch for when we actually want to inspect the SQL.
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
    Ok(EXIT_OK)
}


pub(crate) fn page_tags_str(db: &Db, abs_path: &str) -> String {
    let mut stmt = match db.conn.prepare("SELECT tag FROM page_tags WHERE abs_path = ?1 ORDER BY tag") {
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

