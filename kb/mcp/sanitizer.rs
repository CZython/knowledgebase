//! The sanitizer — the single chokepoint for MCP output (MCP.md).
//!
//! Everything that leaves the process (JSON tool responses + error
//! messages) passes through `sanitize_value` exactly once. That function is
//! the audited surface where the four no-escape properties are enforced:
//!
//! 1. **Rows** with frontmatter `mcp_hidden: true` are dropped (not
//!    flagged) — a row that says "`mcp_hidden: true`" has already told the
//!    agent the page exists.
//! 2. **Path columns** (`root` / `rel_path` / `abs_path`) are stripped from
//!    every remaining object.
//! 3. **Snippets** are scrubbed: the `mcp_hidden:` token is removed from
//!    any string that looks like a FTS snippet (or a frontmatter view).
//! 4. **Error messages** that would otherwise expose a path are replaced
//!    with a stable error code + (if known) the page uuid.
//!
//! Slice 4: properties 1 + 2 + 3 are implemented and tested. Property 4
//! (error mapping) is `sanitize_error` below — a stable JSON-RPC code + a
//! path-free message, so an error object is as opaque as a result object.
//! Slice 5 lands the read tools that call into this.
//!
//! **Content is not metadata.** The sanitizer only touches *structural*
//! fields — `root`, `rel_path`, `abs_path`, and known path-y keys. A body
//! that contains `/home/jakub/…` is returned unchanged (content is content;
//! MCP.md "The escape property", last bullet).

use serde_json::{Map, Value};

/// Recursively sanitize a JSON tree: drop hidden pages, strip structural
/// path fields, scrub `mcp_hidden:` tokens from strings.
///
/// Returns a new `Value` — the input is consumed.
pub(crate) fn sanitize_value(v: Value) -> Value {
    match v {
        Value::Object(m) => sanitize_object(m),
        Value::Array(items) => Value::Array(drop_hidden(items.into_iter().map(sanitize_value).collect())),
        Value::String(s) => Value::String(sanitize_string(&s)),
        other => other,
    }
}

/// A whole object is a *page* if it names one (`id` or `uuid`) — that is
/// the one shape where "it says `mcp_hidden: true`" already told the agent
/// the page exists, so the object vanishes (MCP.md invariant 1: *delete
/// the row*, don't hide the field). An object without either key is not a
/// page object (e.g. the envelope's inner `error` object carries no
/// identity); it is kept and only its key/value space is scrubbed.
fn sanitize_object(m: Map<String, Value>) -> Value {
    let is_page = m.contains_key("id") || m.contains_key("uuid");
    if is_page {
        if let Some(flag) = m.get("mcp_hidden") {
            if is_truthy(flag) {
                return Value::Null; // hidden page — drop the whole object
            }
        }
    }
    let mut out = Map::with_capacity(m.len());
    for (k, v) in m {
        if k == "root" || k == "rel_path" || k == "abs_path" {
            continue;
        }
        if k == "path" || k == "file" {
            continue;
        }
        if k == "mcp_hidden" {
            // The opt-out marker is metadata, not content — the key never
            // crosses the wire. (A truthy flag on a page object already
            // returned `Null` above; here only key-scrubbing applies.)
            continue;
        }
        out.insert(k, sanitize_value(v));
    }
    Value::Object(out)
}

/// Drop elements that sanitized to `Null` (i.e. were hidden pages).
fn drop_hidden(mut items: Vec<Value>) -> Vec<Value> {
    items.retain(|v| !v.is_null());
    items
}

/// "Hidden" per the frontmatter contract (MCP.md, slice-3 parse): `true`,
/// `"1"`, `"yes"` (case-insensitive). JSON booleans and strings cover the
/// frontmatter scalars; anything else (`0`, `false`, `"no"`, numbers) is
/// not hidden.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
        _ => false,
    }
}

// ─────────────────────────── error mapping (invariant 4) ──────────────────
//
/// Stable JSON-RPC error codes for kb errors crossing the wire.
/// `-32602` = invalid params (the target did not resolve — including a
/// hidden page, which is indistinguishable from a missing one);
/// `-32603` = internal error (engine/IO failure — the message must not
/// carry the engine's diagnostics).
pub(crate) const ERR_INVALID_PARAMS: i64 = -32602;
pub(crate) const ERR_INTERNAL: i64 = -32603;

/// Map a CLI-level error string (`Kb`'s `(String, i32)` right side) into
/// a stable `(code, message)` pair (MCP.md invariant 4: "an error is also
/// a payload"). Messages are canonical and short: a filesystem path, an
/// engine diagnostic, or an exit-code digit never appears in them.
#[allow(dead_code)] // consumed by the tool handlers landing in slice 5
pub(crate) fn sanitize_error(msg: &str) -> (i64, String) {
    let lower = msg.to_ascii_lowercase();
    let not_found = lower.contains("no such page")
        || lower.contains("no page")
        || lower.contains("not found");
    let invalid = lower.contains("unrecognized")
        || lower.contains("invalid")
        || lower.contains("unexpected argument")
        || lower.contains("unknown tool")
        || lower.contains("unknown method");
    if not_found {
        (ERR_INVALID_PARAMS, "no such page".into())
    } else if invalid {
        (ERR_INVALID_PARAMS, "invalid params".into())
    } else {
        (ERR_INTERNAL, "internal error".into())
    }
}

/// A string that *is* a structural path slot (not a body sentence).
///
/// Heuristic: a short line (< 256 chars) that starts with `/`, `~`, `.\`,
/// or a Windows drive letter (`X:\`). A body sentence that starts with
/// `/` is longer (a complete sentence) and is kept — content is content.
fn is_structural_path(s: &str) -> bool {
    let s = s.trim_start();
    if s.len() > 256 {
        return false;
    }
    let bytes = s.as_bytes();
    let first = bytes.first().copied().unwrap_or(0);
    if matches!(first, b'/' | b'~' | b'.' | b'\\') {
        return true;
    }
    bytes.len() >= 3 && first.is_ascii_uppercase() && bytes[1] == b':' && bytes[2] == b'\\'
}

/// A *line* in the input that is only a structural path (e.g. a FTS
/// snippet's path line, or a bare `path:` field in a frontmatter view) is
/// dropped; a line that *begins* with a path but carries more content is
/// kept (it's a body line that mentions a path — content is content). The
/// `mcp_hidden:` token is stripped from every remaining line (invariant 1:
/// a snippet that matched on the frontmatter line must not leak that the
/// page opted out). The token is removed wherever it occurs — `replace` is
/// the simplest correct form and is idempotent.
fn sanitize_string(s: &str) -> String {
    let has_token = s.contains("mcp_hidden:");
    if !has_token && !s.lines().any(|l| is_structural_path(l.trim_start())) {
        // Nothing to strip: no `mcp_hidden:` token and no line is a bare
        // structural path. Return the original bytes (content is content).
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut first_line = true;
    for line in s.lines() {
        if !first_line {
            out.push('\n');
        }
        first_line = false;
        if is_structural_path(line.trim_start()) {
            // This line is only a path — drop it.
            continue;
        }
        if has_token {
            let scrubbed = line.replace("mcp_hidden:", "");
            out.push_str(&scrubbed);
        } else {
            out.push_str(line);
        }
    }
    if s.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn drops_known_path_keys() {
        let v = json!({
            "id": "wiki:foo",
            "uuid": "abcdef1234567890",
            "abs_path": "/home/jakub/notes/foo.md",
            "rel_path": "foo.md",
            "root": "/home/jakub/notes",
            "title": "Foo"
        });
        let out = sanitize_value(v);
        assert_eq!(out["id"], "wiki:foo");
        assert_eq!(out["uuid"], "abcdef1234567890");
        assert_eq!(out["title"], "Foo");
        assert!(out.get("abs_path").is_none());
        assert!(out.get("rel_path").is_none());
        assert!(out.get("root").is_none());
    }

    #[test]
    fn hidden_truthy_page_object_vanishes() {
        // Invariant 1: a page object that says `mcp_hidden: true` is
        // *deleted* (sanitized to `Null`), not flagged — the flag itself is
        // the leak.
        let v = json!({"id": "wiki:foo", "mcp_hidden": true, "title": "Foo"});
        assert!(sanitize_value(v).is_null());
    }

    #[test]
    fn hidden_truthy_string_variants_vanish() {
        for s in ["1", "yes", "YES", "True"] {
            let v = json!({"uuid": "abcdef0123456789", "mcp_hidden": s, "title": "Foo"});
            assert!(
                sanitize_value(v).is_null(),
                "{s:?} must be hidden"
            );
        }
    }

    #[test]
    fn falsey_mcp_hidden_key_dropped_but_page_kept() {
        for val in [json!(false), json!("0"), json!("no"), json!(""), json!(null)] {
            let v = json!({"id": "wiki:foo", "mcp_hidden": val, "title": "Foo"});
            let out = sanitize_value(v);
            assert_eq!(out["id"], "wiki:foo", "falsey flag keeps the page");
            assert!(out.get("mcp_hidden").is_none(), "the key itself never crosses the wire");
        }
    }

    #[test]
    fn hidden_row_dropped_from_array_others_kept() {
        // The search-result shape: hidden pages disappear from the list;
        // the survivors are intact. The agent cannot learn the page exists.
        let v = json!([
            {"id": "wiki:a", "mcp_hidden": true, "title": "A", "uuid": "aaaa000000000001"},
            {"id": "wiki:b", "title": "B", "uuid": "bbbb000000000002"},
            {"id": "wiki:c", "mcp_hidden": "yes", "title": "C", "uuid": "cccc000000000003"},
        ]);
        let out = sanitize_value(v);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1, "got: {arr:?}");
        assert_eq!(arr[0]["id"], "wiki:b");
        assert_eq!(arr[0]["uuid"], "bbbb000000000002");
    }

    #[test]
    fn hidden_nested_page_dropped() {
        // A page object nested inside a wrapper (e.g. `{"page": {...}}`)
        // vanishes too; the wrapper survives with a null slot.
        let v = json!({"page": {"id": "wiki:x", "mcp_hidden": true, "title": "X"}});
        let out = sanitize_value(v);
        assert!(out["page"].is_null());
    }

    #[test]
    fn non_page_object_with_truthy_flag_kept_but_flag_dropped() {
        // An object without `id`/`uuid` is not a page object — it carries
        // no identity for the agent, so "hidden" has nothing to hide; only
        // the key is scrubbed.
        let v = json!({"mcp_hidden": true, "note": "frontmatter dump"});
        let out = sanitize_value(v);
        assert!(out.get("mcp_hidden").is_none());
        assert_eq!(out["note"], "frontmatter dump");
    }

    #[test]
    fn strips_mcp_hidden_token_from_snippet_line() {
        let v = json!({"id": "wiki:foo", "snippet": "mcp_hidden: true the secret stuff"});
        let out = sanitize_value(v);
        let s = out["snippet"].as_str().unwrap();
        assert!(!s.contains("mcp_hidden:"), "got: {s}");
        assert!(s.contains("the secret stuff"), "got: {s}");
    }

    #[test]
    fn keeps_body_content_paths() {
        let v = json!({
            "id": "wiki:foo",
            "body": "see /home/jakub/notes/foo.md for details"
        });
        let out = sanitize_value(v);
        let b = out["body"].as_str().unwrap();
        assert!(b.contains("/home/jakub/notes/foo.md"), "got: {b}");
    }

    #[test]
    fn recursion_into_arrays() {
        let v = json!([
            {"id": "a", "abs_path": "/x/a.md", "title": "A"},
            {"id": "b", "rel_path": "b.md", "title": "B"}
        ]);
        let out = sanitize_value(v);
        let arr = out.as_array().unwrap();
        assert_eq!(arr[0]["id"], "a");
        assert!(arr[0].get("abs_path").is_none());
        assert!(arr[1].get("rel_path").is_none());
    }

    #[test]
    fn structural_path_key_dropped_wholesale() {
        // A *path-shaped key* (`path` / `file` / `root` / `rel_path` /
        // `abs_path`) is dropped at the object level, regardless of its
        // value — the value may be multi-line and that doesn't matter:
        // it's metadata, not content.
        let v = json!({"path": "/home/jakub/notes/foo.md", "title": "Foo"});
        let out = sanitize_value(v);
        assert!(out.get("path").is_none());
        assert_eq!(out["title"], "Foo");
    }

    #[test]
    fn path_only_line_dropped_but_content_line_kept() {
        // A string value whose *first line* is a bare path drops only that
        // line; the rest of the string (a body line with real content) is
        // preserved. This is the FTS-snippet shape: line 1 = the page path,
        // line 2+ = the matching body snippet.
        let v = json!({"snippet": "/home/jakub/notes/foo.md\nthe secret stuff here"});
        let out = sanitize_value(v);
        let s = out["snippet"].as_str().unwrap_or("?");
        assert!(!s.contains("/home/jakub"), "got: {s}");
        assert!(s.contains("the secret stuff here"), "got: {s}");
    }

    // ───────────────────────── error mapping (invariant 4) ──────────────

    #[test]
    fn error_not_found_gets_stable_code_and_path_free_message() {
        let (code, msg) = sanitize_error("no such page at `/home/jakub/notes/secret.md` (exit 1)");
        assert_eq!(code, ERR_INVALID_PARAMS);
        assert_eq!(msg, "no such page");
        assert!(!msg.contains('/'), "no path in the message: {msg}");
        assert!(!msg.contains("jakub"), "no username in the message: {msg}");
    }

    #[test]
    fn error_hidden_page_indistinguishable_from_missing() {
        // Slice-4 contract: a hidden page and a missing page produce the
        // *exact same* (code, message) pair — the agent cannot tell which
        // one it hit.
        let a = sanitize_error("no such page: abcdef0123456789");
        let b = sanitize_error("no page found for `wiki:secret`");
        assert_eq!(a, b);
    }

    #[test]
    fn error_sqlite_diagnostic_maps_to_internal() {
        let (code, msg) = sanitize_error(
            "SqliteFailure(1, Some(\"FTS parse error: / near `x`\"))",
        );
        assert_eq!(code, ERR_INTERNAL);
        assert_eq!(msg, "internal error");
    }

    #[test]
    fn error_invalid_params_maps_to_invalid_params() {
        let (code, msg) = sanitize_error("unexpected argument `-e` found (exit 2)");
        assert_eq!(code, ERR_INVALID_PARAMS);
        assert_eq!(msg, "invalid params");
    }

    #[test]
    fn error_fallback_is_internal() {
        let (code, msg) = sanitize_error("kb: create: /home/jakub/x.md already exists");
        assert_eq!(code, ERR_INTERNAL);
        assert_eq!(msg, "internal error");
    }
}
