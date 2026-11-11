use crate::*;

pub(crate) fn cmd_roots(dbpath: Option<&Path>, json: bool) -> Kb {
    let db = open_db(dbpath)?;
    let stmt = db.conn
        .prepare("SELECT path, added_at, extra_exts, include_hidden\nFROM roots\nORDER BY seq DESC, path")
        .map_err(|e| (format!("prepare: {e}"), EXIT_ERR))?;
    let roots = stmt
        .query_map((), |r| {
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
    let db = open_db(dbpath)?;
    // `add` stored the *canonical* path, so canonicalizing the argument gives
    // the same form on drop. But the directory may already be *gone* (moved or
    // deleted) — `canonicalize` fails then, and the root would be undroppable.
    // Fall back to matching the literal argument against the registered roots
    // so the user can still unregister a root whose folder no longer exists
    // (dropping it is exactly what you'd want to do if you removed the dir).
    let target: String = match std::fs::canonicalize(path) {
        Ok(c) => c.to_string_lossy().to_string(),
        Err(_e) => {
            let key = path.to_string_lossy().trim_end_matches('/').to_lowercase();
            let roots = db.list_roots().map_err(|e| (format!("list roots: {e}"), EXIT_ERR))?;
            roots
                .into_iter()
                .find(|r| r.to_lowercase() == key)
                .ok_or((
                    format!("{path:?}: no longer exists on disk and no registered root matches it"),
                    EXIT_ERR,
                ))?
        }
    };
    let n = db.drop_root(&target)
        .map_err(|e| (format!("drop_root: {e}"), EXIT_ERR))?;
    if json {
        let summary = kb_refresh::refresh(&db)
            .map_err(|e| (format!("refresh: {e}"), EXIT_ERR))?;
        println!(
            "{{\n  \"dropped\": {},\n  \"dropped_rows\": {},\n  \"refresh\": {}\n}}",
            jstr(&target),
            n,
            refresh_summary_json(false, &summary),
        );
        return Ok(EXIT_OK);
    }
    if n == 0 {
        println!("no root matched {target} — nothing dropped");
    } else {
        println!("dropped root: {target}");
    }
    refresh_all(&db, false, false)
}


pub(crate) fn cmd_refresh(dbpath: Option<&Path>, full: bool, json: bool) -> Kb {
    let db = open_db(dbpath)?;
    refresh_all(&db, full, json)
}

