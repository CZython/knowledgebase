use crate::*;

/// Look up the page we are about to inspect from `kb links <TARGET>`.
/// Four input shapes are accepted (A2 extraction from `cmd_links`):
///
///   * an absolute path — used verbatim
///   * a display id (`wiki:slug`, `label:slug`, `note:slug`) — looked up
///     through `pages.id`
///   * a **uuid token** (5–16 hex) — looked up through the `pages.uuid`
///     index (`WHERE uuid LIKE ? '%' LIMIT 2`). A full 16-hex token is always
///     unique; a short token may be 0/1/≥2 — 0 and ≥2 are surfaced as
///     "no page found" and "ambiguous (extend the prefix)", respectively.
///   * a relative / partial path — matched by exact or by `LIKE '%target'`
///     (trailing-fragment lookup, mirroring the old `OR abs_path LIKE` clause)
pub(crate) fn resolve_target_path(
    db: &Db,
    target: &str,
) -> Result<Option<String>, (String, i32)> {
    if target.starts_with('/') {
        return Ok(Some(target.to_string()));
    }
    if target.contains(':') {
        let stmt = db.conn
            .prepare("SELECT abs_path FROM pages WHERE id = ?1")
            .map_err(|e| (format!("prepare id lookup: {e}"), EXIT_ERR))?;
        return stmt
            .query_row(rusqlite::params![target], |r| r.get::<_, String>(0))
            .optional()
            .map_err(|e| (format!("id lookup: {e}"), EXIT_ERR));
    }
    if kb_core::uuid::is_uuid_token(target) {
        let like = format!("{target}%");
        let rows: Vec<String> = {
            let stmt = db.conn
                .prepare("SELECT abs_path FROM pages WHERE uuid LIKE ?1 LIMIT 2")
                .map_err(|e| (format!("prepare uuid lookup: {e}"), EXIT_ERR))?;
            stmt.query_map(rusqlite::params![like], |r| r.get::<_, String>(0))
                .map_err(|e| (format!("uuid lookup rows: {e}"), EXIT_ERR))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| (format!("uuid lookup rows: {e}"), EXIT_ERR))?
        };
        return match rows.len() {
            1 => Ok(Some(rows.into_iter().next().unwrap())),
            0 => Ok(None),
            n => Err((
                format!("uuid prefix {target} is ambiguous ({n} pages) — extend the prefix"),
                EXIT_ERR,
            )),
        };
    }
    let stmt = db.conn
        .prepare("SELECT abs_path FROM pages WHERE abs_path = ?1 OR abs_path LIKE '%' || ?1")
        .map_err(|e| (format!("prepare path lookup: {e}"), EXIT_ERR))?;
    stmt.query_row(rusqlite::params![target], |r| r.get::<_, String>(0))
        .optional()
        .map_err(|e| (format!("path lookup: {e}"), EXIT_ERR))
}


pub(crate) fn cmd_links(dbpath: Option<&Path>, target: &str, dedup: bool, json: bool) -> Kb {
    let db = open_db(dbpath)?;

    let Some(abs_path) = resolve_target_path(&db, target)? else {
        return Err((format!("no page found for {target:?}"), EXIT_ERR));
    };

    let corpus_pages = load_corpus(&db)?;

    // `--dedup` is the legacy per-edge view: the `links` table (dedup'd
    // `(target, style)` pair set) plus backlinks. The *default* (below) is
    // the per-occurrence view, which reads the page's body directly
    // (not the `links` table) and resolves every occurrence against the
    // corpus; it does NOT compute backlinks (a list is about forward edges;
    // the edge-set path owns backlinks).
    if dedup {
        // Load the corpus once — used both to resolve the *current* page's
        // forward wikilinks (so a `[[B]]` in this page's body is shown as
        // the page it points to, not the raw "B") and to build the true
        // backlink set (pages whose extracted forward links resolve to
        // `abs_path`).
        let abs_index: std::collections::HashMap<String, &resolver::Page> =
            corpus_pages.iter().map(|p| (p.abs_path.clone(), p)).collect();

        // forward links — resolve raw wikilink/path targets to their page id.
        // Collect into (to_path, style, Some(resolved_abs) | None) so the
        // human + JSON renderers share the exact same resolution.
        let fwd_stmt = db.conn
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

        // backlinks — pages whose forward edge *resolves to* this page. We
        // must iterate the corpus (not `WHERE to_path = ?` as the old SQL
        // did) because a wikilink's raw written target text is stored, not
        // the page's abs_path. A raw target text that fails to resolve is a
        // *dead* link and does NOT contribute a backlink (§4 collision is
        // legal; §8 says the indexer leaves to_path as a pending ref).
        // load ALL forward edges once (one `prepare` +
        // one full-scan) into a `BTreeMap<from_path, Vec<to_path>>` instead
        // of re-preparing the statement per corpus page (N prepares for N
        // pages).
        let edges_by_from: std::collections::BTreeMap<String, Vec<String>> = {
            let e = db.conn
                .prepare("SELECT from_path, to_path FROM links")
                .map_err(|e| (format!("prepare back-edge: {e}"), EXIT_ERR))?;
            let rows = e.query_map((), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
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
            // look up this page's forward edges (already loaded) and check if
            // any resolve to abs_path.
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

        // ---- JSON + human renderers share the same resolution (A2 extraction) --
        render_links(&db, &abs_path, &forward, &back, &abs_index, json);
        return Ok(EXIT_OK);
    }

    // ---- default: the per-occurrence annotated list ----
    let body: String = db
        .conn
        .query_row(
            "SELECT COALESCE(body, '') FROM pages WHERE abs_path = ?1",
            [abs_path.as_str()],
            |r| r.get(0),
        )
        .map_err(|e| (format!("body lookup for {abs_path}: {e}"), EXIT_ERR))?;
    let links = annotated_links(&body, &corpus_pages);
    render_links_list(&abs_path, &links, json);
    Ok(EXIT_OK)
}


/// Render a page's per-occurrence link list in the requested format
/// (shared by `kb links` human + `--json` paths — the default view — so the
/// two can never disagree: same input, same field order).
///
/// `list` is *numbered* in document order: the agent can refer back to
/// `links[i].line` / `links[i].target` without disambiguating by target
/// (which, per the §4 contract, may collide).
fn render_links_list(
    abs_path: &str,
    links: &[crate::AnnotatedLink],
    json: bool,
) {
    if json {
        let items: Vec<String> = links
            .iter()
            .enumerate()
            .map(|(idx, l)| {
                let style = l.kind.style_str(&l.target);
                let mut o = JObject::new();
                o.i("n", idx as i64);
                o.i("line", l.line as i64);
                o.i("col", l.col as i64);
                o.s("context", &l.context);
                o.s("target", &l.target);
                o.s("style", style);
                if let Some(label) = &l.label {
                    o.s("label", label);
                }
                if let Some(section) = &l.section {
                    o.s("section", section);
                }
                o.opt("resolves_to", &l.resolves_to);
                if !l.candidates.is_empty() {
                    o.raw("candidates", inline_array(&l.candidates));
                }
                if let Some((typ, slug)) = &l.resolved_id {
                    o.s("resolved_id", &format!("{typ}:{slug}"));
                }
                o.render()
            })
            .collect();
        let mut o = JObject::new();
        o.s("page", abs_path);
        o.i("count", links.len() as i64);
        o.raw("links", block_array(&items));
        println!("{}", o.render());
    } else {
        println!("page: {abs_path}");
        if links.is_empty() {
            println!("\nlinks: none");
            return;
        }
        println!("\nlink list ({}):", links.len());
        for (idx, l) in links.iter().enumerate() {
            let style = l.kind.style_str(&l.target);
            let target = match l.label.as_deref() {
                Some(label) => format!("{label}  ({})", l.target),
                None => l.target.clone(),
            };
            let annotated = match l.resolves_to.as_deref() {
                Some(abs) => {
                    let id = l
                        .resolved_id
                        .as_ref()
                        .map(|(t, s)| format!("{t}:{s}"))
                        .unwrap_or_else(|| abs.to_string());
                    let section = match l.section.as_deref() {
                        Some(sec) => format!("  [#{sec}]"),
                        None => String::new(),
                    };
                    format!("  → {id}{section}")
                }
                None => {
                    if l.candidates.len() > 1 {
                        let n = l.candidates.len();
                        let first = l
                            .candidates
                            .first()
                            .map(|c| c.rsplit('/').next().unwrap_or(c))
                            .unwrap_or("")
                            .to_string();
                        format!("  ({} candidates — first: {first})", n)
                    } else {
                        "  (unresolved)".to_string()
                    }
                }
            };
            // For `External` / `Current` the *target* is the whole thing (a bare
            // URI, or a `#section` anchor) — there is no page to resolve to, so
            // no `→ <page>` annotation.
            let show_resolution = matches!(
                l.kind,
                kb_core::links::LinkKind::Db | kb_core::links::LinkKind::Path
            );
            let annotated = if show_resolution { annotated } else { String::new() };
            // Show the line (1-based) and context so a user reading in a
            // pager can `grep -n` / `sed -n Np` to jump.
            println!("  [{idx}] L{} [ {style} ] {target}{annotated}", l.line + 1);
            println!("        └─ {}", l.context.chars().take(140).collect::<String>());
        }
    }
}

/// Render a resolved page's forward + backlink set in the requested format.
/// JSON (stdout exactly-one-document) and the human layout both flow off the
/// same `forward` / `back` / `abs_index` inputs, so adding a new field to
/// either path is a one-line change here. (A2 extraction from `cmd_links` —
/// the two `if json { … } else { … }` blocks used to be ~70 lines inline,
/// making the verb read as three concerns when it is really one.)
fn render_links(
    db: &Db,
    abs_path: &str,
    forward: &[(String, String, Option<String>)],
    back: &[String],
    abs_index: &std::collections::HashMap<String, &resolver::Page>,
    json: bool,
) {
    // The page's own uuid: read the stored value (every row indexed by a
    // modern build has one), else re-derive via `uuid_of(id, abs_path)`
    // from the corpus so an agent can paste it back into a path-arg verb.
    let page_uuid: String = {
        let p = abs_index
            .get(abs_path)
            .expect("cmd_links resolved abs_path through the corpus; it must be in the index");
        match &p.uuid {
            Some(u) => u.clone(),
            None => {
                let id = format!("{}:{}", p.typ, p.slug);
                kb_core::uuid::uuid_of(&id, &p.abs_path)
            }
        }
    };
    let page_short = uuid_short_for(db, &page_uuid, abs_path).unwrap_or(page_uuid.clone());
    if json {
        let fwd_items: Vec<String> = forward.iter().map(|(to_path, style, resolved)| {
            let mut parts = vec![format!("\"style\": {}", jstr(style))];
            parts.push(format!("\"to_path\": {}", jstr(to_path)));
            match resolved {
                Some(r) => {
                    parts.push(format!("\"resolves_to\": {}", jstr(r)));
                    if let Some(p) = abs_index.get(r) {
                        let id = format!("{}:{}", p.typ, p.slug);
                        let u = p.uuid.clone().unwrap_or_else(|| kb_core::uuid::uuid_of(&id, &p.abs_path));
                        parts.push(format!("\"target_uuid\": {}", jstr(&u)));
                        parts.push(format!(
                            "\"target_uuid_short\": {}",
                            jstr(&uuid_short_for(db, &u, &p.abs_path).unwrap_or_else(|_| u.to_string()))
                        ));
                    }
                }
                None => parts.push("\"resolves_to\": null".to_string()),
            }
            format!("{{ {} }}", parts.join(", "))
        }).collect();
        let back_items: Vec<String> = back.iter().map(|from_path| {
            let mut parts = vec![format!("\"abs_path\": {}", jstr(from_path))];
            match abs_index.get(from_path) {
                Some(p) => {
                    let id = format!("{}:{}", p.typ, p.slug);
                    parts.push(format!("\"id\": {}", jstr(&id)));
                    let u = p.uuid.clone().unwrap_or_else(|| kb_core::uuid::uuid_of(&id, &p.abs_path));
                    parts.push(format!("\"uuid\": {}", jstr(&u)));
                    parts.push(format!(
                        "\"uuid_short\": {}",
                        jstr(&uuid_short_for(db, &u, from_path).unwrap_or_else(|_| u.to_string()))
                    ));
                }
                None => parts.push("\"id\": null".to_string()),
            }
            format!("{{ {} }}", parts.join(", "))
        }).collect();
        println!(
            "{{\n  \"page\": {},\n  \"uuid\": {},\n  \"uuid_short\": {},\n  \"forward\": [{}],\n  \"backlinks\": [{}]\n}}",
            jstr(abs_path),
            jstr(&page_uuid),
            jstr(&page_short),
            fwd_items.join(","),
            back_items.join(","),
        );
    } else {
        println!("page: {abs_path}  ({page_short})");
        if forward.is_empty() {
            println!("\nforward links: none");
        } else {
            println!("\nforward links ({}):", forward.len());
            for (to_path, style, resolved) in forward.iter() {
                let annotated = match resolved {
                    Some(r) => {
                        let (display, short) = match abs_index.get(r) {
                            Some(p) => {
                                let id = format!("{}:{}", p.typ, p.slug);
                                let u = p.uuid.clone().unwrap_or_else(|| kb_core::uuid::uuid_of(&id, &p.abs_path));
                                let short = uuid_short_for(db, &u, &p.abs_path).unwrap_or(u);
                                (id, short)
                            }
                            None => (r.clone(), String::new()),
                        };
                        if short.is_empty() {
                            format!("  → {display}")
                        } else {
                            format!("  → {display}  ({short})")
                        }
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
            let _ = abs_index; // (kept for symmetry — the corpus is what we used)
            for from_path in back.iter() {
                if let Some(p) = abs_index.get(from_path) {
                    let id = format!("{}:{}", p.typ, p.slug);
                    let u = p.uuid.clone().unwrap_or_else(|| kb_core::uuid::uuid_of(&id, &p.abs_path));
                    let short = uuid_short_for(db, &u, &p.abs_path).unwrap_or_else(|_| u.to_string());
                    println!("  {from_path}  ({id})  ({short})");
                } else {
                    println!("  {from_path}");
                }
            }
        }
    }
}

// ---- resolve ----


pub(crate) fn cmd_resolve(dbpath: Option<&Path>, query: &str, json: bool) -> Kb {
    let db = open_db(dbpath)?;
    // A hex uuid token (5–16) is unambiguous by definition: it resolves
    // through the `uuid` index, not the slug/slug-substring resolver. Short-
    // circuit here so `kb resolve <uuid>` behaves the same as
    // `kb resolve <type:slug>` — one row, one page. 0 rows / ≥2 rows are
    // surfaced exactly like the resolver's Empty / Candidates arms (the
    // ≥2 arm is the "ambiguous — extend the prefix" error).
    if kb_core::uuid::is_uuid_token(query) {
        let like = format!("{query}%");
        let rows: Vec<String> = {
            let stmt = db
                .conn
                .prepare("SELECT abs_path FROM pages WHERE uuid LIKE ?1 LIMIT 2")
                .map_err(|e| (format!("prepare uuid probe: {e}"), EXIT_ERR))?;
            stmt.query_map(rusqlite::params![like], |r| r.get::<_, String>(0))
                .map_err(|e| (format!("uuid probe rows: {e}"), EXIT_ERR))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| (format!("uuid probe rows: {e}"), EXIT_ERR))?
        };
        return match rows.len() {
            1 => {
                let abs = rows.into_iter().next().unwrap();
                let (id, uuid) = db
                    .conn
                    .query_row(
                        "SELECT id, uuid FROM pages WHERE abs_path = ?1",
                        rusqlite::params![abs.clone()],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                    )
                    .unwrap_or_else(|_| (abs.clone(), None));
                let uuid = uuid.unwrap_or_else(|| {
                    // Legacy row without a stored uuid: re-derive from the
                    // (id, abs_path) pair — the same content the indexer used.
                    kb_core::uuid::uuid_of(&id, &abs)
                });
                let uuid_short = uuid_short_for(&db, &uuid, &abs).unwrap_or(uuid.clone());
                if json {
                    let mut o = JObject::new();
                    o.s("query", query);
                    o.s("outcome", "unique");
                    let mut c = JObject::new();
                    c.s("id", &id);
                    c.s("uuid", &uuid);
                    c.s("uuid_short", &uuid_short);
                    c.s("abs_path", &abs);
                    o.raw("candidates", block_array(&[c.render_compact()]));
                    println!("{}", o.render_compact());
                } else {
                    println!("1  {uuid_short}  {id}  {}   [resolved via uuid]", abs);
                }
                return Ok(EXIT_OK);
            }
            0 => {
                if json {
                    let mut o = JObject::new();
                    o.s("query", query);
                    o.s("outcome", "empty");
                    o.raw("candidates", block_array(&[]));
                    println!("{}", o.render_compact());
                } else {
                    println!("no page matches {query:?}");
                }
                return Ok(EXIT_OK);
            }
            n => Err((
                format!("uuid prefix {query} is ambiguous ({n} pages) — extend the prefix"),
                EXIT_ERR,
            )),
        };
    }
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
            // kb-core::resolve builds the outcome by filtering the input
            // `pages` vector; every `abs_path` it returns is (by
            // construction) some `p.abs_path.as_str()` from `pages`,
            // which is exactly the key `idx` is indexed on. If this
            // ever fires, the invariant that "outcome ⊆ input pages"
            // no longer holds in kb-core::resolve — see the spec at
            // kb-core/spec/resolve_spec.rs.
            let p = idx.get(abs.as_str())
                .expect("resolve() returned an abs_path not present in the input pages");
            let mut o = JObject::new();
            let id = format!("{}:{}", p.typ, p.slug);
            o.s("id", &id);
            // The page uuid, so a candidate can be pasted straight back into a
            // path-arg verb without re-typing the slug. Derived if the row
            // predates the stored-uuid backfill (see the hex-uuid arm above).
            let uuid = p.uuid.clone().unwrap_or_else(|| kb_core::uuid::uuid_of(&id, &p.abs_path));
            o.s("uuid", &uuid);
            o.s("uuid_short", &uuid_short_for(&db, &uuid, &p.abs_path).unwrap_or(uuid));
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
        // Invariant: kb-core::resolve filters the input `pages` vector; every
        // `abs_path` it returns is some `p.abs_path.as_str()` from `pages`,
        // which is exactly the key `idx` is indexed on (see
        // kb-core/spec/resolve_spec.rs). Panic = that invariant broke.
        let p = idx.get(resolved[0].as_str())
            .expect("resolve() returned an abs_path not present in the input pages");
        let id = format!("{}:{}", p.typ, p.slug);
        let uuid = p.uuid.clone().unwrap_or_else(|| kb_core::uuid::uuid_of(&id, &p.abs_path));
        let short = uuid_short_for(&db, &uuid, &p.abs_path).unwrap_or_else(|_| uuid.clone());
        println!(
            "1  {short}  {id}  {}   [resolved to abs_path]",
            p.abs_path
        );
    } else {
        // a collision — the §4 contract: a candidate list, never a hard error.
        for (i, abs) in resolved.iter().enumerate() {
            // Same invariant as the `Unique` arm above — every `abs_path` in
            // the candidate list is a `p.abs_path.as_str()` from `pages`.
            let p = idx.get(abs.as_str())
                .expect("resolve() returned an abs_path not present in the input pages");
            let id = format!("{}:{}", p.typ, p.slug);
            let uuid = p.uuid.clone().unwrap_or_else(|| kb_core::uuid::uuid_of(&id, &p.abs_path));
            let short = uuid_short_for(&db, &uuid, &p.abs_path).unwrap_or_else(|_| uuid.clone());
            println!(
                "{:>3}  {short}  {id}  {}   [candidate]",
                i + 1, p.abs_path
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

