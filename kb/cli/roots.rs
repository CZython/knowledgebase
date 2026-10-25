use crate::*;

pub(crate) fn cmd_roots(dbpath: Option<&Path>, json: bool) -> Kb {
    let db = open_db(dbpath)?;
    let mut stmt = db.conn
        .prepare("SELECT path, added_at, extra_exts, include_hidden\nFROM roots\nORDER BY seq DESC, path")
        .map_err(|e| (format!("prepare: {e}"), EXIT_ERR))?;
    let roots = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i32>(3)?,
            ))
        })
        .map_err(|e| (format!("query: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("rows: {e}"), EXIT_ERR))?;

    if json {
        let items: Vec<String> = roots.iter().map(|(path, added_at, extra_exts, include_hidden)| {
            let mut o = JObject::new();
            o.s("path", path);
            if let Some(a) = added_at {
                o.s("added_at", a);
            }
            if let Some(exts) = extra_exts {
                let v: Vec<String> = exts.lines().map(|l| l.to_string()).filter(|l| !l.is_empty()).collect();
                if !v.is_empty() {
                    o.raw("ext", inline_array(&v));
                }
            }
            o.b("include_hidden", *include_hidden != 0);
            o.render()
        }).collect();
        println!("{}", top_array(&items));
        return Ok(EXIT_OK);
    }

    if roots.is_empty() {
        println!("(no roots registered — use `kb add <path>` to register one)");
        return Ok(EXIT_OK);
    }
    for (path, added_at, extra_exts, include_hidden) in roots {
        let mut opts: Vec<String> = Vec::new();
        let exts: Vec<String> = extra_exts
            .map(|s| s.lines().map(|l| l.to_string()).filter(|l| !l.is_empty()).collect())
            .unwrap_or_default();
        if !exts.is_empty() {
            opts.push(format!("ext:{}", exts.join(",")));
        }
        if include_hidden != 0 {
            opts.push("include-hidden".into());
        }
        let opt_str = if opts.is_empty() { String::new() } else { format!("  [{}]", opts.join(" ")) };
        println!("{path}{opt_str}  (added: {})", added_at.as_deref().unwrap_or("?"));
    }
    Ok(EXIT_OK)
}


pub(crate) fn cmd_drop_root(dbpath: Option<&Path>, path: &Path, json: bool) -> Kb {
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| (format!("{path:?}: {e}"), EXIT_ERR))?;
    let db = open_db(dbpath)?;
    let n = db.drop_root(&canonical.to_string_lossy())
        .map_err(|e| (format!("drop_root: {e}"), EXIT_ERR))?;
    if json {
        let summary = kb_refresh::refresh(&db)
            .map_err(|e| (format!("refresh: {e}"), EXIT_ERR))?;
        println!(
            "{{\n  \"dropped\": {},\n  \"dropped_rows\": {},\n  \"refresh\": {}\n}}",
            jstr(&canonical.to_string_lossy()),
            n,
            refresh_summary_json(false, &summary),
        );
        return Ok(EXIT_OK);
    }
    if n == 0 {
        println!("no root matched {} — nothing dropped", canonical.display());
    } else {
        println!("dropped root: {}", canonical.display());
    }
    refresh_all(&db, false, false)
}


pub(crate) fn cmd_refresh(dbpath: Option<&Path>, full: bool, json: bool) -> Kb {
    let db = open_db(dbpath)?;
    refresh_all(&db, full, json)
}

