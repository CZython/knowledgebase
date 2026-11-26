//! Tool dispatch for the `kb mcp` surface (MCP.md "Tool surface" table).
//!
//! Slice 5: the read surface. Every handler has the same shape as its CLI
//! verb — `(String, i32)` on failure — and `dispatch` turns a failure into
//! a JSON-RPC error with a **stable code** before it crosses the wire
//! (sanitizer invariant 4: a filesystem path never appears in `message`).
//! Every successful payload passes through `sanitize_value` exactly once.

use std::io::{Read, Write};
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::*;

/// Handler signature for every tool in the surface.
type Handler = fn(&Db, &Value) -> Result<Value, (String, i32)>;

// The `mcp serve` loop holds a file-backed `Db`, but [`Db::path()`] only
// returns a value for the in-memory test opener — so the *resolved* DB
// path (the same value `serve(dbpath)` was handed) is threaded through a
// thread-local. `serve()` runs as a single-threaded stdio loop, so a
// thread-local is safe; the test suite sets it directly before calling a
// handler. `run_verb` (in `mcp/write.rs`) reads this when invoking the
// CLI verb subprocess, so the child opens the same DB file as the parent.
std::thread_local! {
    static DBPATH: std::cell::RefCell<Option<String>> = const {
        std::cell::RefCell::new(None)
    };
}

/// Set the active DB path for the duration of the enclosing
/// [`serve`]/test — call once before the first `tools/call`.
pub(crate) fn set_dbpath(p: Option<&std::path::Path>) {
    DBPATH.with(|c| *c.borrow_mut() = p.map(|p| p.display().to_string()));
}

/// Read the active DB path (None if unset — the verb subprocess then
/// falls back to the CLI default data-dir path).
///
/// `dead_code`-suppressed only when `mcp-writes` is off: in the read-only
/// build the call site (in `write.rs`) is compiled out, so the accessor
/// would otherwise be flagged.
#[cfg_attr(not(feature = "mcp-writes"), allow(dead_code))]
pub(crate) fn dbpath() -> Option<String> {
    DBPATH.with(|c| c.borrow().clone())
}

// ─────────────────────────── small argument helpers ───────────────────────

/// Required string argument. Errors ("invalid params: missing 'X'") map to
/// `-32602` through `sanitize_error`.
fn pstr<'a>(args: &'a Value, key: &str) -> Result<&'a str, (String, i32)> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| (format!("invalid params: missing '{key}'"), EXIT_ERR))
}

/// Optional boolean argument (defaults to `false`).
fn pbool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Optional positive integer limit (default 20).
fn plimit(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n.clamp(1, 1024) as usize)
        .unwrap_or(default)
}

/// True when the frontmatter `mcp_hidden` flag is set on the page at
/// `abs_path`. A missing row is *not* hidden (the caller decides).
fn hidden_flag(db: &Db, abs_path: &str) -> Result<bool, (String, i32)> {
    let v: Option<Option<String>> = db
        .conn
        .query_row(
            format!("SELECT {HIDDEN_COL} FROM pages WHERE abs_path = ?1"),
            params![abs_path],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| (format!("hidden flag: {e}"), EXIT_ERR))?;
    Ok(matches!(v.flatten().as_deref(), Some("1")))
}

/// Resolve a page target (abs path / `type:slug` / uuid token / fragment)
/// to its `abs_path`, then apply the hidden-page rule: a hidden page is
/// indistinguishable from a missing one (MCP.md invariant: the agent cannot
/// learn the page exists). Returns the `abs_path` on success.
///
/// `pub(super)` so `kb/mcp/write.rs` (the `mcp-writes` surface) can
/// pre-check the same semantics before re-exec'ing the CLI verb.
pub(super) fn resolve_and_check(
    db: &Db,
    target: &str,
) -> Result<String, (String, i32)> {
    let abs = resolve_target_path(db, target)?
        .ok_or_else(|| ("no such page".into(), EXIT_ERR))?;
    let hidden = hidden_flag(db, &abs)?;
    if hidden {
        return Err(("no such page".into(), EXIT_ERR));
    }
    Ok(abs)
}

/// The page's uuid (stored, else re-derived): the identity token an agent
/// pastes back. `None` when the page has no stable identity.
fn page_uuid(db: &Db, abs_path: &str) -> Result<Option<String>, (String, i32)> {
    let row: Option<(Option<String>, String)> = db
        .conn
        .query_row(
            "SELECT uuid, id FROM pages WHERE abs_path = ?1",
            params![abs_path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| (format!("uuid lookup: {e}"), EXIT_ERR))?;
    let Some((stored, id)) = row else {
        return Ok(None);
    };
    Ok(stored.filter(|u| !u.is_empty()).or(Some(kb_core::uuid::uuid_of(&id, abs_path))))
}

/// Emit a stable identity pair (`uuid` + `uuid_short` when shorter) into a
/// JSON object under the given key prefix semantics (top-level keys).
fn push_identity(o: &mut Map<String, Value>, db: &Db, abs_path: &str) {
    if let Some(u) = page_uuid(db, abs_path).unwrap_or_default() {
        o.insert("uuid".into(), json!(u));
        if let Ok(short) = uuid_short_for(db, &u, abs_path) {
            if !short.is_empty() && short != u {
                o.insert("uuid_short".into(), json!(short));
            } else if short.is_empty() {
                o.insert("uuid_short".into(), json!(u));
            }
        }
    }
}

/// The frontmatter frame text for `fm` / `fm.<key>` refs (re-implemented
/// over the CLI verb: `cmd_show_fm` prints; we need the bytes).
fn fm_show_text(data: &[u8], r: &str) -> Result<String, (String, i32)> {
    let fm = fm::find_fm(data)
        .ok_or_else(|| ("no such page: no frontmatter frame".into(), EXIT_ERR))?;
    let lines = lines_of_str(data);
    let frame: Vec<&str> = lines
        .iter()
        .skip(fm.open_line)
        .take(fm.close_line - fm.open_line + 1)
        .map(|s| s.as_str())
        .collect();
    let interior: Vec<&str> = if frame.len() >= 2 {
        frame[1..frame.len() - 1].to_vec()
    } else {
        Vec::new()
    };
    let key: Option<&str> = r.strip_prefix("fm.").map(|s| s.trim());
    let text: String = match key {
        None => frame.concat(),
        Some(k) => {
            let mut out = String::new();
            let mut i = 0usize;
            while i < interior.len() {
                let t = interior[i].trim_start_matches('\u{feff}').trim();
                let Some(colon) = t.find(':') else { i += 1; continue; };
                if t[..colon].trim() == k {
                    out.push_str(interior[i]);
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    let mut j = i + 1;
                    while j < interior.len() {
                        let nt = interior[j].trim_start_matches('\u{feff}').trim();
                        if nt.starts_with('-') || nt.is_empty() || nt.starts_with('#') {
                            out.push_str(interior[j]);
                            if !out.ends_with('\n') {
                                out.push('\n');
                            }
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    break;
                }
                i += 1;
            }
            out
        }
    };
    Ok(text)
}

// ─────────────────────────────── list tools ───────────────────────────────

/// `kb_search` — full-text search. `Query` is optional; an empty/omitted query
/// matches every non-hidden page. Hidden pages are carried with
/// `mcp_hidden: true` so the **sanitizer** drops them (invariant 1).
fn t_search(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let limit = plimit(args, "limit", 20);
    let (rows, _missing_tag) = search_rows(db, &query, limit)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut o = Map::new();
            o.insert("id".into(), json!(r.id));
            o.insert("type".into(), json!(r.pt));
            if let Some(t) = &r.title {
                if !t.is_empty() {
                    o.insert("title".into(), json!(t));
                }
            }
            push_identity(&mut o, db, &r.abs_path);
            o.insert("mcp_hidden".into(), json!(r.mcp_hidden));
            Value::Object(o)
        })
        .collect();
    Ok(Value::Array(items))
}

/// `kb_browse` — list pages, optionally scoped by a `path`-prefix filter.
/// The *filter* is a directory path (the operator knows it); the *returned*
/// rows expose no root/rel/abs columns — the sanitizer strips them anyway.
fn t_browse(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let filter = args.get("path").and_then(|v| v.as_str()).map(String::from);
    let rows = fetch_browse_pages(db, filter.as_deref())?;
    let items: Vec<Value> = rows
        .iter()
        .map(
            |(abs_path, id, pt, title, _root, _rel, _gate, _uuid, mcp_hidden)| {
                let mut o = Map::new();
                o.insert("id".into(), json!(id));
                o.insert("type".into(), json!(pt));
                if let Some(t) = title {
                    if !t.is_empty() {
                        o.insert("title".into(), json!(t));
                    }
                }
                push_identity(&mut o, db, abs_path);
                o.insert(
                    "mcp_hidden".into(),
                    json!(matches!(mcp_hidden.as_deref(), Some("1"))),
                );
                Value::Object(o)
            },
        )
        .collect();
    Ok(Value::Array(items))
}

/// `kb_tag` — pages carrying `tag` (aliases transparently expanded). No
/// alias / no candidate in the corpus is a clean empty array, not an error.
fn t_tag(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let tag = pstr(args, "tag")?.to_string();
    let aliases = read_all_aliases(db)?;
    let all_tags: Vec<String> = {
        let stmt = db
            .conn
            .prepare("SELECT DISTINCT tag FROM page_tags")
            .map_err(|e| (format!("tags: {e}"), EXIT_ERR))?;
        stmt.query_map((), |r| r.get::<_, String>(0))
            .map_err(|e| (format!("tags rows: {e}"), EXIT_ERR))?
            .collect::<Result<_, _>>()
            .map_err(|e| (format!("tags rows: {e}"), EXIT_ERR))?
    };
    let match_set: Vec<String> = kbtags::canonicalize(&tag, &all_tags, &aliases)
        .iter()
        .map(|s| s.to_string())
        .collect();
    if match_set.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let in_list: Vec<String> = match_set.iter().map(|t| sql_quote_lower(t)).collect();
    let sql = format!(
        "SELECT pages.abs_path, pages.id, pages.type, pages.title\n\
         FROM pages\n\
         JOIN page_tags pt ON pt.abs_path = pages.abs_path\n\
         WHERE lower(pt.tag) IN ({})\n\
         ORDER BY pages.abs_path",
        in_list.join(",")
    );
    let stmt = db.conn.prepare(&sql).map_err(|e| (format!("tag query: {e}"), EXIT_ERR))?;
    let rows = stmt
        .query_map((), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(|e| (format!("tag rows: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("tag rows: {e}"), EXIT_ERR))?;
    let items: Vec<Value> = rows
        .iter()
        .map(|(abs, id, pt, title)| {
            let mut o = Map::new();
            o.insert("id".into(), json!(id));
            o.insert("type".into(), json!(pt));
            if let Some(t) = title {
                if !t.is_empty() {
                    o.insert("title".into(), json!(t));
                }
            }
            push_identity(&mut o, db, abs);
            o.insert("mcp_hidden".into(), json!(hidden_flag(db, abs).unwrap_or(false)));
            Value::Object(o)
        })
        .collect();
    Ok(Value::Array(items))
}

/// `kb_roots` — the *count* of registered roots only (root paths are a path
/// leak — the agent cannot enumerate where the files live).
fn t_roots(db: &Db, _args: &Value) -> Result<Value, (String, i32)> {
    let n: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM roots", (), |r| r.get(0))
        .map_err(|e| (format!("roots count: {e}"), EXIT_ERR))?;
    Ok(json!({ "roots": n }))
}

/// `kb_stats` — aggregate counts only (numbers, never paths or sizes that
/// expose a directory structure).
fn t_stats(db: &Db, _args: &Value) -> Result<Value, (String, i32)> {
    let q = |sql: &str| -> Result<i64, (String, i32)> {
        db.conn
            .query_row(sql, (), |r| r.get::<_, i64>(0))
            .map_err(|e| (format!("stats: {e}"), EXIT_ERR))
    };
    let pages = q("SELECT COUNT(*) FROM pages")?;
    let tags = q("SELECT COUNT(DISTINCT tag) FROM page_tags")?;
    let links = q("SELECT COUNT(*) FROM links")?;
    let tag_aliases = q("SELECT COUNT(*) FROM tag_aliases")?;
    Ok(json!({
        "pages": pages,
        "tags": tags,
        "links": links,
        "tag_aliases": tag_aliases
    }))
}

/// `kb_tag_alias_list` — the full alias map (alias -> canonical tag).
fn t_tag_alias_list(db: &Db, _args: &Value) -> Result<Value, (String, i32)> {
    let aliases = read_all_aliases(db)?;
    let items: Vec<Value> = aliases
        .iter()
        .map(|(alias, canonical)| json!({ "alias": alias, "canonical": canonical }))
        .collect();
    Ok(Value::Array(items))
}

// ───────────────────────────── targeted tools ─────────────────────────────

/// `kb_show` — the whole page, or a section / frontmatter frame when `ref`
/// is provided. A hidden page is indistinguishable from a missing one.
fn t_show(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let target = pstr(args, "file")?;
    let abs = resolve_and_check(db, target)?;
    let data = read_file(Path::new(&abs))?;
    let doc = Doc::new(data);
    let etag = etag_of(&doc.data);
    let uuid = page_uuid(db, &abs)?;
    let r = args.get("ref").and_then(|v| v.as_str());
    match r {
        None => {
            let text = String::from_utf8_lossy(&doc.data).into_owned();
            let mut o = Map::new();
            o.insert("etag".into(), json!(etag));
            o.insert("text".into(), json!(text));
            if let Some(u) = uuid {
                o.insert("uuid".into(), json!(u));
            }
            Ok(Value::Object(o))
        }
        Some(refc) => {
            if is_fm_ref(refc) {
                let text = fm_show_text(&doc.data, refc)?;
                let mut o = Map::new();
                o.insert("etag".into(), json!(etag_of(text.as_bytes())));
                o.insert("text".into(), json!(text));
                o.insert("ref".into(), json!(refc));
                if let Some(u) = uuid {
                    o.insert("uuid".into(), json!(u));
                }
                return Ok(Value::Object(o));
            }
            let full = pbool(args, "full");
            let bytes: Vec<u8> = if full {
                doc.full_bytes(refc).map_err(|e| (e.to_string(), EXIT_ERR))?
            } else {
                doc.own_bytes(refc).map_err(|e| (e.to_string(), EXIT_ERR))?
            }
            .to_vec();
            let sp = if full { "full" } else { "body" };
            let (title, level, start_line, end_line): (String, u8, usize, usize) =
                if refc == Doc::PAGE_ROW_REF {
                    let rgn = doc.page_span();
                    (String::from("*"), 0, 0, rgn.end)
                } else {
                    let idx = doc.resolve(refc).map_err(|e| (e.to_string(), EXIT_ERR))?;
                    let s = &doc.sections[idx];
                    let end_line = if full { s.end_line } else { s.own_end_line };
                    (s.title.clone(), s.level, s.start_line, end_line)
                };
            let mut o = Map::new();
            o.insert("etag".into(), json!(etag_of(&bytes)));
            o.insert("text".into(), json!(String::from_utf8_lossy(&bytes).into_owned()));
            o.insert("ref".into(), json!(refc));
            o.insert("title".into(), json!(title));
            o.insert("level".into(), json!(level));
            o.insert("span".into(), json!(sp));
            o.insert("line".into(), json!(start_line + 1));
            o.insert("lines".into(), json!(end_line.saturating_sub(start_line)));
            if let Some(u) = uuid {
                o.insert("uuid".into(), json!(u));
            }
            Ok(Value::Object(o))
        }
    }
}

/// `kb_toc` — the section outline. A hidden page is indistinguishable from
/// a missing one.
fn t_toc(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let target = pstr(args, "file")?;
    let abs = resolve_and_check(db, target)?;
    let data = read_file(Path::new(&abs))?;
    let doc = Doc::new(data);
    let items: Vec<Value> = doc
        .sections
        .iter()
        .map(|s| {
            json!({
                "ref": s.ref_,
                "level": s.level,
                "title": s.title,
                "line": s.start_line + 1,
                "lines": s.end_line.saturating_sub(s.start_line),
                "chars": s.end.saturating_sub(s.start)
            })
        })
        .collect();
    Ok(Value::Array(items))
}

/// `kb_info` — page metadata (id, uuid, type, tags, link counts, wc). A
/// hidden page is indistinguishable from a missing one.
fn t_info(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let target = pstr(args, "file")?;
    let abs = resolve_and_check(db, target)?;
    let info_sql = format!(
        "SELECT id, type, title, description, mtime_ns, {gate}, uuid\n\
         FROM pages WHERE abs_path = ?1",
        gate = GATE_COL
    );
    #[allow(clippy::type_complexity)]
    let rec: (String, String, Option<String>, Option<String>,
              i64, Option<String>, Option<String>) = db
        .conn
        .query_row(&info_sql, params![abs], |r| {
            Ok((
                r.get(0)?, r.get(1)?, r.get(2)?,
                r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?,
            ))
        })
        .map_err(|e| (format!("info: {e}"), EXIT_ERR))?;
    let (id, pt, title, description, mtime_ns, _gate, uuid_opt) = rec;
    let uuid = uuid_opt.filter(|u| !u.is_empty()).map(|u| u.to_string());
    let uuid_short = uuid.as_ref().and_then(|u| uuid_short_for(db, u, &abs).ok());
    let tags: Vec<String> = page_tags_str(db, &abs)
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .collect();
    let fwd: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM links WHERE from_path = ?1", params![abs], |r| r.get(0))
        .map_err(|e| (format!("count out: {e}"), EXIT_ERR))?;
    let back: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM links WHERE to_path = ?1", params![abs], |r| r.get(0))
        .map_err(|e| (format!("count in: {e}"), EXIT_ERR))?;
    let body: String = match std::fs::read_to_string(&abs) {
        Ok(s) => s,
        Err(_) => db
            .conn
            .query_row(
                "SELECT COALESCE(body, '') FROM pages WHERE abs_path = ?1",
                params![abs],
                |r| r.get::<_, String>(0),
            )
            .unwrap_or_default(),
    };
    let (wc_lines, wc_chars, wc_words) = text_wc(&body);
    let secs_f = (mtime_ns as f64) / 1e9;
    let mut o = Map::new();
    o.insert("id".into(), json!(id));
    if let Some(u) = &uuid {
        o.insert("uuid".into(), json!(u));
    }
    if let Some(short) = &uuid_short {
        o.insert("uuid_short".into(), json!(short));
    }
    o.insert("type".into(), json!(pt));
    if let Some(t) = &title {
        if !t.is_empty() {
            o.insert("title".into(), json!(t));
        }
    }
    if let Some(d) = &description {
        if !d.is_empty() {
            o.insert("description".into(), json!(d));
        }
    }
    o.insert("tags".into(), Value::Array(tags.into_iter().map(Value::String).collect()));
    o.insert("links_out".into(), json!(fwd));
    o.insert("links_in".into(), json!(back));
    o.insert("lines".into(), json!(wc_lines));
    o.insert("chars".into(), json!(wc_chars));
    o.insert("words".into(), json!(wc_words));
    o.insert("mtime".into(), json!(secs_f));
    Ok(Value::Object(o))
}

/// `kb_links` — the page's per-occurrence link list (default) or the dedup'd
/// edge set + backlinks (`dedup: true`). `resolves_to` / `candidates` carry
/// the *uuid* of the resolved page, never its path.
fn t_links(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let target = pstr(args, "file")?;
    let abs = resolve_and_check(db, target)?;
    let dedup = pbool(args, "dedup");
    let corpus = load_corpus(db)?;
    let abs2uuid: std::collections::HashMap<String, String> = corpus
        .iter()
        .map(|p| {
            let u = p
                .uuid
                .clone()
                .unwrap_or_else(|| kb_core::uuid::uuid_of(&format!("{}:{}", p.typ, p.slug), &p.abs_path));
            (p.abs_path.clone(), u)
        })
        .collect();
    let to_uuid = |a: &str| -> Option<Value> {
        abs2uuid.get(a).map(|u| Value::String(u.clone()))
    };
    if dedup {
        let fwd: Vec<(String, String)> = {
            let stmt = db
                .conn
                .prepare("SELECT to_path, style FROM links WHERE from_path = ?1 ORDER BY to_path, style")
                .map_err(|e| (format!("fwd: {e}"), EXIT_ERR))?;
            stmt.query_map(params![abs], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| (format!("fwd rows: {e}"), EXIT_ERR))?
                .collect::<Result<_, _>>()
                .map_err(|e| (format!("fwd rows: {e}"), EXIT_ERR))?
        };
        let edges_by_from: std::collections::BTreeMap<String, Vec<String>> = {
            let stmt = db
                .conn
                .prepare("SELECT from_path, to_path FROM links")
                .map_err(|e| (format!("back: {e}"), EXIT_ERR))?;
            let rows = stmt
                .query_map((), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| (format!("back rows: {e}"), EXIT_ERR))?;
            let mut m: std::collections::BTreeMap<String, Vec<String>> = Default::default();
            for row in rows {
                let (f, t) = row.map_err(|e| (format!("back rows: {e}"), EXIT_ERR))?;
                m.entry(f).or_default().push(t);
            }
            m
        };
        let mut back_links: Vec<String> = Vec::new();
        for from in edges_by_from.keys() {
            if from == &abs {
                continue;
            }
            let hit = edges_by_from
                .get(from)
                .map(|t| t.iter().any(|tt| resolve_target(&corpus, tt).as_deref() == Some(abs.as_str())))
                .unwrap_or(false);
            if hit {
                back_links.push(from.clone());
            }
        }
        back_links.sort();
        back_links.dedup();
        let fwd_items: Vec<Value> = fwd
            .iter()
            .map(|(to_path, style)| {
                let resolved = resolve_target(&corpus, to_path);
                let mut o = Map::new();
                o.insert("target".into(), json!(to_path));
                o.insert("style".into(), json!(style));
                if let Some(r) = &resolved {
                    o.insert("resolves_to".into(), to_uuid(r).unwrap_or(json!(r)));
                }
                Value::Object(o)
            })
            .collect();
        let back_items: Vec<Value> = back_links
            .iter()
            .map(|b| json!({ "uuid": to_uuid(b).unwrap_or(Value::Null) }))
            .collect();
        let mut o = Map::new();
        if let Some(u) = page_uuid(db, &abs)? {
            o.insert("uuid".into(), json!(u));
        }
        o.insert("forward".into(), Value::Array(fwd_items));
        o.insert("backlinks".into(), Value::Array(back_items));
        return Ok(Value::Object(o));
    }
    let body: String = db
        .conn
        .query_row(
            "SELECT COALESCE(body, '') FROM pages WHERE abs_path = ?1",
            params![abs],
            |r| r.get(0),
        )
        .map_err(|e| (format!("body: {e}"), EXIT_ERR))?;
    let links: Vec<AnnotatedLink> = annotated_links(&body, &corpus);
    let items: Vec<Value> = links
        .iter()
        .map(|l| {
            let mut o = Map::new();
            o.insert("line".into(), json!(l.line + 1));
            o.insert("col".into(), json!(l.col));
            o.insert("end".into(), json!(l.end));
            o.insert("context".into(), json!(l.context));
            o.insert("target".into(), json!(l.target));
            o.insert("style".into(), json!(l.kind.style_str(&l.target)));
            if let Some(label) = &l.label {
                o.insert("label".into(), json!(label));
            }
            if let Some(section) = &l.section {
                o.insert("section".into(), json!(section));
            }
            if let Some(r) = l.resolves_to.as_deref() {
                o.insert("resolves_to".into(), to_uuid(r).unwrap_or(json!(r)));
            }
            if !l.candidates.is_empty() {
                let c: Vec<Value> = l.candidates.iter().map(|c| to_uuid(c).unwrap_or(json!(c))).collect();
                o.insert("candidates".into(), Value::Array(c));
            }
            Value::Object(o)
        })
        .collect();
    Ok(Value::Array(items))
}

/// `kb_resolve` — disambiguate a page reference; return the candidate list
/// (uuid + id) an agent can paste back verbatim.
fn t_resolve(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let query = pstr(args, "query")?.to_string();
    let pages = load_corpus(db)?;
    let idx: std::collections::HashMap<&str, &resolver::Page> =
        pages.iter().map(|p| (p.abs_path.as_str(), p)).collect();
    let (resolved, outcome): (Vec<String>, &str) =
        if kb_core::uuid::is_uuid_token(&query) {
            let like = format!("{query}%");
            let rows: Vec<String> = {
                let stmt = db
                    .conn
                    .prepare("SELECT abs_path FROM pages WHERE uuid LIKE ?1 LIMIT 2")
                    .map_err(|e| (format!("uuid probe: {e}"), EXIT_ERR))?;
                stmt.query_map(params![like], |r| r.get::<_, String>(0))
                    .map_err(|e| (format!("uuid rows: {e}"), EXIT_ERR))?
                    .collect::<Result<_, _>>()
                    .map_err(|e| (format!("uuid rows: {e}"), EXIT_ERR))?
            };
            match rows.len() {
                1 => (rows, "unique"),
                0 => (Vec::new(), "empty"),
                n => {
                    return Err((
                        format!("uuid prefix is ambiguous ({n} pages) — extend"),
                        EXIT_ERR,
                    ));
                }
            }
        } else {
            let out = resolver::resolve(&query, &pages);
            let list: Vec<String> = match &out {
                resolver::Resolution::Unique(p) => vec![p.clone()],
                resolver::Resolution::Candidates(v) => v.clone(),
                resolver::Resolution::Empty => Vec::new(),
            };
            let kind = match &out {
                resolver::Resolution::Unique(_) => "unique",
                resolver::Resolution::Candidates(v) if v.len() > 1 => "candidates",
                resolver::Resolution::Candidates(_) => "unique",
                resolver::Resolution::Empty => "empty",
            };
            (list, kind)
        };
    let items: Vec<Value> = resolved
        .iter()
        .filter_map(|abs| {
            let p = idx.get(abs.as_str())?;
            let id = format!("{}:{}", p.typ, p.slug);
            let u = p
                .uuid
                .clone()
                .unwrap_or_else(|| kb_core::uuid::uuid_of(&id, &p.abs_path));
            let mut o = Map::new();
            o.insert("id".into(), json!(id));
            o.insert("uuid".into(), json!(u));
            if let Ok(short) = uuid_short_for(db, &u, &p.abs_path) {
                if !short.is_empty() && short != u {
                    o.insert("uuid_short".into(), json!(short));
                }
            }
            Some(Value::Object(o))
        })
        .collect();
    let mut o = Map::new();
    o.insert("query".into(), json!(query));
    o.insert("outcome".into(), json!(outcome));
    o.insert("candidates".into(), Value::Array(items));
    Ok(Value::Object(o))
}

// ─────────────────────────────── dispatch ─────────────────────────────────

/// One tool entry for the `tools/list` surface.
fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

/// The full `tools/list` surface for the read server (MCP.md tool table).
fn tools_list() -> Value {
    // `mut` only needed when `mcp-writes` pulls in the `arr.extend(...)`
    // below — in the read-only build the vec is never reassigned, so the
    // `unused_mut` lint fires. Gate the `mut` on the same feature.
    #[cfg_attr(not(feature = "mcp-writes"), allow(unused_mut))]
    let mut arr = vec![
        tool("kb_search",
            "Full-text search across all indexed knowledge-base pages — use this instead of find/grep over the filesystem. Pass `query` (optional; FTS syntax: type:/tag: filters, ~a ~b OR, -c NOT, \"phrases\") and `limit` (default 20).",
            json!({"type":"object",
                   "properties":{"query":{"type":"string","description":"Search query"},
                                 "limit":{"type":"integer","description":"Max results (default 20)"}}
                  })),
        tool("kb_browse",
            "List the pages in the knowledge base (all roots, or those under a given directory) with title, tags, and link count.",
            json!({"type":"object",
                   "properties":{"path":{"type":"string","description":"Restrict to a directory prefix (optional)"}}
                  })),
        tool("kb_show",
            "Read the knowledge-base page (whole file, or one section via `ref`) and its `etag` for safe concurrent editing. Use this when reading a note — not the file Read tool.",
            json!({"type":"object",
                   "required":["file"],
                   "properties":{"file":{"type":"string","description":"Page path or uuid token"},
                                 "ref":{"type":"string","description":"Section ref (dotted number or title path); omit for the whole page"},
                                 "full":{"type":"boolean","description":"Full span including subsections (default: own body only)"}}
                  })),
        tool("kb_toc",
            "Outline of a knowledge-base page: the section tree (ref / level / title / line). Call this BEFORE show when you only need the structure.",
            json!({"type":"object",
                   "required":["file"],
                   "properties":{"file":{"type":"string","description":"Page path or uuid token"}}
                  })),
        tool("kb_info",
            "Show a page's DB record: id, type, title, description, tags, search_terms, links (in/out), mtime.",
            json!({"type":"object",
                   "required":["file"],
                   "properties":{"file":{"type":"string","description":"Page path or uuid token"}}
                  })),
        tool("kb_links",
            "Links for one page. Default: the per-occurrence annotated list. Pass `dedup=true` for the dedup'd (target, style) edge set + backlinks.",
            json!({"type":"object",
                   "required":["file"],
                   "properties":{"file":{"type":"string","description":"Page path or uuid token"},
                                 "dedup":{"type":"boolean","description":"Edge-set view (default: per-occurrence list)"}}
                  })),
        tool("kb_tag",
            "Find every knowledge-base page carrying a given tag (aliases are expanded).",
            json!({"type":"object",
                   "required":["tag"],
                   "properties":{"tag":{"type":"string","description":"The tag to find (alias-expanded)"}}
                  })),
        tool("kb_tag_alias_list",
            "List all tag aliases (alias -> canonical tag).",
            json!({"type":"object","properties":{}})),
        tool("kb_resolve",
            "Resolve an ambiguous page reference (slug, title, or partial name) to the exact list of candidate page ids/paths.",
            json!({"type":"object",
                   "required":["query"],
                   "properties":{"query":{"type":"string","description":"The page reference to disambiguate"}}
                  })),
        tool("kb_roots",
            "List registered roots (with their stored walk options).",
            json!({"type":"object","properties":{}})),
        tool("kb_stats",
            "Health check for the knowledge base: how many pages per root, total tags/links, DB size, last index time.",
            json!({"type":"object","properties":{}})),
    ];
    #[cfg(feature = "mcp-writes")]
    arr.extend([
        tool("kb_create",
            "Scaffold a new page (frontmatter + H1) and index it. `name` = page slug; `type` = wiki|note|label; `path` = target directory; `dry_run` = show the scaffold diff without writing.",
            json!({"type":"object","required":["name"],
                    "properties":{"name":{"type":"string","description":"Page slug"},
                                  "type":{"type":"string","description":"wiki | note | label (default wiki)"},
                                  "path":{"type":"string","description":"Target directory"},
                                  "dry_run":{"type":"boolean","description":"Show diff, do not write"}}})),
        tool("kb_section_set",
            "Replace a section's full span (heading + body + subsections), or write a frontmatter scalar / append to a list. `keep_heading` = keep the existing heading (text is body-only). Pass `if_unchanged` = etag from `kb_show` to abort if the section changed.",
            json!({"type":"object","required":["file","ref","text"],
                    "properties":{"file":{"type":"string","description":"Page path or uuid token"},
                                  "ref":{"type":"string","description":"Section ref (dotted number, 'A > B', title, or fm.<key>)"},
                                  "text":{"type":"string","description":"The complete new text for the section"},
                                  "keep_heading":{"type":"boolean","description":"Keep existing heading (text is body-only)"},
                                  "if_unchanged":{"type":"string","description":"Etag guard"},
                                  "dry_run":{"type":"boolean","description":"Compute diff, do not write"}}})),
        tool("kb_section_replace",
            "Replace a section's body, keeping its heading. Pass `if_unchanged` = etag from `kb_show` to abort if the section changed since read.",
            json!({"type":"object","required":["file","ref","text"],
                    "properties":{"file":{"type":"string","description":"Page path or uuid token"},
                                  "ref":{"type":"string","description":"Section ref"},
                                  "text":{"type":"string","description":"The new body text"},
                                  "if_unchanged":{"type":"string","description":"Etag guard"},
                                  "dry_run":{"type":"boolean","description":"Compute diff, do not write"}}})),
        tool("kb_section_append",
            "Append text at the end of a section body, or append a list item to a `fm.<list>` ref.",
            json!({"type":"object","required":["file","ref","text"],
                    "properties":{"file":{"type":"string","description":"Page path or uuid token"},
                                  "ref":{"type":"string","description":"Section ref"},
                                  "text":{"type":"string","description":"Text to append"},
                                  "if_unchanged":{"type":"string","description":"Etag guard"},
                                  "dry_run":{"type":"boolean","description":"Compute diff, do not write"}}})),
        tool("kb_section_insert",
            "Add a brand-new section (heading + body) at a placement (`before` / `after` / `child` / `top` / `end`). `top` / `end` need no `ref`.",
            json!({"type":"object","required":["file","title","text"],
                    "properties":{"file":{"type":"string","description":"Page path or uuid token"},
                                  "title":{"type":"string","description":"New section's title"},
                                  "text":{"type":"string","description":"New section's body"},
                                  "ref":{"type":"string","description":"Target for before/after/child placement"},
                                  "placement":{"type":"string","description":"before | after | child | top | end"},
                                  "if_unchanged":{"type":"string","description":"Etag guard"},
                                  "dry_run":{"type":"boolean","description":"Compute diff, do not write"}}})),
        tool("kb_section_rm",
            "Delete a section (heading + body + subsections), a frontmatter field (`rm fm.<key>`), or one list item from `fm.<list>` (pass `value`).",
            json!({"type":"object","required":["file","ref"],
                    "properties":{"file":{"type":"string","description":"Page path or uuid token"},
                                  "ref":{"type":"string","description":"Section ref or fm.<key>"},
                                  "value":{"type":"string","description":"For fm.<list>: the element to remove"},
                                  "if_unchanged":{"type":"string","description":"Etag guard"},
                                  "dry_run":{"type":"boolean","description":"Compute diff, do not write"}}})),
        tool("kb_replace",
            "Substring find/replace across a file, or scoped to one section. `count` bounds the number of substitutions (default: all).",
            json!({"type":"object","required":["file","find","new"],
                    "properties":{"file":{"type":"string","description":"Page path or uuid token"},
                                  "find":{"type":"string","description":"Substring to find"},
                                  "new":{"type":"string","description":"Substring to replace it with"},
                                  "scope":{"type":"string","description":"Restrict to a section ref"},
                                  "count":{"type":"integer","description":"At most N occurrences (default: all)"},
                                  "if_unchanged":{"type":"string","description":"Etag guard"},
                                  "dry_run":{"type":"boolean","description":"Compute diff, do not write"}}})),
        tool("kb_patch",
            "Apply an *external* unified diff (from `diff -u` / `git diff`) to a file. The patch is given inline in `patch_text`.",
            json!({"type":"object","required":["file","patch_text"],
                    "properties":{"file":{"type":"string","description":"Target file"},
                                  "patch_text":{"type":"string","description":"The unified diff to apply"},
                                  "if_unchanged":{"type":"string","description":"Etag guard"},
                                  "dry_run":{"type":"boolean","description":"Compute diff, do not write"}}})),
        tool("kb_tag_alias_add",
            "Add a tag alias: `alias` -> `canonical tag`.",
            json!({"type":"object","required":["alias","canonical"],
                    "properties":{"alias":{"type":"string","description":"The alias to add"},
                                  "canonical":{"type":"string","description":"The canonical tag"}}})),
        tool("kb_tag_alias_rm",
            "Remove a tag alias.",
            json!({"type":"object","required":["alias"],
                    "properties":{"alias":{"type":"string","description":"The alias to remove"}}})),
    ]);
    json!({ "tools": arr })
}

/// Serialize a JSON-RPC response object **through the sanitizer**.
fn ok(id: &Value, result: Value) -> String {
    super::sanitizer::sanitize_value(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    }))
    .to_string()
}

fn err(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}).to_string()
}

fn err_msg(id: &Value, msg: &str) -> String {
    let (code, message) = super::sanitizer::sanitize_error(msg);
    err(id, code, &message)
}

/// Dispatch one JSON-RPC frame against `db`. Returns the serialized
/// response (to be written to stdout, one `\n`-terminated line per frame).
pub(crate) fn dispatch(frame: &str, db: &Db) -> String {
    let v: Value = match serde_json::from_str(frame) {
        Ok(v) => v,
        Err(_) => return err(&Value::Null, -32700, "parse error"),
    };
    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => ok(&id, json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "kb",
                "version": env!("CARGO_PKG_VERSION"),
                "description": "kb knowledge-base MCP server (kb-hardened, stdio JSON-RPC).",
                "instructions": "kb is the user's Markdown knowledge base. \
                                 Pages are addressed by uuid (16 hex); paths \
                                 are never exposed. See MCP.md for the design."
            }
        })),
        "tools/list" => ok(&id, tools_list()),
        "tools/call" => {
            let name = v.get("params")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let args = v.get("params")
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or(Value::Object(Map::new()));
            let handler: Handler = match name {
                "kb_search" => t_search,
                "kb_browse" => t_browse,
                "kb_show" => t_show,
                "kb_toc" => t_toc,
                "kb_info" => t_info,
                "kb_links" => t_links,
                "kb_tag" => t_tag,
                "kb_tag_alias_list" => t_tag_alias_list,
                "kb_resolve" => t_resolve,
                "kb_roots" => t_roots,
                "kb_stats" => t_stats,
                #[cfg(feature = "mcp-writes")]
                "kb_create" => super::write::t_create,
                #[cfg(feature = "mcp-writes")]
                "kb_section_set" => super::write::t_section_set,
                #[cfg(feature = "mcp-writes")]
                "kb_section_replace" => super::write::t_section_replace,
                #[cfg(feature = "mcp-writes")]
                "kb_section_append" => super::write::t_section_append,
                #[cfg(feature = "mcp-writes")]
                "kb_section_insert" => super::write::t_section_insert,
                #[cfg(feature = "mcp-writes")]
                "kb_section_rm" => super::write::t_section_rm,
                #[cfg(feature = "mcp-writes")]
                "kb_replace" => super::write::t_replace,
                #[cfg(feature = "mcp-writes")]
                "kb_patch" => super::write::t_patch,
                #[cfg(feature = "mcp-writes")]
                "kb_tag_alias_add" => super::write::t_tag_alias_add,
                #[cfg(feature = "mcp-writes")]
                "kb_tag_alias_rm" => super::write::t_tag_alias_rm,
                _ => {
                    let m = if name.is_empty() {
                        "unknown tool".to_string()
                    } else {
                        format!("unknown tool: {name}")
                    };
                    return err_msg(&id, &m);
                }
            };
            match handler(db, &args) {
                Ok(result) => ok(&id, result),
                Err((msg, _)) => err_msg(&id, &msg),
            }
        }
        _ => err(&id, -32601, "unknown method"),
    }
}

/// The stdio JSON-RPC 2.0 loop. Reads one frame at a time from stdin
/// (newline-terminated), dispatches, and writes exactly one response to
/// stdout. Logs go to stderr only (MCP.md: the stdout channel must carry
/// only JSON-RPC frames).
pub(crate) fn serve(dbpath: Option<&Path>) -> i32 {
    let db = match open_db(dbpath) {
        Ok(db) => db,
        Err((msg, code)) => {
            eprintln!("kb mcp: {msg}");
            return code.max(EXIT_ERR);
        }
    };
    // Resolve the DB path the verb subprocess must reuse (`Db::path()` is
    // None for a normal `open()`), then hand it off thread-locally so the
    // write handlers can route their verbs to the same file. Explicit flag
    // wins; otherwise fall back to the default data-dir path.
    let effective = dbpath.map(Path::to_path_buf).or_else(Db::default_db_path);
    set_dbpath(effective.as_deref());
    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut line: Vec<u8> = Vec::new();
    loop {
        line.clear();
        let n = loop {
            let mut b = [0u8; 1];
            match reader.read(&mut b) {
                Ok(0) => break 0,
                Ok(_) => {
                    if b[0] == b'\n' {
                        break line.len();
                    }
                    line.push(b[0]);
                }
                Err(e) => {
                    eprintln!("kb mcp: read stdin: {e}");
                    return EXIT_ERR;
                }
            }
        };
        if n == 0 {
            break;
        }
        let frame = String::from_utf8_lossy(&line);
        let resp = dispatch(&frame, &db);
        let mut out = std::io::BufWriter::new(std::io::stdout());
        let _ = out.write_all(resp.as_bytes());
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }
    EXIT_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    static CTR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    struct T {
        root: PathBuf,
        db: PathBuf,
    }

    impl T {
        fn new(tag: &str) -> Self {
            let n = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let pid = std::process::id();
            let base = std::env::temp_dir().join(format!("kbmcp_{pid}_{n}_{tag}"));
            let _ = std::fs::remove_dir_all(&base);
            let root = base.join("root");
            std::fs::create_dir_all(&root).unwrap();
            let db = base.join("kb.db");
            T { root, db }
        }

        fn page(&self, name: &str, body: &str) {
            std::fs::write(self.root.join(name), body).unwrap();
        }

        fn indexed(&self, _tag: &str) -> (Db, String) {
            let db = open_db(Some(&self.db)).unwrap();
            let canonical = std::fs::canonicalize(&self.root).unwrap();
            db.add_root(&canonical.to_string_lossy(), None, false).unwrap();
            kb_refresh::refresh(&db).unwrap();
            (db, canonical.to_string_lossy().to_string())
        }
    }

    impl Drop for T {
        fn drop(&mut self) {
            if let Some(p) = self.root.parent() {
                let _ = std::fs::remove_dir_all(p);
            }
        }
    }

    fn call(db: &Db, name: &str, args: Value) -> String {
        let frame = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{name}","arguments":{args}}}}}"#
        );
        dispatch(&frame, db)
    }

    #[test]
    fn dispatch_initialize_is_shape_correct() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        let resp = dispatch("{}", &db);
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v.get("jsonrpc").and_then(|x| x.as_str()), Some("2.0"));
    }

    #[test]
    fn unknown_tool_is_error() {
        let (db, _) = {
            let t = T::new("t1");
            t.page("a.md", "---\ntype: note\ntitle: A\n---\nx\n");
            t.indexed("a")
        };
        let resp = call(&db, "kb_nope", json!({}));
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v.get("error").is_some());
        assert_eq!(v["error"]["code"], -32602);
    }

    #[test]
    fn tools_list_returns_all_read_tools() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        let resp = dispatch(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, &db);
        let v: Value = serde_json::from_str(&resp).unwrap();
        let tools = v["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for want in ["kb_search", "kb_browse", "kb_show", "kb_toc", "kb_info",
                     "kb_links", "kb_tag", "kb_tag_alias_list", "kb_resolve",
                     "kb_roots", "kb_stats"] {
            assert!(names.contains(&want), "missing {want} in {names:?}");
        }
    }

    #[test]
    fn roots_returns_count_not_paths() {
        let t = T::new("r");
        t.page("a.md", "---\ntype: note\ntitle: A\n---\nx\n");
        let (db, _) = t.indexed("a");
        let resp = call(&db, "kb_roots", json!({}));
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["roots"], 1);
        let s = v["result"].to_string();
        assert!(!s.contains("/"), "must not leak a path: {s}");
        assert!(!s.contains("kbmcp"), "must not leak a temp-path token: {s}");
    }

    #[test]
    fn stats_returns_numbers_only() {
        let t = T::new("s");
        t.page("a.md", "---\ntype: note\ntitle: A\ntags:\n  - foo\n---\nsee [[b]]\n");
        t.page("b.md", "---\ntype: note\ntitle: B\n---\nok\n");
        let (db, _) = t.indexed("a");
        let resp = call(&db, "kb_stats", json!({}));
        let v: Value = serde_json::from_str(&resp).unwrap();
        for k in ["pages", "tags", "links", "tag_aliases"] {
            assert!(v["result"][k].is_number(), "{k} should be a number, got {:?}", v["result"][k]);
        }
        assert_eq!(v["result"]["pages"], 2);
        assert_eq!(v["result"]["tags"], 1);
    }

    #[test]
    fn search_returns_pages() {
        let t = T::new("search");
        t.page("a.md", "---\ntype: note\ntitle: Apple pie\n---\napple body text\n");
        let (db, _) = t.indexed("a");
        let resp = call(&db, "kb_search", json!({"query": "apple"}));
        let v: Value = serde_json::from_str(&resp).unwrap();
        let arr = v["result"].as_array().expect("search result array");
        assert!(!arr.is_empty(), "expected at least one search hit: {v}");
        assert!(arr[0].get("id").is_some());
        // hidden flag stripped by sanitizer (key must not appear)
        assert!(arr[0].get("mcp_hidden").is_none());
    }

    #[test]
    fn search_drops_hidden_pages() {
        let t = T::new("hidden");
        t.page("a.md", "---\ntype: note\ntitle: Apple pie\n---\napple body\n");
        t.page("h.md", "---\ntype: note\ntitle: Secret apple\nmcp_hidden: true\n---\napple body\n");
        let (db, _abs) = t.indexed("a");
        let resp = call(&db, "kb_search", json!({"query": "apple"}));
        let v: Value = serde_json::from_str(&resp).unwrap();
        let arr = v["result"].as_array().unwrap();
        // hidden pages (mcp_hidden: true) are dropped by the sanitizer
        assert_eq!(arr.len(), 1, "hidden page must be dropped; got {arr:?}");
    }

    #[test]
    fn show_hidden_indistinguishable_from_missing() {
        let t = T::new("showhid");
        t.page("h.md", "---\ntype: note\ntitle: H\nmcp_hidden: true\n---\nsecret\n");
        let (db, _) = t.indexed("h");
        let abs_hidden: String = db.conn.query_row(
            "SELECT abs_path FROM pages", (), |r| r.get(0)).unwrap();
        let hidden = call(&db, "kb_show", json!({"file": abs_hidden.clone()}));
        let missing = call(&db, "kb_show", json!({"file": "wiki:totally-missing"}));
        let vh: Value = serde_json::from_str(&hidden).unwrap();
        let vm: Value = serde_json::from_str(&missing).unwrap();
        assert!(vh.get("error").is_some(), "hidden: {vh}");
        assert!(vm.get("error").is_some(), "missing: {vm}");
        assert_eq!(vh["error"]["code"], vm["error"]["code"]);
        assert_eq!(vh["error"]["message"], vm["error"]["message"]);
    }

    #[test]
    fn toc_returns_section_tree() {
        let t = T::new("toc");
        t.page("doc.md", "---\ntype: note\ntitle: Doc\n---\n# A\nalpha\n# B\n  ## B1\nbeta\n");
        let (db, _) = t.indexed("a");
        let abs: String = db.conn.query_row("SELECT abs_path FROM pages", (), |r| r.get(0)).unwrap();
        let resp = call(&db, "kb_toc", json!({"file": abs}));
        eprintln!("[dbg toc raw={}]", resp);
        let v: Value = serde_json::from_str(&resp).unwrap();
        let arr = v["result"].as_array().expect("toc: must be an array");
        assert!(arr.iter().any(|s| s["title"] == "A"), "should contain section A: {arr:?}");
    }

    #[test]
    fn info_returns_metadata() {
        let t = T::new("info");
        t.page("n.md", "---\ntype: note\ntitle: Note\ndescription: a note\ntags:\n  - t1\n  - t2\n---\nbody text here\n");
        let (db, _) = t.indexed("n");
        let abs: String = db.conn.query_row("SELECT abs_path FROM pages", (), |r| r.get(0)).unwrap();
        let resp = call(&db, "kb_info", json!({"file": abs}));
        let v: Value = serde_json::from_str(&resp).unwrap();
        let r = &v["result"];
        assert!(r.get("id").is_some());
        assert!(r.get("type").is_some());
        assert_eq!(r["type"], "note");
        assert!(r.get("uuid").is_some(), "uuid should be present: {v}");
        // tags should be an array of the tags the page has
        let tags = r["tags"].as_array().expect("tags should be array");
        assert!(tags.iter().any(|t| t.as_str() == Some("t1")), "t1 missing: {tags:?}");
        assert!(tags.iter().any(|t| t.as_str() == Some("t2")), "t2 missing: {tags:?}");
        // path keys stripped by sanitize_value
        assert!(r.get("abs_path").is_none());
        assert!(r.get("rel_path").is_none());
    }

    #[test]
    fn tag_alias_list_returns_all_aliases() {
        let t = T::new("alias");
        t.page("a.md", "---\ntype: note\ntitle: A\n---\nx\n");
        let (db, _) = t.indexed("a");
        db.conn.execute(
            "INSERT INTO tag_aliases(alias, canonical) VALUES ('old', 'new')",
            (),
        ).unwrap();
        let resp = call(&db, "kb_tag_alias_list", json!({}));
        let v: Value = serde_json::from_str(&resp).unwrap();
        let arr = v["result"].as_array().unwrap();
        assert!(arr.iter().any(|a| a["alias"] == "old" && a["canonical"] == "new"), "alias missing: {arr:?}");
    }

    #[test]
    fn link_dedup_returns_edge_set() {
        let t = T::new("links");
        t.page("b.md", "---\ntype: note\ntitle: B\n---\ntarget\n");
        t.page("a.md", "---\ntype: note\ntitle: A\n---\nsee [[B]]\n");
        let (db, _) = t.indexed("a");
        let abs_a: String = db.conn.query_row(
            "SELECT abs_path FROM pages WHERE id LIKE '%a%'", (), |r| r.get(0)).unwrap();
        let resp = call(&db, "kb_links", json!({"file": abs_a, "dedup": true}));
        let v: Value = serde_json::from_str(&resp).unwrap();
        let r = &v["result"];
        assert!(r.get("forward").is_some(), "missing forward: {v}");
        let fwd = r["forward"].as_array().unwrap();
        assert_eq!(fwd.len(), 1, "should have exactly one forward link: {fwd:?}");
    }
}
