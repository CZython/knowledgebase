//! The `mcp-writes` surface — the seven MCP *write* tools (MCP.md write table).
//!
//! Each handler re-executes the *same* binary's CLI verb with `--json` and
//! captures its stdout (piped, so it cannot pollute the server's stdio
//! channel). The verb already implements the correct hardening — etag
//! guard, atomic write, on-disk hash check, auto-reindex
//! (`kb/cli/*.rs`) — so there is no duplicated write logic in this
//! module.
//!
//! Feature gating: every handler in this file is `#[cfg(feature =
//! "mcp-writes")]`. The feature is **on by default** in the hardened build
//! (`default = ["mcp-writes"]`); compiling it out (`--no-default-features`)
//! is the only way to get a read-only server — compile-time only, there is
//! no runtime flag.
//!
//! Error contract: a nonzero verb exit is mapped through
//! `sanitize_error` to a stable (code, message). The *shape* of the
//! failure (which ref was touched, which section was not found) is part
//! of the verb's diagnostics and never crosses the wire (MCP.md invariant 4).

use serde_json::{json, Value};

use crate::*;

// ── argument helpers (same shape as tools.rs) ────────────────────────

fn wstr<'a>(args: &'a Value, key: &str) -> Result<&'a str, (String, i32)> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| (format!("invalid params: missing '{key}'"), EXIT_ERR))
}

fn wbool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn woptstr<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

// ── subprocess plumbing ──────────────────────────────────────────────

/// Resolve the CLI executable to spawn as a verb subprocess, in order
/// of precedence:
///
/// 1. **`KB_MCP_VERB` env var** — highest priority; lets the test
///    harness (or a developer) point the handler at a specific binary.
/// 2. **`current_exe()`** — in production this is the running
///    `kb-hardened` MCP server, which is the same binary as the CLI verb
///    body. This is the primary path the MCP surface relies on.
/// 3. **`CARGO_TARGET_DIR/kb-hardened`** — in a `cargo test` build,
///    `current_exe()` is the libtest harness (not a CLI); falling back
///    to the pre-built production binary keeps unit tests honest.
///    `CARGO_TARGET_DIR` is always set by cargo when it runs `cargo test`.
fn verb_exe() -> String {
    if let Some(v) = std::env::var_os("KB_MCP_VERB") {
        return v.display().to_string();
    }
    #[cfg(not(test))]
    {
        if let Ok(p) = std::env::current_exe() {
            return p.display().to_string();
        }
    }
    if let Some(td) = std::env::var_os("CARGO_TARGET_DIR") {
        for prof in [std::path::Path::new("debug"), std::path::Path::new("release")] {
            let p = std::path::PathBuf::from(&*td).join(prof).join("kb-hardened");
            if p.exists() {
                return p.display().to_string();
            }
        }
    }
    match std::env::current_exe() {
        Ok(p) => p.display().to_string(),
        Err(_) => "kb".into(),
    }
}

/// Spawn the verb binary, routing it to the same DB the parent holds.
///
/// `argv` is the verb argv *after* the executable path — subcommand
/// tokens + flags + positional values in the order the CLI expects, and
/// it must carry the verb's `--json` so its stdout is exactly one JSON
/// document (the human diff is routed to stderr by the CLI's
/// `--json` mode).
///
/// DB routing: the parent's `serve()` opened the parent [`Db`] from the
/// CLI `--db` flag / `KB_DB` / default data dir. We pass that same path
/// down as a *global* `--db` argument when the parent's DB is file
/// backed (`Db::path()`), so the child reopens the exact same file.
/// For an in-memory parent there is no path to pass; the child falls
/// back to the default DB (that case is test-only; in production the
/// parent always has a file-backed DB).
///
/// Stdout is piped — whatever the verb emits is captured and parsed
/// here, never leaked onto the MCP server's stdio channel.
fn run_verb(db: &Db, argv: &[&str]) -> Result<Value, (String, i32)> {
    // Inject `--db <path>` *after* the subcommand token (argv[0]) so the
    // bare-string dispatch in `main.rs` routes on the subcommand first.
    // (Placing `--db <path>` before the subcommand lets its *value* count
    // as a bare dispatch token → `search`.)
    let mut full: Vec<String> = Vec::with_capacity(argv.len() + 2);
    let (head, tail) = match argv.split_first() {
        Some((h, t)) => (*h, t),
        None => return Err((String::from("empty verb argv"), EXIT_ERR)),
    };
    full.push(head.to_string());
    // Prefer the thread-local (set by `serve()` or by a test); fall back
    // to the `Db` path field (only populated for in-memory test opens).
    let dbpath_str: Option<String> =
        crate::mcp::tools::dbpath().or_else(|| db.path().map(|p| p.display().to_string()));
    if let Some(p) = dbpath_str {
        full.push(String::from("--db"));
        full.push(p);
    }
    full.extend(tail.iter().map(|s| s.to_string()));
    let out = std::process::Command::new(verb_exe())
        .args(&full)
        .output()
        .map_err(|e| (format!("spawn verb failed: {e}"), EXIT_ERR))?;
    if out.status.success() {
        match serde_json::from_slice::<Value>(&out.stdout) {
            Ok(v) => Ok(v),
            Err(e) => Err((format!("verb stdout not JSON: {e}"), EXIT_ERR)),
        }
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let last = stderr
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("unknown failure");
        let (code, msg) = crate::mcp::sanitizer::sanitize_error(last);
        Err((format!("[{code}] {msg}"), EXIT_ERR))
    }
}

/// Pre-check a page's visibility via the read-side resolver (hidden
/// pages are indistinguishable from missing ones; the agent cannot
/// learn the page exists — MCP.md invariant). Returns `abs_path` so
/// the CLI verb's `--file` path is canonical.
fn page_ok(db: &Db, file: &str) -> Result<String, (String, i32)> {
    crate::mcp::tools::resolve_and_check(db, file)
}

// ── the tool handlers ────────────────────────────────────────────────

/// `kb_create` — scaffold a new page (frontmatter + H1) and index it.
/// `name` = page slug; `type` = `wiki` / `note` / `label`; `path`
/// = target directory (optional, default: cwd); `dry_run` = show
/// the scaffold diff without writing.
pub fn t_create(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let name = wstr(args, "name")?;
    let typ = woptstr(args, "type").unwrap_or("wiki");
    let mut argv: Vec<String> = vec![
        String::from("create"), name.to_string(),
        String::from("--type"), typ.to_string(),
    ];
    if let Some(dir) = woptstr(args, "path") {
        argv.push(String::from("--path")); argv.push(dir.to_string());
    }
    if wbool(args, "dry_run") { argv.push(String::from("--dry-run")); }
    argv.push(String::from("--json"));
    let res = run_verb_owned(db, &argv)?;
    Ok(json!({ "created": !wbool(args, "dry_run"), "name": name, "type": typ, "report": res }))
}

fn run_verb_owned(db: &Db, argv: &[String]) -> Result<Value, (String, i32)> {
    let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    run_verb(db, &refs)
}

/// `kb_section_set` — replace a section's full span (heading + body +
/// subsections). `keep_heading = true` keeps the existing heading
/// (text is body-only). For `fm.<key>` refs, `text` is the new scalar
/// value.
pub fn t_section_set(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let file = page_ok(db, wstr(args, "file")?)?;
    let ref_ = wstr(args, "ref")?;
    let text = wstr(args, "text")?;
    let mut argv: Vec<String> = vec![
        String::from("section"), String::from("--file"), file.clone(),
        String::from("set"), ref_.to_string(),
        String::from("--text"), text.to_string(),
    ];
    if wbool(args, "keep_heading") { argv.push(String::from("--keep-heading")); }
    if let Some(e) = woptstr(args, "if_unchanged") {
        argv.push(String::from("--if-unchanged")); argv.push(e.to_string());
    }
    if wbool(args, "dry_run") { argv.push(String::from("--dry-run")); }
    argv.push(String::from("--json"));
    let res = run_verb_owned(db, &argv)?;
    Ok(json!({ "applied": !wbool(args, "dry_run"), "file": file, "ref": ref_, "report": res }))
}

/// `kb_section_replace` — replace a section's body, keeping its heading.
pub fn t_section_replace(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let file = page_ok(db, wstr(args, "file")?)?;
    let ref_ = wstr(args, "ref")?;
    let text = wstr(args, "text")?;
    let mut argv: Vec<String> = vec![
        String::from("section"), String::from("--file"), file.clone(),
        String::from("replace"), ref_.to_string(),
        String::from("--text"), text.to_string(),
    ];
    if let Some(e) = woptstr(args, "if_unchanged") {
        argv.push(String::from("--if-unchanged")); argv.push(e.to_string());
    }
    if wbool(args, "dry_run") { argv.push(String::from("--dry-run")); }
    argv.push(String::from("--json"));
    let res = run_verb_owned(db, &argv)?;
    Ok(json!({ "applied": !wbool(args, "dry_run"), "file": file, "ref": ref_, "report": res }))
}

/// `kb_section_append` — append text at the end of a section's body.
pub fn t_section_append(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let file = page_ok(db, wstr(args, "file")?)?;
    let ref_ = wstr(args, "ref")?;
    let text = wstr(args, "text")?;
    let mut argv: Vec<String> = vec![
        String::from("section"), String::from("--file"), file.clone(),
        String::from("append"), ref_.to_string(),
        String::from("--text"), text.to_string(),
    ];
    if let Some(e) = woptstr(args, "if_unchanged") {
        argv.push(String::from("--if-unchanged")); argv.push(e.to_string());
    }
    if wbool(args, "dry_run") { argv.push(String::from("--dry-run")); }
    argv.push(String::from("--json"));
    let res = run_verb_owned(db, &argv)?;
    Ok(json!({ "applied": !wbool(args, "dry_run"), "file": file, "ref": ref_, "report": res }))
}

/// `kb_section_insert` — add a new section (heading + body) at a
/// placement (`before` / `after` / `child` / `top` / `end`).
pub fn t_section_insert(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let file = page_ok(db, wstr(args, "file")?)?;
    let title = wstr(args, "title")?;
    let text = wstr(args, "text")?;
    let placement = woptstr(args, "placement").unwrap_or("child");
    let mut argv: Vec<String> = vec![
        String::from("section"), String::from("--file"), file.clone(),
        String::from("insert"),
        String::from("--title"), title.to_string(),
        String::from("--placement"), placement.to_string(),
        String::from("--text"), text.to_string(),
    ];
    if let Some(r) = woptstr(args, "ref") {
        argv.push(String::from("--ref")); argv.push(r.to_string());
    }
    if let Some(e) = woptstr(args, "if_unchanged") {
        argv.push(String::from("--if-unchanged")); argv.push(e.to_string());
    }
    if wbool(args, "dry_run") { argv.push(String::from("--dry-run")); }
    argv.push(String::from("--json"));
    let res = run_verb_owned(db, &argv)?;
    Ok(json!({ "applied": !wbool(args, "dry_run"), "file": file, "title": title, "report": res }))
}

/// `kb_section_rm` — delete a section (heading + body + subsections),
/// a frontmatter field, or one list item from `fm.<list>`.
pub fn t_section_rm(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let file = page_ok(db, wstr(args, "file")?)?;
    let ref_ = wstr(args, "ref")?;
    let mut argv: Vec<String> = vec![
        String::from("section"), String::from("--file"), file.clone(),
        String::from("rm"), ref_.to_string(),
    ];
    if let Some(v) = woptstr(args, "value") { argv.push(v.to_string()); }
    if let Some(e) = woptstr(args, "if_unchanged") {
        argv.push(String::from("--if-unchanged")); argv.push(e.to_string());
    }
    if wbool(args, "dry_run") { argv.push(String::from("--dry-run")); }
    argv.push(String::from("--json"));
    let res = run_verb_owned(db, &argv)?;
    Ok(json!({ "applied": !wbool(args, "dry_run"), "file": file, "ref": ref_, "report": res }))
}

/// `kb_replace` — substring find/replace across a file, or scoped to
/// one section. `count` bounds the number of substitutions.
pub fn t_replace(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let file = page_ok(db, wstr(args, "file")?)?;
    let find = wstr(args, "find")?;
    let new = wstr(args, "new")?;
    let mut argv: Vec<String> = vec![
        String::from("replace"), file.clone(),
        String::from("--find"), find.to_string(),
        String::from("--new"), new.to_string(),
    ];
    if let Some(s) = woptstr(args, "scope") {
        argv.push(String::from("--scope")); argv.push(s.to_string());
    }
    if let Some(n) = args.get("count").and_then(|v| v.as_u64()) {
        argv.push(String::from("--count")); argv.push(n.to_string());
    }
    if let Some(e) = woptstr(args, "if_unchanged") {
        argv.push(String::from("--if-unchanged")); argv.push(e.to_string());
    }
    if wbool(args, "dry_run") { argv.push(String::from("--dry-run")); }
    argv.push(String::from("--json"));
    let res = run_verb_owned(db, &argv)?;
    Ok(json!({ "applied": !wbool(args, "dry_run"), "file": file, "report": res }))
}

/// `kb_patch` — apply an external `diff -u` / `git diff` patch to a file.
pub fn t_patch(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let file = page_ok(db, wstr(args, "file")?)?;
    let patch_text = wstr(args, "patch_text")?;
    let mut argv: Vec<String> = vec![
        String::from("patch"), file.clone(),
        String::from("--patch-text"), patch_text.to_string(),
    ];
    if let Some(e) = woptstr(args, "if_unchanged") {
        argv.push(String::from("--if-unchanged")); argv.push(e.to_string());
    }
    if wbool(args, "dry_run") { argv.push(String::from("--dry-run")); }
    argv.push(String::from("--json"));
    let res = run_verb_owned(db, &argv)?;
    Ok(json!({ "applied": !wbool(args, "dry_run"), "file": file, "report": res }))
}

// ── tag-alias writes (DB-only, no file touched) ──────────────────────

/// `kb_tag_alias_add` — map `alias` -> `canonical tag`.
pub fn t_tag_alias_add(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let alias = wstr(args, "alias")?;
    let canonical = wstr(args, "canonical")?;
    let argv: Vec<String> = vec![
        String::from("tag-alias"),
        String::from("--add"), alias.to_string(),
        String::from("--canonical"), canonical.to_string(),
        String::from("--json"),
    ];
    let res = run_verb_owned(db, &argv)?;
    Ok(json!({ "added": true, "alias": alias, "canonical": canonical, "report": res }))
}

/// `kb_tag_alias_rm` — remove an `alias` from the map.
pub fn t_tag_alias_rm(db: &Db, args: &Value) -> Result<Value, (String, i32)> {
    let alias = wstr(args, "alias")?;
    let argv: Vec<String> = vec![
        String::from("tag-alias"),
        String::from("--rm"), alias.to_string(),
        String::from("--json"),
    ];
    let res = run_verb_owned(db, &argv)?;
    Ok(json!({ "removed": true, "alias": alias, "report": res }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CTR: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        /// Set up a one-page corpus (with the `foo` tag) and a file-backed
        /// DB. The `KB_DB` env var is what the spawned verb inherits, so
        /// the child indexes the same page set as the parent would.
        fn new(tag: &str) -> (Self, Db) {
            let n = CTR.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let base = std::env::temp_dir().join(format!("kbwrite_{pid}_{n}_{tag}"));
            let _ = std::fs::remove_dir_all(&base);
            let root = base.join("root");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("a.md"),
                "---\ntype: note\ntitle: A\ntags:\n  - foo\n---\n# A\nbody line\n",
            )
            .unwrap();
            let db = base.join("kb.db");
            let t = Fixture { root };
            // Register the path thread-locally so the spawned verb subprocess
            // routes through the same file the parent `Db` opened.
            crate::mcp::tools::set_dbpath(Some(&db));
            // `current_exe()` in the test harness is the libtest binary (in
            // `target/debug/build/kb-hardened/<hash>/out/`), *not* the
            // production CLI — pin the verb to the production binary built by
            // the previous gate stage (`target/debug/kb-hardened`), the same
            // path production uses at runtime.
            let prod = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..").join("target").join("debug").join("kb-hardened");
            std::env::set_var("KB_MCP_VERB", &prod);
            let d = Db::open(&db).unwrap();
            d.migrate().unwrap();
            d.add_root(&t.root.to_string_lossy(), None, false).unwrap();
            kb_refresh::refresh(&d).unwrap();
            (t, d)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(p) = self.root.parent() {
                let _ = std::fs::remove_dir_all(p);
            }
        }
    }

    /// The `run_verb` subprocess route invokes the *production* `kb-hardened`
    /// binary built by an earlier gate stage — the test harness is itself
    /// not a CLI, so it would fail if used here. We rely on
    /// `CARGO_TARGET_DIR` (always set by cargo) to find that binary.
    /// `db.path()` is also required: it is how `run_verb` routes the
    /// verb to the same DB file the parent holds (via the global `--db`
    /// flag).
    #[test]
    fn section_set_dry_run_does_not_modify_the_page() {
        let (fix, db) = Fixture::new("dry");
        let before = std::fs::read(fix.root.join("a.md")).unwrap();
        let v = json!({"file" : fix.root.join("a.md").display().to_string(), "ref" : "1", "text" : "new body", "dry_run" : true});
        let res = super::t_section_set(&db, &v).unwrap();
        assert_eq!(res.get("applied").and_then(|x| x.as_bool()), Some(false));
        let after = std::fs::read(fix.root.join("a.md")).unwrap();
        assert_eq!(before, after, "dry-run must not change the page");
    }

    #[test]
    fn section_set_applied_rewrites_the_page() {
        let (fix, db) = Fixture::new("apply");
        let v = json!({"file" : fix.root.join("a.md").display().to_string(), "ref" : "1", "text" : "brand new body"});
        let res = super::t_section_set(&db, &v).unwrap();
        assert_eq!(res.get("applied").and_then(|x| x.as_bool()), Some(true));
        let body = std::fs::read_to_string(fix.root.join("a.md")).unwrap();
        assert!(body.contains("brand new body"), "expected the new body; got {body:?}");
    }

    #[test]
    fn tag_alias_add_then_list_returns_the_alias() {
        let (_fix, db) = Fixture::new("alias_list");
        let v = json!({"alias" : "foo", "canonical" : "bar"});
        let res = super::t_tag_alias_add(&db, &v).unwrap();
        assert_eq!(res.get("added").and_then(|x| x.as_bool()), Some(true));
        let argv = vec![String::from("tag-alias"), String::from("--list"), String::from("--json")];
        let vlist = run_verb_owned(&db, &argv).unwrap();
        let s = vlist.to_string();
        assert!(
            s.contains("foo") && s.contains("bar"),
            "alias mapping missing from list: {s}"
        );
    }

    // ── slice 7: the wire-level e2e round-trip ─────────────────────────
    // Existing tests call the handlers directly (t_section_set, …), which
    // proves the handler logic but skips the JSON-RPC wire layer — the
    // part that actually carries frames over stdio in production. This test
    // drives real frames through `dispatch` (the function `serve` calls
    // per line), exercising initialize → tools/list → tools/call (a write
    // that lands via the subprocess verb path) → read-back, in one session.
    // It is the closest in-process equivalent of `mcp serve < fixture.json`
    // without the strace harness; the no-network gate (d) is the syscall
    // proof, this is the *does-it-work* proof.

    /// Send one frame through `dispatch` and parse the JSON-RPC response.
    fn frame(db: &Db, payload: &str) -> Value {
        let resp = crate::mcp::tools::dispatch(payload, db);
        serde_json::from_str(&resp).expect("dispatch returned non-JSON")
    }

    #[test]
    fn e2e_initialize_list_create_and_read_back_over_the_wire() {
        let (fix, db) = Fixture::new("e2e");
        // (1) initialize — handshake shape must be stable.
        let init = frame(&db, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}"#);
        assert_eq!(init.get("result").and_then(|r| r.get("protocolVersion")).and_then(|x| x.as_str()), Some("2024-11-05"));
        assert_eq!(init.get("result").and_then(|r| r.get("serverInfo")).and_then(|r| r.get("name")).and_then(|x| x.as_str()), Some("kb"));
        // (2) tools/list — the write surface is advertised.
        let list = frame(&db, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap()
            .iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"kb_create"), "kb_create missing from tools/list: {names:?}");
        assert!(names.contains(&"kb_browse"), "kb_browse missing from tools/list: {names:?}");
        // (3) tools/call kb_create — the write lands through the subprocess verb.
        let call = frame(&db, &format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"kb_create","arguments":{{"name":"e2e-page","type":"note","path":"{}"}}}}}}"#,
            fix.root.display()
        ));
        assert_eq!(call.get("result").and_then(|r| r.get("created")).and_then(|x| x.as_bool()), Some(true),
            "kb_create did not report success: {call}");
        // (4) the page actually landed on disk under the target directory.
        let created = fix.root.join("e2e-page.md");
        assert!(created.exists(), "expected {created:?} on disk after kb_create over the wire");
        let body = std::fs::read_to_string(&created).unwrap();
        assert!(body.contains("e2e-page"), "created page should carry its slug; got {body:?}");
        // (5) read it back through kb_show over the wire (the index picked
        //     it up; the show path resolves it back to its canonical abs).
        let back = frame(&db, &format!(
            r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"kb_show","arguments":{{"file":"{}"}}}}}}"#,
            created.display()
        ));
        assert!(back.to_string().contains("e2e-page"),
            "kb_show did not return the created page: {back}");
        // (6) invariant 4 over the wire: the *identity* is the uuid token
        //     (the one stable address), and the response must not carry a
        //     structural `root`/`abs_path`/`rel_path`/`file` page-identity
        //     key. (The `diff` content blob may embed a path — that is
        //     content-the-agent-asked-for and is preserved by design; the
        //     structural keys are the sanitizer's job, unit-tested in
        //     sanitizer.rs.)
        let rep = &call["result"]["report"];
        assert!(rep.get("uuid").and_then(|x| x.as_str()).ok_or("no uuid in create report").is_ok());
        for k in ["root", "abs_path", "rel_path", "file", "path"] {
            assert!(rep.get(k).is_none(), "create report leaked a structural path key '{k}': {rep}");
        }
    }
}
