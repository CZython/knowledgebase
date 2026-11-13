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
    // `wc` (lines/chars/words) of the page body. Disk is the source of truth
    // (the file may be newer than the cache); fall back to the cached DB body
    // when the file is missing (a page removed from disk but still indexed) so
    // the metric degrades instead of erroring.
    let body_str: String = match std::fs::read_to_string(&abs_path) {
        Ok(s) => s,
        Err(_) => db.conn
            .query_row(
                "SELECT COALESCE(body, '') FROM pages WHERE abs_path = ?1",
                params![abs_path],
                |r| r.get::<_, String>(0),
            )
            .unwrap_or_default(),
    };
    let (wc_lines, wc_chars, wc_words) = text_wc(&body_str);
    if json {
        let tag_arr: Vec<String> = tags.split(',')
            .filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string()).collect();
        let mut o = JObject::new();
        o.s("id", &id);
        o.s("type", &pt);
        o.opt("title", &title);
        o.s("abs_path", &abs_path);
        o.s("rel_path", &_rel_path);
        o.opt("description", &description);
        o.s("search_required_phrase", &search_required_phrase);
        o.raw("tags", inline_array(&tag_arr));
        let mut links = JObject::new();
        links.i("out", fwd_count);
        links.i("in", back_count);
        o.raw("links", links.render());
        let mut wc = JObject::new();
        wc.i("lines", wc_lines as i64);
        wc.i("chars", wc_chars as i64);
        wc.i("words", wc_words as i64);
        o.raw("wc", wc.render_compact());
        o.n1("mtime", secs_f);
        o.opt("created_at", &created_at);
        println!("{}", o.render());
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
    println!("wc:      {wc_lines} lines, {wc_chars} chars, {wc_words} words");
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
        let stmt = db.conn
            .prepare("SELECT path, extra_exts, include_hidden FROM roots ORDER BY seq DESC")
            .map_err(|e| (format!("prepare roots: {e}"), EXIT_ERR))?;
        let rows = stmt.query_map((), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        }).map_err(|e| (format!("roots: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("roots rows: {e}"), EXIT_ERR))?;
        rows
    };
    let mut root_stats: Vec<(String, Option<Vec<String>>, i32, i64)> = Vec::new();
    if json {
        for (root, extra_exts, include_hidden) in &roots {
            // B7: `COUNT(*)` can't be NULL or error in practice, but a DB error
            // is a real failure — surface it rather than reporting `0 page(s)`.
            let count: i64 = db.conn
                .query_row(
                    "SELECT COUNT(*) FROM pages WHERE root = ?1",
                    params![root],
                    |r| r.get(0),
                ).map_err(|e| (format!("stats count pages for root '{root}': {e}"), EXIT_ERR))?;
            let exts = extra_exts.as_ref().and_then(|e| {
                if e.is_empty() { None }
                else { Some(e.split('\n').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect()) }
            });
            root_stats.push((root.clone(), exts, *include_hidden, count));
        }
    } else {
        println!("== per-root page counts ==");
        for (root, extra_exts, include_hidden) in &roots {
            // B7 (as above): surface a DB error rather than `0 page(s)`.
            let count: i64 = db.conn
                .query_row(
                    "SELECT COUNT(*) FROM pages WHERE root = ?1",
                    params![root],
                    |r| r.get(0),
                ).map_err(|e| (format!("stats count pages for root '{root}': {e}"), EXIT_ERR))?;
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
    // B7: `COUNT(*)` belt-and-suspenders is gone — a failure here is a real DB
    // error and should surface, not be reported as a `0` total with exit 0.
    let total: i64 = db.conn
        .query_row("SELECT COUNT(*) FROM pages", (), |r| r.get(0))
        .map_err(|e| (format!("stats count pages: {e}"), EXIT_ERR))?;
    let tag_count: i64 = db.conn
        .query_row("SELECT COUNT(DISTINCT tag) FROM page_tags", (), |r| r.get(0))
        .map_err(|e| (format!("stats count tags: {e}"), EXIT_ERR))?;
    let link_count: i64 = db.conn
        .query_row("SELECT COUNT(*) FROM links", (), |r| r.get(0))
        .map_err(|e| (format!("stats count links: {e}"), EXIT_ERR))?;
    let alias_count: i64 = db.conn
        .query_row("SELECT COUNT(*) FROM tag_aliases", (), |r| r.get(0))
        .map_err(|e| (format!("stats count tag_aliases: {e}"), EXIT_ERR))?;
    // db file size (derive from `default_db_display()` — one shared,
    // OS-correct source of the default path; consistent with `open_default`,
    // the `cron` verb, and `Db::default_db_path()`).
    let db_path = dbpath
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(default_db_display);
    let size: u64 = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    // `updated_at`/`created_at` are TEXT storing epoch-seconds strings
    // (see `now_iso8601()` in kb-db). Read the TEXT, then parse to `i64` —
    // a malformed/empty value degrades to `null`, not a hard error, which
    // matches the lenient `.ok().flatten()` intent here.
    let last = db.conn
        .query_row(
            "SELECT COALESCE(MAX(updated_at), MAX(created_at)) FROM pages",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<i64>().ok());

    if json {
        let mut root_items: Vec<String> = Vec::new();
        for (path, exts, inc, count) in root_stats.iter() {
            let mut ro = JObject::new();
            ro.s("path", path);
            if let Some(v) = exts {
                ro.raw("ext", inline_array(v));
            }
            ro.b("include_hidden", *inc != 0);
            ro.i("pages", *count);
            root_items.push(ro.render());
        }
        // `last` is epoch-*seconds* (`now_iso8601()` stores `as_secs()`), so
        // emit the integer seconds verbatim — no unit conversion.
        let last_val = match last {
            Some(sec) => sec.to_string(),
            None => "null".to_string(),
        };
        let mut totals = JObject::new();
        totals.i("pages", total);
        totals.i("tags", tag_count);
        totals.i("links", link_count);
        totals.i("tag_aliases", alias_count);
        let mut dbo = JObject::new();
        dbo.s("path", &db_path);
        dbo.i("size_bytes", size as i64);
        dbo.raw("last_touch", last_val);
        let mut o = JObject::new();
        o.raw("roots", block_array(&root_items));
        o.raw("totals", totals.render());
        o.raw("db", dbo.render());
        println!("{}", o.render());
        return Ok(EXIT_OK);
    }

    println!("\n== totals ==");
    println!("pages:        {total}");
    println!("tags:         {tag_count}");
    println!("links:        {link_count}");
    println!("tag-aliases:  {alias_count}");
    println!("db file size: {size} bytes ({db_path})");
    if let Some(sec) = last {
        println!("last touch:   {sec} s since unix epoch");
    } else {
        println!("last touch:   (never)");
    }
    Ok(EXIT_OK)
}

// ---- shell-quote helper (for kb cron) ----

