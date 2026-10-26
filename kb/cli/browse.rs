use crate::*;

/// A `kb browse` row: the seven display columns of a `pages` row. (Named so
/// the local binding below stays readable without inlining the tuple type.)
pub(crate) type BrowseRow = (String, String, String, Option<String>, String, String, Option<String>);

/// Fetch the pages a `kb browse` will display (A2 extraction from
/// `cmd_browse`): optional `--path`-prefix filter (`LIKE 'prefix/%'` or exact
/// match), ordered `abs_path` when a filter is present and by `root, rel_path`
/// otherwise. Keeping the two `prepare` sites in one function makes the
/// "only `pages`, ordered deterministically" contract greppable.
fn fetch_browse_pages(
    db: &Db,
    filter_prefix: Option<&str>,
) -> Result<Vec<BrowseRow>, (String, i32)> {
    let map_row = |r: &rusqlite::Row| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    };
    Ok(match filter_prefix {
        Some(prefix) => {
            let like = format!("{prefix}/%");
            let browslike = format!(
                "SELECT abs_path, id, type, title, root, rel_path, {gate}\n\
                FROM pages\n\
                WHERE abs_path LIKE ?1 OR abs_path = ?2\n\
                ORDER BY abs_path",
                gate = GATE_COL
            );
            let mut stmt = db.conn
                .prepare(&browslike)
                .map_err(|e| (format!("prepare browse: {e}"), EXIT_ERR))?;
            let rows = stmt
                .query_map(params![like, prefix], &map_row)
                .map_err(|e| (format!("browse query: {e}"), EXIT_ERR))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| (format!("browse rows: {e}"), EXIT_ERR))?;
            rows
        }
        None => {
            let browseall = format!(
                "SELECT abs_path, id, type, title, root, rel_path, {gate}\n\
                FROM pages\n\
                ORDER BY root, rel_path",
                gate = GATE_COL
            );
            let mut stmt = db.conn
                .prepare(&browseall)
                .map_err(|e| (format!("prepare browse: {e}"), EXIT_ERR))?;
            let rows = stmt
                .query_map([], &map_row)
                .map_err(|e| (format!("browse query: {e}"), EXIT_ERR))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| (format!("browse rows: {e}"), EXIT_ERR))?;
            rows
        }
    })
}


/// Render the fetched `BrowseRow` batch (A2 extraction from `cmd_browse`).
/// JSON path emits one page object per row via `PageJson` and the canonical
/// empty array when empty. Human path mirrors `tag` / `links` layout; labels
/// append their `refs`.
fn render_browse(db: &Db, pages: &[BrowseRow], json: bool) -> i32 {
    if json {
        let mut items = Vec::new();
        for (abs_path, id, pt, title, _root, rel_path, search_required_phrase) in pages.iter() {
            let tags: Vec<String> = page_tags_str(db, abs_path).split(',')
                .filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string()).collect();
            let refs = if pt == "label" { Some(label_expand(db, abs_path)) } else { None };
            let gj = PageJson {
                id: id.clone(), pt: pt.clone(),
                title: title.clone(), abs: abs_path.clone(), rel: rel_path.clone(),
                tags, gates: search_required_phrase.clone().unwrap_or_default(),
                links: Some(page_link_count(db, abs_path)),
                refs, description: None,
            };
            items.push(gj.render());
        }
        println!("{}", top_array(&items));
        return EXIT_OK;
    }

    if pages.is_empty() {
        println!("no pages found under the given path");
        return EXIT_OK;
    }

    for (i, (abs_path, id, pt, title, _root, rel_path, search_required_phrase)) in pages.iter().enumerate() {
        let tags = page_tags_str(db, abs_path);
        let link_count = page_link_count(db, abs_path);
        let gate_str = search_required_phrase.as_deref().unwrap_or("");

        let mut line = format!("{:>3}  {:<20} {}", i + 1, id, title.as_deref().unwrap_or(""));
        if !rel_path.is_empty() {
            line.push_str(&format!("  {rel_path}"));
        }
        if !tags.is_empty() {
            line.push_str(&format!("  tags: {tags}"));
        }
        if !gate_str.is_empty() {
            line.push_str(&format!("  gates: {gate_str}"));
        }
        if link_count > 0 {
            line.push_str(&format!("  links: {link_count}"));
        }
        println!("{line}");

        if pt == "label" {
            let refs = label_expand(db, abs_path);
            for r in &refs {
                println!("     → {r}");
            }
        }
    }
    println!("── {} page(s)", pages.len());
    EXIT_OK
}


pub(crate) fn cmd_browse(dbpath: Option<&Path>, path: Option<&Path>, json: bool) -> Kb {
    let db = open_db(dbpath)?;

    let filter_prefix: Option<String> = path
        .map(|p| {
            let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            c.to_string_lossy().to_string()
        });

    let pages = fetch_browse_pages(&db, filter_prefix.as_deref())?;
    Ok(render_browse(&db, &pages, json))
}

// ---- tag ----

