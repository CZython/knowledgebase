//! M9 — the agent-facing surface.
//!
//! Part A (every TUI feature has a plain-CLI bypass) and Part C (DB location
//! via `--db`) were already true before this milestone; this suite pins Part B:
//! **every** CLI verb carries `--json`, and the JSON is *valid* (parsed by
//! `python3 -c 'import json,sys; json.loads(sys.stdin.read())'`), well-formed
//! (the right top-level kind for the verb), and carries the fields an agent
//! would need.
//!
//! Drives the real `kb` binary as a subprocess against a throwaway root +
//! DB, mirroring `m7_multifile.rs` so the surface is asserted end-to-end.
//!
//! Run: `cargo test -p kb-user --test m9_agent_surface`

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn kb() -> Command {
    // `kb` is the bin name (the *package* is `kb-user`); cargo exposes it to
    // tests as `CARGO_BIN_EXE_kb` at runtime.
    Command::new(std::env::var("CARGO_BIN_EXE_kb").expect("CARGO_BIN_EXE_kb unset"))
}

struct Setup {
    root: PathBuf,
    db: PathBuf,
}

impl Setup {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("kbm9_{pid}_{tag}", pid = std::process::id()));
        let root = base.join("root");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let db = base.join("kb.db");
        let _ = fs::remove_file(&db);
        Setup { root, db }
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.root.parent().unwrap());
    }
}

fn run(setup: &Setup, args: &[&str]) -> (i32, String, String) {
    let db = setup.db.to_string_lossy().into_owned();
    let mut cmd = kb();
    cmd.arg(format!("--db={db}"));
    for x in args {
        cmd.arg(x);
    }
    let out = cmd.output().expect("failed to run kb");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write_page(setup: &Setup, rel: &str, fm: &str, body: &str) {
    let p = setup.root.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&p, format!("---\n{fm}---\n{body}")).unwrap();
}

/// Validate that `s` parses as a single JSON document and the top-level
/// kind matches (`array` for list-shaped verbs, `object` for record-shaped).
fn assert_json(s: &str, what: &str, is_array: bool, is_object: bool) {
    let mut v = Command::new("python3");
    v.arg("-c").arg(
        "import json,sys\n\
         d = json.loads(sys.stdin.read())\n\
         t = type(d).__name__\n\
         want = sys.argv[1]\n\
         sys.exit(0 if t == want else 1)\n",
    );
    v.arg(if is_array { "list" } else { "dict" });
    v.stdin(std::process::Stdio::piped());
    v.stdout(std::process::Stdio::piped());
    v.stderr(std::process::Stdio::piped());
    let mut ch = v.spawn().unwrap();
    {
        use std::io::Write;
        ch.stdin.as_mut().unwrap().write_all(s.as_bytes()).unwrap();
    }
    let res = ch.wait_with_output().unwrap();
    if !res.status.success() {
        eprintln!(
            "assert_json({what}) FAILED\n--- stdout ---\n{s}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&res.stderr)
        );
        panic!("assert_json({what}) failed — bad JSON or wrong top-level kind");
    }
    let _ = is_object;
}

// ============================ M9 tests ============================

#[test]
fn part_c_db_flag_is_honored() {
    // Part C: every command respects --db; this test asserts it end-to-end
    // (the M7 suite already pins it; M9 re-asserts it for the write path
    // with a throwaway DB).
    let s = Setup::new("dbflag");
    let root = s.root.to_string_lossy().into_owned();
    write_page(&s, "a.md", "type: note\ntitle: A\n", "hello\n");
    let (code, _, _) = run(&s, &["add", &root]);
    assert_eq!(code, 0, "add --db must succeed against the throwaway DB");
    let (code, out, _) = run(&s, &["roots", "--json"]);
    assert_eq!(code, 0);
    assert!(out.contains(&root), "roots --json must surface the path");
}

#[test]
fn roots_json_roundtrip() {
    let s = Setup::new("roots");
    let root = s.root.to_string_lossy().into_owned();
    write_page(&s, "a.md", "type: note\ntitle: A\n", "x\n");
    assert_eq!(run(&s, &["add", &root]).0, 0);
    let (code, out, _) = run(&s, &["roots", "--json"]);
    assert_eq!(code, 0);
    assert_json(&out, "roots", true, false);
    assert!(
        out.starts_with('[') || out.trim_start().starts_with('['),
        "roots --json must be a JSON array"
    );
    assert!(
        out.contains(&format!("\"path\": \"{root}\"")),
        "roots --json must carry the registered path;\ngot:\n{out}"
    );
    assert!(
        out.contains("\"include_hidden\""),
        "roots --json must carry include_hidden"
    );
}

#[test]
fn add_droproot_refresh_json() {
    let s = Setup::new("addrefresh");
    let root = s.root.to_string_lossy().into_owned();
    write_page(&s, "a.md", "type: note\ntitle: A\n", "x\n");
    write_page(&s, "b.md", "type: wiki\ntitle: B\n", "y\n");

    let (code, out, _) = run(&s, &["add", &root, "--json"]);
    assert_eq!(code, 0);
    assert_json(&out, "add", false, true);
    assert!(
        out.contains("\"registered\""),
        "add --json must carry the registered path"
    );
    assert!(
        out.contains("\"refresh\""),
        "add --json must embed the refresh summary"
    );

    let (code, out, _) = run(&s, &["refresh", "--json"]);
    assert_eq!(code, 0);
    assert_json(&out, "refresh", false, true);
    for k in [
        "full",
        "indexed",
        "skipped_no_type",
        "skipped_missing_slug",
        "gc",
        "roots_walked",
    ] {
        assert!(
            out.contains(&format!("\"{k}\"")),
            "refresh --json missing key {k}\ngot: {out}"
        );
    }

    let (code, out, _) = run(&s, &["full-rebuild", "--json"]);
    assert_eq!(code, 0);
    assert_json(&out, "full-rebuild", false, true);
    assert!(
        out.contains("\"full\": true"),
        "full-rebuild --json must be full:true"
    );

    let (code, out, _) = run(&s, &["drop-root", &root, "--json"]);
    assert_eq!(code, 0);
    assert_json(&out, "drop-root", false, true);
    assert!(out.contains("\"dropped\"") && out.contains("\"gc\""));
    // After the drop, the indexed pages should be gone (GC).
    let (code, out, _) = run(&s, &["refresh", "--json"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("\"gc\": ") || out.contains("\"gc\":0"),
        "refresh after drop must carry a gc count; got: {out}"
    );
}

#[test]
fn browse_tag_json() {
    let s = Setup::new("browsetag");
    let root = s.root.to_string_lossy().into_owned();
    write_page(
        &s,
        "a.md",
        "type: note\ntitle: Alpha\ntags:\n  - foo\n  - bar\n",
        "alpha\n",
    );
    write_page(
        &s,
        "b.md",
        "type: wiki\ntitle: Beta\ntags:\n  - bar\n",
        "beta\n",
    );
    let (code, _, _) = run(&s, &["add", &root]);
    assert_eq!(code, 0);

    {
        let (code, out, _) = run(&s, &["browse", "--json"]);
        assert_eq!(code, 0);
        assert_json(&out, "browse", true, false);
        assert!(
            out.contains("\"id\"") && out.contains("\"tags\""),
            "browse must carry id+tags; got: {out}"
        );
        assert!(
            out.contains("Alpha") && out.contains("Beta"),
            "both pages present; got: {out}"
        );
    }
    {
        let (code, out, _) = run(&s, &["tag", "bar", "--json"]);
        assert_eq!(code, 0);
        assert_json(&out, "tag", true, false);
        assert!(
            out.contains("Alpha") && out.contains("Beta"),
            "both tagged pages; got: {out}"
        );
    }
    {
        // A tag no one uses — the canonical empty array `[]` (not `[ ]`,
        // not the phantom record `[{}]`). `jq 'length'` / `json.loads` must
        // see a genuinely empty list. (F6 / C3 exact-pin.)
        let (code, out, _) = run(&s, &["tag", "nobody", "--json"]);
        assert_eq!(code, 0);
        assert_eq!(
            out.trim(),
            "[]",
            "empty tag --json must be exactly the canonical empty array; got: {out:?}"
        );
    }
}

#[test]
fn tag_alias_json_roundtrip() {
    let s = Setup::new("alias");
    let root = s.root.to_string_lossy().into_owned();
    write_page(&s, "a.md", "type: note\ntitle: A\n", "x\n");
    let (code, _, _) = run(&s, &["add", &root]);
    assert_eq!(code, 0);

    // empty list — canonical `[]` (F6 / C3 exact-pin)
    {
        let (code, out, _) = run(&s, &["tag-alias", "--list", "--json"]);
        assert_eq!(code, 0);
        assert_eq!(
            out.trim(),
            "[]",
            "empty alias --list --json must be exactly the canonical empty array; got: {out:?}"
        );
    }
    // add
    {
        let (code, out, _) = run(
            &s,
            &[
                "tag-alias",
                "--add",
                "newalias",
                "--canonical",
                "REALCANON",
                "--json",
            ],
        );
        assert_eq!(code, 0);
        assert_json(&out, "alias--add", false, true);
        assert!(
            out.contains("\"alias\"") && out.contains("\"canonical\""),
            "alias --add --json must carry both; got: {out}"
        );
    }
    // list now shows the row
    {
        let (code, out, _) = run(&s, &["tag-alias", "--list", "--json"]);
        assert_eq!(code, 0);
        assert_json(&out, "alias--list", true, false);
        assert!(
            out.contains("\"newalias\"") && out.contains("\"REALCANON\""),
            "alias --list --json must show the row; got: {out}"
        );
    }
    // rm removes it
    {
        let (code, out, _) = run(&s, &["tag-alias", "--rm", "newalias", "--json"]);
        assert_eq!(code, 0);
        assert_json(&out, "alias--rm", false, true);
        assert!(
            out.contains("\"removed\""),
            "alias --rm --json must carry a removed count; got: {out}"
        );
    }
}

#[test]
fn resolve_info_links_json() {
    let s = Setup::new("rii");
    let root = s.root.to_string_lossy().into_owned();
    write_page(
        &s,
        "alpha.md",
        "type: note\ntitle: Unique-Slug-A\n",
        "alpha\n",
    );
    write_page(
        &s,
        "beta.md",
        "type: note\ntitle: Unique-Slug-B\n",
        "beta\n",
    );
    let (code, _, _) = run(&s, &["add", &root]);
    assert_eq!(code, 0);

    // resolve -- unique
    {
        let (code, out, _) = run(&s, &["resolve", "Unique-Slug-A", "--json"]);
        assert_eq!(code, 0);
        assert_json(&out, "resolve-unique", false, true);
        assert!(
            out.contains("\"outcome\": \"unique\""),
            "unique outcome; got: {out}"
        );
        assert!(
            out.contains("\"id\"") && out.contains("\"abs_path\""),
            "id+abs_path in candidates; got: {out}"
        );
    }
    // resolve -- empty
    {
        let (code, out, _) = run(&s, &["resolve", "nobody-here", "--json"]);
        assert_eq!(code, 0);
        assert_json(&out, "resolve-empty", false, true);
        assert!(
            out.contains("\"outcome\": \"empty\""),
            "empty outcome; got: {out}"
        );
    }
    // info
    let page = s.root.join("alpha.md");
    {
        let (code, out, _) = run(&s, &["info", page.to_str().unwrap(), "--json"]);
        assert_eq!(code, 0);
        assert_json(&out, "info", false, true);
        for k in [
            "id", "type", "title", "abs_path", "rel_path", "tags", "links", "out", "in", "mtime",
        ] {
            assert!(
                out.contains(&format!("\"{k}\"")),
                "info must carry {k}; got: {out}"
            );
        }
        assert!(
            out.contains("Unique-Slug-A"),
            "info must carry the title; got: {out}"
        );
    }
    // links
    {
        let (code, out, _) = run(&s, &["links", "note:Unique-Slug-A", "--json"]);
        assert_eq!(code, 0);
        assert_json(&out, "links", false, true);
        assert!(
            out.contains("\"page\"")
                && out.contains("\"forward\"")
                && out.contains("\"backlinks\""),
            "links --json must carry page/forward/backlinks; got: {out}"
        );
    }
}

#[test]
fn stats_json() {
    let s = Setup::new("stats");
    let root = s.root.to_string_lossy().into_owned();
    write_page(&s, "a.md", "type: note\ntitle: A\n", "x\n");
    let (code, _, _) = run(&s, &["add", &root]);
    assert_eq!(code, 0);
    let (code, out, _) = run(&s, &["stats", "--json"]);
    assert_eq!(code, 0);
    assert_json(&out, "stats", false, true);
    for k in [
        "roots",
        "totals",
        "pages",
        "tags",
        "links",
        "tag_aliases",
        "db",
        "path",
        "size_bytes",
        "last_touch",
    ] {
        assert!(
            out.contains(&format!("\"{k}\"")),
            "stats must carry {k}; got: {out}"
        );
    }
}

#[test]
fn cron_json_roundtrip() {
    // Every CLI verb is --json-able, including cron. The envelope is a
    // single JSON object with the fields a script needs.
    let s = Setup::new("cron");
    {
        let (code, out, _) = run(&s, &["cron"]);
        assert_eq!(code, 0);
        assert!(
            out.contains("crontab -e"),
            "cron (text form) must print the paste-ready block; got: {out}"
        );
    }
    {
        let (code, out, _) = run(&s, &["cron", "--json"]);
        assert_eq!(code, 0);
        assert_json(&out, "cron", false, true);
        for k in ["command", "db", "line"] {
            assert!(
                out.contains(&format!("\"{k}\"")),
                "cron --json must carry {k}; got: {out}"
            );
        }
        assert!(
            out.contains("refresh"),
            "cron --json command must be the refresh subcommand; got: {out}"
        );
    }
}

// =============== write path: section / replace ===============

#[test]
fn section_op_json_roundtrip() {
    let s = Setup::new("section");
    let file = s.root.join("doc.md");
    let initial = "# Alpha\n\noriginal content\n\n# Beta\n\nother\n";
    fs::create_dir_all(&s.root).unwrap();
    fs::write(&file, initial).unwrap();

    // append a child — non-interactive (no TTY, no --yes needed under --json)
    let (code, out, _) = run(
        &s,
        &[
            "section",
            "--file",
            file.to_str().unwrap(),
            "append",
            "Alpha",
            "--text",
            "appended line",
            "--json",
        ],
    );
    assert_eq!(
        code, 0,
        "section --json append must apply non-interactively"
    );
    assert_json(&out, "section-append", false, true);
    for k in [
        "file",
        "changed",
        "dry_run",
        "applied",
        "bytes_before",
        "bytes_after",
        "diff",
    ] {
        assert!(
            out.contains(&format!("\"{k}\"")),
            "section --json must carry {k}; got: {out}"
        );
    }
    assert!(
        out.contains("\"applied\": true"),
        "append must report applied:true; got: {out}"
    );
    // the diff on stderr (not stdout), the JSON on stdout.
    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("appended line"),
        "append must have written"
    );

    // dry-run — applied:false, file unchanged
    let before = fs::read_to_string(&file).unwrap();
    {
        let (code, out, _) = run(
            &s,
            &[
                "section",
                "--file",
                file.to_str().unwrap(),
                "append",
                "Beta",
                "--text",
                "DRY",
                "--dry-run",
                "--json",
            ],
        );
        assert_eq!(code, 0);
        assert_json(&out, "section-dryrun", false, true);
        assert!(
            out.contains("\"applied\": false"),
            "dry-run must be applied:false; got: {out}"
        );
    }
    let after = fs::read_to_string(&file).unwrap();
    assert_eq!(before, after, "--dry-run --json must NOT write");
}

#[test]
fn replace_op_json_roundtrip() {
    let s = Setup::new("replace");
    let file = s.root.join("doc.md");
    fs::create_dir_all(&s.root).unwrap();
    fs::write(&file, "findme findme findme").unwrap();

    // --count 1 --json
    let (code, out, _) = run(
        &s,
        &[
            "replace",
            file.to_str().unwrap(),
            "--find",
            "findme",
            "--new",
            "Z",
            "--count",
            "1",
            "--json",
        ],
    );
    assert_eq!(code, 0);
    assert_json(&out, "replace", false, true);
    assert!(
        out.contains("\"applied\": true"),
        "replace must be applied; got: {out}"
    );
    let content = fs::read_to_string(&file).unwrap();
    assert_eq!(
        content, "Z findme findme",
        "replace --count 1 must replace exactly one"
    );

    // all remaining
    let (code, out, _) = run(
        &s,
        &[
            "replace",
            file.to_str().unwrap(),
            "--find",
            "findme",
            "--new",
            "W",
            "--json",
        ],
    );
    assert_eq!(code, 0);
    assert_json(&out, "replace2", false, true);
    assert!(out.contains("\"applied\": true"));
    let content = fs::read_to_string(&file).unwrap();
    assert_eq!(content, "Z W W", "replace (no count) must replace the rest");
}

#[test]
#[cfg(not(feature = "editor"))]
fn section_edit_removed() {
    // SECURITY_REPORT 3.1 — the `editor` feature is OFF for this test build,
    // so the `edit` subcommand (which spawned $VISUAL/$EDITOR) does not
    // exist. Clap must reject it as an unknown bare verb.
    let s = Setup::new("editgone");
    let file = s.root.join("doc.md");
    fs::create_dir_all(&s.root).unwrap();
    fs::write(&file, "# Sec\n\nx\n").unwrap();
    let (code, _, err) = run(
        &s,
        &[
            "section",
            "--file",
            file.to_str().unwrap(),
            "edit",
            "Sec",
            "--yes",
        ],
    );
    assert_ne!(
        code, 0,
        "`section edit` is not compiled in; clap should reject it"
    );
    let _ = err; // clap names the unrecognised subcommand on stderr
}

// =============== the search-namespace --json (already had it) ===============

#[test]
fn search_json_still_works() {
    let s = Setup::new("search");
    let root = s.root.to_string_lossy().into_owned();
    write_page(
        &s,
        "a.md",
        "type: note\ntitle: Unique-Slug\nsearch_required_phrase:\n  - gatedword\n",
        "gatedword text\n",
    );
    let (code, _, _) = run(&s, &["add", &root]);
    assert_eq!(code, 0);
    let (code, out, _) = run(&s, &["search", "gatedword", "--json"]);
    assert_eq!(code, 0);
    assert_json(&out, "search", true, false);
    assert!(
        out.contains("Unique-Slug") || out.contains("gatedword"),
        "search --json must include the page; got: {out}"
    );
}

// ==================== F6 / C3 — canonical empty array (exact) ====================

#[test]
fn search_empty_tag_filter_emits_canonical_empty_array() {
    // Regression for F6: `search "tag:<missing>" --json` used to emit `[{}]`
    // (a single phantom empty record) while the text-filter path emitted `[]`.
    // Both "no rows" outcomes must now be the identical canonical `[]`, so
    // `jq 'length'` / `len(json.loads(...))` see a genuinely empty list either
    // way. (The text path is already exact-pinned in browse_tag_json.)
    let s = Setup::new("emptytagfilter");
    let root = s.root.to_string_lossy().into_owned();
    write_page(&s, "a.md", "type: note\ntitle: A\ntags:\n  - foo\n", "body\n");
    let (code, _, _) = run(&s, &["add", &root]);
    assert_eq!(code, 0);

    // tag: filter with a value no page carries -> empty match-set branch.
    let (code, out, err) = run(&s, &["search", "tag:nobodyexists", "--json"]);
    assert_eq!(code, 0, "empty tag filter should succeed; stderr: {err}");
    assert_eq!(
        out.trim(),
        "[]",
        "empty `tag:` search --json must be exactly the canonical `[]`, got: {out:?}"
    );

    // And the plain-text no-match path must agree byte-for-byte.
    let (code, out, _) = run(&s, &["search", "zzz_no_match_term", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "[]",
        "no-match text search --json must be exactly the canonical `[]`, got: {out:?}"
    );
}

// ==================== F4 — bare-token dispatch ====================

#[test]
fn bare_token_containing_equals_routes_to_search() {
    // Regression for F4: a bare token that contains `=` (e.g. `E=mc2`) is a
    // legitimate single search term. Before the fix the `!a.contains('=')`
    // filter dropped it from the bare-token set, so the shortcut was skipped
    // and clap rejected the whole thing with "unrecognized subcommand 'E=mc2'".
    // It must route to `search` (which then reports an FTS-level error about
    // the `=` inside the query — *not* clap's "unrecognized subcommand").
    let s = Setup::new("bareeq");
    let root = s.root.to_string_lossy().into_owned();
    write_page(&s, "a.md", "type: note\ntitle: A\n", "body\n");
    let (code, _, _) = run(&s, &["add", &root]);
    assert_eq!(code, 0);

    // `E=mc2` is the only non-flag token -> bare path fires -> `search E=mc2`.
    // FTS chokes on the literal `=` ("syntax error near") — proof we reached
    // the search verb. The *wrong* outcome is clap's "unrecognized subcommand".
    let (_code, out, err) = run(&s, &["E=mc2"]);
    let combined = format!("{out}{err}");
    assert!(
        !combined.contains("unrecognized subcommand"),
        "bare `E=mc2` must not be rejected by clap as an unknown subcommand; got: {combined}"
    );
    // We must have been routed *to* search, not swallowed as a no-op.
    assert!(
        combined.to_lowercase().contains("search")
            || combined.contains("syntax error")
            || combined.contains("fts5"),
        "bare `E=mc2` should reach the search verb; got: {combined}"
    );
}
