use crate::*;

pub(crate) fn cmd_tag(dbpath: Option<&Path>, tag: &str, json: bool) -> Kb {
    let db = open_db(dbpath)?;

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
    let match_set: Vec<String> = kbtags::canonicalize(tag, &all_tags, &aliases)
        .into_iter().map(|s: &str| s.to_string()).collect();
    let _ = (all_tags, aliases);
    if match_set.is_empty() {
        if json {
            println!("{}", "[]" as &str);
        } else {
            println!("no pages tagged with {tag:?} (or any alias)");
        }
        return Ok(EXIT_OK);
    }

    // SECURITY_REPORT 3.4: the left side of the comparison is
    // `lower(pt.tag)`, so the IN-list must be lowercased too. `canonicalize`
    // returns the *stored* strings in their original casing, so a stored
    // mixed-case tag like `Czech` would otherwise fail `lower('Czech')
    // IN ('Czech',…)` → no match. Folding to lowercase matches both sides.
    let in_list: Vec<String> = match_set
        .iter()
        .map(|t| sql_quote_lower(t))
        .collect();

    let sql = format!(
        "SELECT pages.abs_path, pages.id, pages.type, pages.title, pages.root, pages.rel_path\n\
         FROM pages\n\
         JOIN page_tags pt ON pt.abs_path = pages.abs_path\n\
         WHERE lower(pt.tag) IN ({})\n\
         ORDER BY pages.root, pages.rel_path",
        in_list.join(",")
    );

    let stmt = db.conn.prepare(&sql)
        .map_err(|e| (format!("prepare tag query: {e}"), EXIT_ERR))?;
    let pages: Vec<(String, String, String, Option<String>, String, String)> = stmt
        .query_map((), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| (format!("tag query: {e}"), EXIT_ERR))?
        .collect::<Result<_, _>>()
        .map_err(|e| (format!("tag rows: {e}"), EXIT_ERR))?;

    if json {
        // An empty match-set is still a clean empty array (the agent does not
        // need the human "match-set was: ..." hint).
        if pages.is_empty() {
            // Canonical empty array — `json.loads` must see a genuinely empty
            // list (F6 / C3 exact pin, asserted in `m9_agent_surface`).
            println!("{}", top_array(&[]));
            return Ok(EXIT_OK);
        }
        let mut items = Vec::new();
        for (abs_path, id, pt, title, _root, rel_path) in pages.iter() {
            let tags: Vec<String> = page_tags_str(&db, abs_path).split(',')
                .filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string()).collect();
            let refs = if pt == "label" { Some(label_expand(&db, abs_path)) } else { None };
            let gj = PageJson {
                id: id.clone(), pt: pt.clone(),
                title: title.clone(), abs: abs_path.clone(), rel: rel_path.clone(),
                tags, gates: String::new(),
                links: None, refs, description: None,
            };
            items.push(gj.render());
        }
        println!("{}", top_array(&items));
        return Ok(EXIT_OK);
    }

    if pages.is_empty() {
        println!("no pages tagged with {tag:?} (match-set was: {match_set:?})");
        return Ok(EXIT_OK);
    }

    for (i, (abs_path, id, pt, title, _root, rel_path)) in pages.iter().enumerate() {
        let tags = page_tags_str(&db, abs_path);
        let mut line = format!("{:>3}  {:<24} {}", i + 1, id, title.as_deref().unwrap_or(""));
        if !rel_path.is_empty() {
            line.push_str(&format!("  {rel_path}"));
        }
        if !tags.is_empty() {
            line.push_str(&format!("  tags: {tags}"));
        }
        println!("{line}");

        if pt == "label" {
            let refs = label_expand(&db, abs_path);
            for r in &refs {
                println!("     → {r}");
            }
        }
    }
    println!("── {} tagged page(s) for {tag:?}", pages.len());
    Ok(EXIT_OK)
}

// ---- tag-alias ----


/// Read the full `tag_aliases` table, sorted by alias (deterministic).
pub(crate) fn read_all_aliases(db: &Db) -> Result<Vec<(String, String)>, (String, i32)> {
    let stmt = db.conn
        .prepare("SELECT alias, canonical FROM tag_aliases ORDER BY alias")
        .map_err(|e| (format!("prepare: {e}"), EXIT_ERR))?;
    let rows = stmt
        .query_map((), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| (format!("query: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("rows: {e}"), EXIT_ERR))?;
    Ok(rows)
}


pub(crate) struct TagAliasOpts {
    pub(crate) add:Option<String>,
    pub(crate) canonical:Option<String>,
    pub(crate) rm:Option<String>,
    pub(crate) list:bool,
    pub(crate) export:bool,
    pub(crate) import:bool,
    pub(crate) file:Option<PathBuf>,
}


pub(crate) fn cmd_tag_alias(dbpath: Option<&Path>, o: &TagAliasOpts, json: bool) -> Kb {
    let add = o.add.as_deref();
    let canonical = o.canonical.as_deref();
    let rm = o.rm.as_deref();
    let list = o.list;
    let export = o.export;
    let import = o.import;
    let file = o.file.as_deref();
    let db = open_db(dbpath)?;

    // ---- export / import (the M8 recovery pathway) -----------------------
    if export {
        let aliases = read_all_aliases(&db)?;
        let text = kbalias::to_text(&aliases);
        let n = aliases.len();
        match file {
            Some(p) => {
                // SECURITY_REPORT 3.20 / CODEBASE_REVIEW F2: the alias TSV is
                // the M8 recovery pathway — an operator may export it to a
                // shared directory, so a pre-planted symlink at the target was
                // the same `O_CREAT|O_TRUNC` race as 3.19. Route it through
                // `atomic_write` (unguessable temp + same-dir rename) instead
                // of a bare `std::fs::write` — export/import now share one
                // write-side discipline (the C4 / F2 single-owner pattern).
                atomic_write(p, text.as_bytes())
                    .map_err(|e| (format!("write {}: {e}", p.display()), EXIT_ERR))?;
                println!("exported {n} alias row(s) to {}", p.display());
            }
            None => {
                // stdout (the caller can redirect to the recovery file:
                // `kb tag-alias --export > backup.tsv`).
                print!("{text}");
            }
        }
        return Ok(EXIT_OK);
    }

    if import {
        return do_import(&db, file, json);
    }

    if list {
        let stmt = db.conn
            .prepare("SELECT alias, canonical FROM tag_aliases ORDER BY alias")
            .map_err(|e| (format!("prepare: {e}"), EXIT_ERR))?;
        let aliases = stmt
            .query_map((), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| (format!("query: {e}"), EXIT_ERR))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| (format!("rows: {e}"), EXIT_ERR))?;
        if json {
            let items: Vec<String> = aliases.iter().map(|(alias, canonical)| {
                let mut o = JObject::new();
                o.s("alias", alias);
                o.s("canonical", canonical);
                o.render()
            }).collect();
            println!("{}", top_array(&items));
            return Ok(EXIT_OK);
        }
        if aliases.is_empty() {
            println!("(no tag aliases defined)");
        }
        for (alias, canonical) in aliases {
            println!("{alias}  →  {canonical}");
        }
        return Ok(EXIT_OK);
    }

    if let Some(alias) = rm {
        let n = db.conn
            .execute(
                "DELETE FROM tag_aliases WHERE alias = ?1",
                rusqlite::params![alias],
            )
            .map_err(|e| (format!("delete alias: {e}"), EXIT_ERR))?;
        if json {
            println!(
                "{{\n  \"alias\": {},\n  \"removed\": {}\n}}",
                jstr(alias),
                n,
            );
        } else if n == 0 {
            println!("no alias {alias:?} found — nothing removed");
        } else {
            println!("removed alias: {alias:?}");
        }
        return Ok(EXIT_OK);
    }

    let Some(alias) = add else {
        return Err(("--add <alias> --canonical <tag> | --rm <alias> | --list | --export | --import required".into(), EXIT_ERR));
    };
    let Some(canonical) = canonical else {
        return Err((format!("--add {alias:?} requires --canonical <tag>"), EXIT_ERR));
    };
    db.conn
        .execute(
            "INSERT INTO tag_aliases(alias, canonical) VALUES (?1, ?2)\n\
             ON CONFLICT(alias) DO UPDATE SET canonical = excluded.canonical",
            rusqlite::params![alias, canonical],
        )
        .map_err(|e| (format!("insert alias: {e}"), EXIT_ERR))?;
    if json {
        println!(
            "{{\n  \"alias\": {},\n  \"canonical\": {}\n}}",
            jstr(alias),
            jstr(canonical),
        );
    } else {
        println!("alias: {alias:?} → {canonical:?}");
    }
    Ok(EXIT_OK)
}


/// `--import` path for `kb tag-alias` (A2 extraction from `cmd_tag_alias`).
/// Self-contained: reads a backup (file or stdin), walks it line-by-line
/// through the same `kbalias::parse_line` / `line_is_ignorable` codec the
/// pure core uses (so the codec stays the single source of truth for what
/// counts as a data line), and upserts under a *checked* transaction — raw
/// `BEGIN` / `COMMIT` via `execute_batch` left the transaction open if any
/// step in between returned `Err` (SECURITY_REPORT 3.8); `checked_transaction`
/// issues `ROLLBACK` on `Err` unwind.
fn do_import(db: &Db, file: Option<&Path>, json: bool) -> Kb {
    let text = match file {
        Some(p) => std::fs::read_to_string(p)
            .map_err(|e| (format!("read {}: {e}", p.display()), EXIT_ERR))?,
        None => {
            // stdin (so `kb tag-alias --import < backup.tsv` works).
            let mut buf = String::new();
            use std::io::Read;
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| (format!("read stdin: {e}"), EXIT_ERR))?;
            buf
        }
    };
    // Report the *skips* (malformed / non-data lines) before loading, so a
    // partially-typed backup is never silently truncated.
    let mut loaded = 0usize;
    let mut skipped = 0usize;
    let mut rows: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        match kbalias::parse_line(line) {
            Some((a, c)) => {
                if let Some(slot) = rows.iter_mut().find(|(x, _)| x == a) {
                    slot.1 = c.to_string();
                } else {
                    rows.push((a.to_string(), c.to_string()));
                    loaded += 1;
                }
            }
            None => {
                if !kbalias::line_is_ignorable(line) {
                    skipped += 1;
                }
            }
        }
    }
    let tx = db
        .conn
        .unchecked_transaction()
        .map_err(|e| (format!("begin import: {e}"), EXIT_ERR))?;
    {
        let upsert = tx
            .prepare(
                "INSERT INTO tag_aliases(alias, canonical) VALUES (?1, ?2)\n\
                  ON CONFLICT(alias) DO UPDATE SET canonical = excluded.canonical",
            )
            .map_err(|e| (format!("prepare import: {e}"), EXIT_ERR))?;
        for (a, c) in &rows {
            upsert
                .execute(rusqlite::params![a, c])
                .map_err(|e| (format!("upsert import: {e}"), EXIT_ERR))?;
        }
    }
    tx.commit()
        .map_err(|e| (format!("commit import: {e}"), EXIT_ERR))?;
    if json {
        let mut o = JObject::new();
        o.i("loaded", loaded as i64);
        o.i("skipped", skipped as i64);
        println!("{}", o.render());
    } else {
        println!(
            "imported {loaded} alias row(s) (last-wins on duplicate aliases); {skipped} line(s) skipped (blank/comment/malformed)",
        );
    }
    Ok(EXIT_OK)
}

// ---- links ----

