use crate::*;

/// The ten display columns of a `kb info` row, in SELECT order. (Named so the
/// optional row binding stays readable without inlining the tuple type.)
pub(crate) type InfoRow = (
    String, String, String, Option<String>, String, String,
    Option<String>, i64, Option<String>, Option<String>,
);


pub(crate) fn cmd_info(dbpath: Option<&Path>, path: &Path, json: bool) -> Kb {
    let abs: String = path
        .to_str()
        .ok_or_else(|| (format!("{:?}: not a UTF-8 path", path), EXIT_ERR))?
        .to_string();
    let db = open_db(dbpath)?;
    let info_sql = format!(
        "SELECT abs_path, id, type, title, root, rel_path,\n\
         description, mtime_ns, created_at, {gate}\n\
         FROM pages WHERE abs_path = ?1",
        gate = GATE_COL
    );
    let row: Option<InfoRow> = db.conn
        .query_row(
            &info_sql,
            // mtime_ns is INTEGER NOT NULL; created_at is TEXT (ISO-8601).
            params![abs],
            |r| Ok((
                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?,
                r.get(4)?, r.get(5)?, r.get(6)?,
                r.get(7)?, r.get(8)?, r.get(9)?,
            )),
        )
        .optional()
        .map_err(|e| (format!("info query: {e}"), EXIT_ERR))?;
    let Some(rec) = row else {
        return Err((format!("no indexed page for {abs}"), EXIT_ERR));
    };
    let (abs_path, id, pt, title, _root, _rel_path, description, mtime_ns, created_at, search_required_phrase) = rec;
    let tags = page_tags_str(&db, &abs_path);
    let fwd_count = db.conn
        .query_row(
            "SELECT COUNT(*) FROM links WHERE from_path = ?1",
            params![abs_path],
            |r| r.get::<_, i64>(0),
        )
        .map_err(|e| (format!("count links out: {e}"), EXIT_ERR))?;
    let back_count = db.conn
        .query_row(
            "SELECT COUNT(*) FROM links WHERE to_path = ?1",
            params![abs_path],
            |r| r.get::<_, i64>(0),
        )
        .map_err(|e| (format!("count links in: {e}"), EXIT_ERR))?;
    let secs_f = (mtime_ns as f64) / 1e9;
    let mtime_str = format!("{secs_f:.1}s since unix epoch");
    let search_required_phrase = search_required_phrase.unwrap_or_default();
    if json {
        let tag_arr: Vec<String> = tags.split(',')
            .filter(|s| !s.trim().is_empty()).map(|s| jstr(s.trim())).collect();
        println!(
            "{{\n  \"id\": {},\n  \"type\": {},\n  \"title\": {},\n  \"abs_path\": {},\n  \"rel_path\": {},\n  \"description\": {},\n  \"search_required_phrase\": {},\n  \"tags\": [{}],\n  \"links\": {{\n    \"out\": {},\n    \"in\": {}\n  }},\n  \"mtime\": {},\n  \"created_at\": {}\n}}",
            jstr(&id),
            jstr(&pt),
            jstr_opt(&title),
            jstr(&abs_path),
            jstr(&_rel_path),
            jstr_opt(&description),
            jstr(&search_required_phrase),
            tag_arr.join(","),
            fwd_count,
            back_count,
            secs_f,
            jstr_opt(&created_at),
        );
        return Ok(EXIT_OK);
    }
    println!("path:    {abs_path}");
    println!("id:      {id}");
    println!("type:    {pt}");
    if !title.as_deref().unwrap_or("").is_empty() {
        println!("title:   {}", title.as_deref().unwrap_or(""));
    }
    if !description.as_deref().unwrap_or("").is_empty() {
        println!("desc:    {}", description.as_deref().unwrap_or(""));
    }
    if !search_required_phrase.is_empty() {
        println!("gates:   {search_required_phrase}");
    }
    if !tags.is_empty() { println!("tags:    {tags}"); }
    println!("links:   {fwd_count} out / {back_count} in");
    println!("mtime:   {mtime_str}");
    if let Some(c) = created_at {
        println!("created: {c}");
    }
    Ok(EXIT_OK)
}

// ---- stats ----


pub(crate) fn cmd_stats(dbpath: Option<&Path>, json: bool) -> Kb {
    let db = open_db(dbpath)?;
    let roots: Vec<(String, Option<String>, i32)> = {
        let mut stmt = db.conn
            .prepare("SELECT path, extra_exts, include_hidden FROM roots ORDER BY seq DESC")
            .map_err(|e| (format!("prepare roots: {e}"), EXIT_ERR))?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        }).map_err(|e| (format!("roots: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("roots rows: {e}"), EXIT_ERR))?;
        rows
    };
    let mut root_stats: Vec<(String, Option<Vec<String>>, i32, i64)> = Vec::new();
    if json {
        for (root, extra_exts, include_hidden) in &roots {
            let count: i64 = db.conn
                .query_row(
                    "SELECT COUNT(*) FROM pages WHERE root = ?1",
                    params![root],
                    |r| r.get(0),
                ).unwrap_or(0);
            let exts = extra_exts.as_ref().and_then(|e| {
                if e.is_empty() { None }
                else { Some(e.split('\n').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect()) }
            });
            root_stats.push((root.clone(), exts, *include_hidden, count));
        }
    } else {
        println!("== per-root page counts ==");
        for (root, extra_exts, include_hidden) in &roots {
            let count: i64 = db.conn
                .query_row(
                    "SELECT COUNT(*) FROM pages WHERE root = ?1",
                    params![root],
                    |r| r.get(0),
                ).unwrap_or(0);
            let mut opts = Vec::new();
            if let Some(exts) = extra_exts {
                if !exts.is_empty() {
                    opts.push(format!("ext:{}", exts.split('\n').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(",")));
                }
            }
            if *include_hidden != 0 { opts.push("include-hidden".into()); }
            let opt_str = if opts.is_empty() { String::new() } else { format!(" [{}]", opts.join(" ")) };
            println!("{root}{opt_str}  →  {count} page(s)");
        }
    }
    let total: i64 = db.conn
        .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
        .unwrap_or(0);
    let tag_count: i64 = db.conn
        .query_row("SELECT COUNT(DISTINCT tag) FROM page_tags", [], |r| r.get(0))
        .unwrap_or(0);
    let link_count: i64 = db.conn
        .query_row("SELECT COUNT(*) FROM links", [], |r| r.get(0))
        .unwrap_or(0);
    let alias_count: i64 = db.conn
        .query_row("SELECT COUNT(*) FROM tag_aliases", [], |r| r.get(0))
        .unwrap_or(0);
    // db file size (W-dedup: derive from Db::default_db_path(), not a hardcoded
    // ~/.local/share/... string; consistent with open_default and the cron verb).
    let db_path = dbpath
        .map(|p| p.to_string_lossy().to_string())
        .or_else(|| Db::default_db_path().map(|p| p.display().to_string()))
        .unwrap_or_else(|| "~/.local/share/knowledgebase/kb.db".into());
    let size: u64 = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let last = db.conn
        .query_row(
            "SELECT COALESCE(MAX(updated_at), MAX(created_at)) FROM pages",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten();

    if json {
        let root_items: Vec<String> = root_stats.iter().map(|(path, exts, inc, count)| {
            let mut o = format!("\n    {{\n      \"path\": {}", jstr(path));
            if let Some(v) = exts {
                let arr: Vec<String> = v.iter().map(|e| jstr(e)).collect();
                o.push_str(&format!(",\n      \"ext\": [{}]", arr.join(",")));
            }
            o.push_str(&format!(",\n      \"include_hidden\": {}", jbool(*inc != 0)));
            o.push_str(&format!(",\n      \"pages\": {count}\n    }}\n"));
            o
        }).collect();
        let last_val = match last {
            Some(ns) => format!("{:?}", ns as f64 / 1e9),
            None => "null".into(),
        };
        println!(
            "{{\n  \"roots\": [{}{}],\n  \"totals\": {{\n    \"pages\": {total},\n    \"tags\": {tag_count},\n    \"links\": {link_count},\n    \"tag_aliases\": {alias_count}\n  }},\n  \"db\": {{\n    \"path\": {},\n    \"size_bytes\": {size},\n    \"last_touch\": {last_val}\n  }}\n}}",
            if root_items.is_empty() { String::new() } else { root_items.join(",") },
            if root_items.is_empty() { "" } else { " " },
            jstr(&db_path),
        );
        return Ok(EXIT_OK);
    }

    println!("\n== totals ==");
    println!("pages:        {total}");
    println!("tags:         {tag_count}");
    println!("links:        {link_count}");
    println!("tag-aliases:  {alias_count}");
    println!("db file size: {size} bytes ({db_path})");
    if let Some(ns) = last {
        println!("last touch:   {:.1}s since unix epoch", ns as f64 / 1e9);
    } else {
        println!("last touch:   (never)");
    }
    Ok(EXIT_OK)
}

// ---- shell-quote helper (for kb cron) ----

