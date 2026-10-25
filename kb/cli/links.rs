use crate::*;

pub(crate) fn cmd_links(dbpath: Option<&Path>, target: &str, json: bool) -> Kb {
    let db = open_db(dbpath)?;

    // Resolve the target to an abs_path. It could be:
    // - an absolute path (/home/u/notes/foo.md)
    // - a relative-ish path (resolved against the page's root)
    // - a display id (wiki:slug, label:slug, note:slug)
    let abs_path: Option<String> = if target.starts_with('/') {
        Some(target.to_string())
    } else if target.contains(':') {
        // display id
        let mut stmt = db.conn
            .prepare("SELECT abs_path FROM pages WHERE id = ?1")
            .map_err(|e| (format!("prepare id lookup: {e}"), EXIT_ERR))?;
        stmt.query_row(rusqlite::params![target], |r| r.get::<_, String>(0))
            .optional()
            .map_err(|e| (format!("id lookup: {e}"), EXIT_ERR))?
    } else {
        // treat as path — try matching against abs_path (exact or relative)
        let mut stmt = db.conn
            .prepare("SELECT abs_path FROM pages WHERE abs_path = ?1 OR abs_path LIKE '%' || ?1")
            .map_err(|e| (format!("prepare path lookup: {e}"), EXIT_ERR))?;
        stmt.query_row(rusqlite::params![target], |r| r.get::<_, String>(0))
            .optional()
            .map_err(|e| (format!("path lookup: {e}"), EXIT_ERR))?
    };

    let Some(abs_path) = abs_path else {
        return Err((format!("no page found for {target:?}"), EXIT_ERR));
    };

    // Load the corpus once — used both to resolve the *current* page's forward
    // wikilinks (so a `[[B]]` in this page's body is shown as the page it
    // points to, not the raw "B") and to build the true backlink set (pages
    // whose extracted forward links resolve to `abs_path`).
    let corpus_pages = load_corpus(&db)?;
    let abs_index: std::collections::HashMap<String, &resolver::Page> =
        corpus_pages.iter().map(|p| (p.abs_path.clone(), p)).collect();

    // forward links — resolve raw wikilink/path targets to their page id.
    // Collect into (to_path, style, Some(resolved_abs) | None) so the human +
    // JSON renderers share the exact same resolution.
    let mut fwd_stmt = db.conn
        .prepare("SELECT to_path, style FROM links WHERE from_path = ?1 ORDER BY to_path, style")
        .map_err(|e| (format!("prepare fwd: {e}"), EXIT_ERR))?;
    let forward_raw: Vec<(String, String)> = fwd_stmt
        .query_map(rusqlite::params![abs_path], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| (format!("fwd query: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("fwd rows: {e}"), EXIT_ERR))?;
    let forward: Vec<(String, String, Option<String>)> = forward_raw
        .iter()
        .map(|(to_path, style)| {
            let resolved = resolve_target(&corpus_pages, to_path);
            (to_path.clone(), style.clone(), resolved)
        })
        .collect();

    // backlinks — pages whose forward edge *resolves to* this page. We must
    // iterate the corpus (not `WHERE to_path = ?` as the old SQL did) because
    // a wikilink's raw written target text is stored, not the page's
    // abs_path. A raw target text that fails to resolve is a *dead* link and
    // does NOT contribute a backlink (§4 collision is legal; §8 says the
    // indexer leaves to_path as a pending ref).
    // SECURITY_REPORT 3.17: load ALL forward edges once (one `prepare` + one
    // full-scan) into a `BTreeMap<from_path, Vec<to_path>>` instead of
    // re-preparing the statement per corpus page (N prepares for N pages).
    let edges_by_from: std::collections::BTreeMap<String, Vec<String>> = {
        let mut e = db.conn
            .prepare("SELECT from_path, to_path FROM links")
            .map_err(|e| (format!("prepare back-edge: {e}"), EXIT_ERR))?;
        let rows = e.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| (format!("back-edge query: {e}"), EXIT_ERR))?;
        let mut map: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for row in rows {
            let (from, to) = row.map_err(|e| (format!("back-edge rows: {e}"), EXIT_ERR))?;
            map.entry(from).or_default().push(to);
        }
        map
    };
    let mut back: Vec<String> = Vec::new();
    for (from_abs, _p) in corpus_pages
        .iter()
        .map(|p| (p.abs_path.clone(), p))
        .filter(|(src,_)| *src != abs_path)
    {
        // look up this page's forward edges (already loaded) and check if any
        // resolve to abs_path.
        let edges = edges_by_from.get(&from_abs);
        let mut hit = false;
        if let Some(targets) = edges {
            for target_text in targets {
                if resolve_target(&corpus_pages, target_text).as_deref() == Some(abs_path.as_str()) {
                    hit = true;
                    break;
                }
            }
        }
        if hit { back.push(from_abs.clone()); }
    }
    back.sort();
    back.dedup();

    // ---- JSON ---------------------------------------------------------
    if json {
        let fwd_items: Vec<String> = forward.iter().map(|(to_path, style, resolved)| {
            format!(
                "{{\n    \"style\": {},\n    \"to_path\": {},\n    \"resolves_to\": {}\n  }}",
                jstr(style),
                jstr(to_path),
                match resolved {
                    Some(r) => jstr(r),
                    None => "null".to_string(),
                },
            )
        }).collect();
        let back_items: Vec<String> = back.iter().map(|from_path| {
            format!(
                "{{\n    \"abs_path\": {},\n    \"id\": {}\n  }}",
                jstr(from_path),
                match abs_index.get(from_path) {
                    Some(p) => jstr(&format!("{}:{}", p.typ, p.slug)),
                    None => "null".to_string(),
                },
            )
        }).collect();
        println!(
            "{{\n  \"page\": {},\n  \"forward\": [{}],\n  \"backlinks\": [{}]\n}}",
            jstr(&abs_path),
            fwd_items.join(","),
            back_items.join(","),
        );
        return Ok(EXIT_OK);
    }

    // ---- human ---------------------------------------------------------
    println!("page: {abs_path}");

    if forward.is_empty() {
        println!("\nforward links: none");
    } else {
        println!("\nforward links ({}):", forward.len());
        for (to_path, style, resolved) in &forward {
            let annotated = match resolved {
                Some(r) => {
                    let display = abs_index
                        .get(r)
                        .map(|p| format!("{}:{}", p.typ, p.slug))
                        .unwrap_or(r.clone());
                    format!("  → {display}")
                }
                None => "  (unresolved)".to_string(),
            };
            println!("  [ {style} ]  {to_path}{annotated}");
        }
    }

    if back.is_empty() {
        println!("\nbacklinks: none");
    } else {
        println!("\nbacklinks ({}):", back.len());
        let _ = &abs_index; // (kept for symmetry — the corpus is what we used)
        for from_path in &back {
            if let Some(p) = abs_index.get(from_path) {
                println!("  {from_path}  ({}:{})", p.typ, p.slug);
            } else {
                println!("  {from_path}");
            }
        }
    }

    Ok(EXIT_OK)
}

// ---- resolve ----


pub(crate) fn cmd_resolve(dbpath: Option<&Path>, query: &str, json: bool) -> Kb {
    let db = open_db(dbpath)?;
    let pages = load_corpus(&db)?;
    let outcome = resolver::resolve(query, &pages);
    // Map each resolved `abs_path` back to its page so we can render the
    // display id (`type:slug`), not just the path — the shell's contract is
    // "show me the candidate list", and the pure core's outcome (an
    // abs_path set) is exactly the identity key we index on.
    let idx: std::collections::HashMap<&str, &resolver::Page> =
        pages.iter().map(|p| (p.abs_path.as_str(), p)).collect();
    let resolved: Vec<String> = match &outcome {
        resolver::Resolution::Unique(p) => vec![p.clone()],
        resolver::Resolution::Candidates(v) => v.clone(),
        resolver::Resolution::Empty => Vec::new(),
    };
    let kind = match &outcome {
        resolver::Resolution::Unique(_) => "unique",
        resolver::Resolution::Candidates(v) if v.len() > 1 => "candidates",
        resolver::Resolution::Candidates(_) => "unique",
        resolver::Resolution::Empty => "empty",
    };
    if json {
        let items: Vec<String> = resolved.iter().map(|abs| {
            let p = idx.get(abs.as_str()).unwrap();
            let mut o = JObject::new();
            o.s("id", &format!("{}:{}", p.typ, p.slug));
            o.s("type", &p.typ);
            o.s("slug", &p.slug);
            o.s("abs_path", &p.abs_path);
            o.render()
        }).collect();
        let mut o = JObject::new();
        o.s("query", query);
        o.s("outcome", kind);
        o.raw("candidates", block_array(&items));
        println!("{}", o.render());
        return Ok(EXIT_OK);
    }
    if resolved.is_empty() {
        println!("no page matches {query:?}");
        return Ok(EXIT_OK);
    }
    if resolved.len() == 1 {
        // a clean unique hit — render the page + a "resolved to" marker so the
        // user can see the resolution terminated.
        let p = idx.get(resolved[0].as_str()).unwrap();
        println!(
            "1  {:<24} {:<6} {}  {}   [resolved to abs_path]",
            format!("{}:{}", p.typ, p.slug), p.typ, p.slug, p.abs_path
        );
    } else {
        // a collision — the §4 contract: a candidate list, never a hard error.
        for (i, abs) in resolved.iter().enumerate() {
            let p = idx.get(abs.as_str()).unwrap();
            println!(
                "{:>3}  {:<24} {:<6} {}  {}   [candidate]",
                i + 1, format!("{}:{}", p.typ, p.slug), p.typ, p.slug, p.abs_path
            );
        }
        println!(
            "\n{} candidates — disambiguate with a `type:slug` id or a more specific slug/substring",
            resolved.len()
        );
    }
    Ok(EXIT_OK)
}

// ---- info ----

