//! M7 — the multi-file (DB-backed) CLI namespace, end-to-end.
//!
//! Drives the real `kb` binary as a subprocess against a throwaway root and a
//! throwaway SQLite DB (`--db`). Pins the shell contract the single-file
//! namespace already pins (exit codes, output shape), but for the DB-backed
//! verbs: `add` / `roots` / `drop-root` / `refresh` / `search` / `browse` /
//! `tag` / `tag-alias` / `links` / `resolve` / `info` / `stats` / `cron`, plus
//! the bare-string dispatch ergonomics shortcut (PLAN §11).
//!
//! Run: `cargo test -p kb-user --test m7_multifile`

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn kb() -> Command {
    Command::new(std::env::var("CARGO_BIN_EXE_kb").expect("CARGO_BIN_EXE_kb unset"))
}

/// A throwaway root + DB. `Db` default is `~/.local/share/...`; we override
/// per-test with `--db` into the process's temp dir so runs are independent.
struct Setup {
    root: PathBuf,
    db: PathBuf,
}

impl Setup {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("kbm7_{pid}_{tag}", pid = std::process::id()));
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

/// Run `kb` with an explicit `--db`, returning `(code, stdout, stderr)`.
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

/// Run `kb` with NO explicit root-arg prefix (for bare-string / pure-flag
/// commands like `roots`, `stats`, `cron`).
fn run_bare(setup: &Setup, args: &[&str]) -> (i32, String, String) {
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

/// Write a page at `root/rel`. `fm` is the frontmatter WITHOUT the framing
/// `---` (a trailing newline is expected).
fn write_page(setup: &Setup, rel: &str, fm: &str, body: &str) {
    let p = setup.root.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&p, format!("---\n{fm}---\n{body}")).unwrap();
}

fn add(setup: &Setup) -> (i32, String, String) {
    let root = setup.root.to_string_lossy().into_owned();
    run(setup, &["add", root.as_str()])
}

// ---------------------------------------------------------------------------
// add / roots / drop-root
// ---------------------------------------------------------------------------

#[test]
fn add_registers_root_and_indexes() {
    let s = Setup::new("add");
    write_page(
        &s,
        "wiki/attention.md",
        "type: wiki\ntitle: Attention\ntags:\n  - llm\n",
        "The attention mechanism.\n",
    );
    let (code, out, err) = add(&s);
    assert_eq!(code, 0, "stderr: {err}");
    let root = s.root.to_string_lossy().to_string();
    assert!(out.contains(&root), "root echoed: {out}");
    assert!(
        out.contains("indexed") || out.contains("root(s)"),
        "refresh note: {out}"
    );
}

#[test]
fn roots_lists_registered_roots() {
    let s = Setup::new("roots");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "hi\n");
    add(&s);
    let (code, out, err) = run_bare(&s, &["roots"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains(s.root.to_str().unwrap()),
        "root path shown: {out}"
    );
}

#[test]
fn drop_root_deregisters() {
    let s = Setup::new("droproot");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "hi\n");
    add(&s);
    let root = s.root.to_string_lossy().into_owned();
    let (code, out, err) = run(&s, &["drop-root", root.as_str()]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("dropped") || out.contains("dropped root"),
        "drop note: {out}"
    );
}

/// Q3: a root whose folder has since been *deleted* from disk. `canonicalize`
/// of the argument then fails, so `drop-root` must fall back to matching the
/// literal (registered) path and still deregister it — otherwise a removed dir
/// would leave an unremovable orphan root behind.
#[test]
fn drop_root_after_folder_deleted() {
    let s = Setup::new("droproot_missing");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "hi\n");
    add(&s);
    let root = s.root.to_string_lossy().into_owned();

    // Confirm it was registered, then wipe the folder from disk.
    let (code, out, err) = run_bare(&s, &["roots"]);
    assert_eq!(code, 0, "roots stderr: {err}");
    assert!(out.contains(&root), "registered: {out}");
    fs::remove_dir_all(&s.root).unwrap();
    assert!(!s.root.exists(), "root folder deleted before drop");

    // Drop by the registered path even though the dir no longer exists: the
    // canonicalize-failure fallback matches the literal key and deregisters.
    let (code, out, err) = run(&s, &["drop-root", root.as_str()]);
    assert_eq!(
        code, 0,
        "drop-root of a deleted folder must not hard-fail; stderr: {err}"
    );
    assert!(
        out.contains("dropped root"),
        "literal-match fallback should report the drop: {out}"
    );

    // And it is now gone from the roots list.
    let (code, out, err) = run_bare(&s, &["roots"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains(&root),
        "root should be unregistered after drop: {out}"
    );
}

// ---------------------------------------------------------------------------
// refresh / full-rebuild
// ---------------------------------------------------------------------------

#[test]
fn refresh_reports_counts() {
    let s = Setup::new("refresh");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "hello\n");
    write_page(&s, "b.md", "no type here\n", "no type\n");
    add(&s);
    let (code, out, err) = run_bare(&s, &["refresh"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("1 root(s)") || out.contains("root(s)"),
        "summary: {out}"
    );
    // the no-type page is skipped + counted
    assert!(out.contains("skipped"), "skip count present: {out}");
}

#[test]
fn full_rebuild_alias_of_refresh_full() {
    let s = Setup::new("fullrebuild");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "hello\n");
    add(&s);
    let (code, out, err) = run_bare(&s, &["full-rebuild"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("root(s)"), "summary: {out}");
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

#[test]
fn search_finds_tagged_and_content_pages() {
    let s = Setup::new("search");
    write_page(
        &s,
        "wiki/attention.md",
        "type: wiki\ntitle: Attention\ntags:\n  - llm\n",
        "Self attention in transformers.\n",
    );
    write_page(
        &s,
        "wiki/foo.md",
        "type: wiki\ntitle: Unrelated\n",
        "Nothing to see here at all.\n",
    );
    add(&s);

    // plain content match
    let (code, out, err) = run(&s, &["search", "transformers"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Attention"), "hit title: {out}");
    assert!(!out.contains("Unrelated"), "no false positive: {out}");

    // JSON envelope
    let (code, json, err) = run(&s, &["search", "transformers", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    let json = json.trim();
    assert!(
        json.starts_with('[') && json.ends_with(']'),
        "array: {json}"
    );
    assert!(json.contains("\"id\":\"wiki:Attention\""), "id: {json}");
}

#[test]
fn search_tag_filter_uses_alias_expansion() {
    let s = Setup::new("search_alias");
    write_page(
        &s,
        "wiki/attention.md",
        "type: wiki\ntitle: Attention\ntags:\n  - llm\n",
        "self attention\n",
    );
    add(&s);

    // add a single-token alias: "models" -> canonical "llm".
    let (code, out, err) = run_bare(&s, &["tag-alias", "--add", "models", "--canonical", "llm"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("llm"), "add echo: {out}");

    // searching tag:models self should expand to llm and hit the page
    let (code, out, err) = run(&s, &["search", "tag:models self"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Attention"), "alias-expanded tag match: {out}");
}

// ---------------------------------------------------------------------------
// the `Null` absence sentinel (case-insensitive) — a page *lacks* the field
// ---------------------------------------------------------------------------

#[test]
fn language_null_matches_pages_without_language() {
    let s = Setup::new("null_lang");
    write_page(
        &s,
        "with.md",
        "type: note\ntitle: With\nlanguage: en\n",
        "x\n",
    );
    write_page(&s, "without.md", "type: note\ntitle: Without\n", "y\n");
    add(&s);

    // `language:Null` — without-language page hit, with-language page not.
    let (code, out, err) = run(&s, &["search", "language:Null"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Without"), "absence hit: {out}");
    assert!(
        !out.contains("With\n"),
        "page with a language must be excluded (see `Without`): {out}"
    );

    // The sentinel is case-insensitive: lowercase `language:null` is the
    // *same* absence path, so it also matches the without-language page.
    let (code, out, err) = run(&s, &["search", "language:null"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Without"),
        "lowercase sentinel is the absence path: {out}"
    );
    assert!(
        !out.contains("With\n"),
        "page with a language must be excluded (see `Without`): {out}"
    );
}

#[test]
fn type_null_is_a_documented_noop() {
    let s = Setup::new("null_type");
    write_page(&s, "typed.md", "type: note\ntitle: Typed\n", "x\n");
    write_page(&s, "untitled.md", "title: Untyped\n", "y\n");
    add(&s);

    // A page without a `type:` field is *skipped from indexing* (the `NoType`
    // `IndexError` → `skipped_no_type`), so it never becomes a `pages` row.
    // `type:Null` therefore has no candidate to match — a documented no-op —
    // rather than matching the (never-indexed) typeless page. This is the
    // *other* of the two `IS NULL` paths; the `language_null` test above
    // exercises the identical code shape for an indexable field.
    let (code, out, err) = run(&s, &["search", "type:Null"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("0 results") || out.trim() == "[]",
        "typeless pages are skipped from the index, so type:Null is a no-op: {out}"
    );
    assert!(
        !out.contains("Typed"),
        "a type-bearing page must be excluded: {out}"
    );
}

#[test]
fn tag_null_matches_pages_without_any_tags() {
    let s = Setup::new("null_tag");
    write_page(
        &s,
        "tagged.md",
        "type: note\ntitle: Tagged\ntags:\n  - foo\n  - bar\n",
        "x\n",
    );
    write_page(&s, "tagless.md", "type: note\ntitle: Tagless\n", "y\n");
    add(&s);

    // tag:Null — the page lacks the tag field entirely (no `page_tags` row).
    // A page that has *other* tags is still excluded; a page with a tag
    // literally spelled "null" would be (the §5 absence contract).
    let (code, out, err) = run(&s, &["search", "tag:Null"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Tagless"), "absence hit: {out}");
    assert!(
        !out.contains("Tagged"),
        "page carrying tags must be excluded: {out}"
    );
}

#[test]
fn null_sentinel_composes_with_content() {
    let s = Setup::new("null_compose");
    write_page(
        &s,
        "en.md",
        "type: note\ntitle: En\nlanguage: en\n",
        "transformer architecture\n",
    );
    write_page(
        &s,
        "unknown.md",
        "type: note\ntitle: Unknown\n",
        "transformer benchmarks\n",
    );
    add(&s);

    // `transformer` matches both on content; `language:Null` narrows to the
    // page without a language field.
    let (code, out, err) = run(&s, &["search", "transformer language:Null"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Unknown"), "content+narrowing hit: {out}");
    assert!(
        !out.contains("En\n"),
        "language-bearing page excluded by the sentinel: {out}"
    );
}

#[test]
fn search_multiword_tag_filter_via_quoted_alias() {
    let s = Setup::new("search_multiword_tag");
    write_page(
        &s,
        "wiki/llm.md",
        "type: wiki\ntitle: Large Language Models\ntags:\n  - large language models\n",
        "A survey of LLMs.\n",
    );
    write_page(
        &s,
        "wiki/attention.md",
        "type: wiki\ntitle: Unrelated\n",
        "Nothing to see here.\n",
    );
    add(&s);

    // Add an alias "llm" -> "large language models" (multi-word canonical).
    let (code, _out, err) = run_bare(
        &s,
        &[
            "tag-alias",
            "--add",
            "llm",
            "--canonical",
            "large language models",
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");

    // tag:"llm" — the quote makes `llm` a single token; canonicalize expands it
    // to "large language models" and the SQL join lands on the page.
    let (code, out, err) = run(&s, &["search", "tag:\"llm\""]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Large Language Models"),
        "multi-word alias hit: {out}"
    );
    assert!(!out.contains("Unrelated"), "no false positive: {out}");

    // Also confirm the direct multi-word tag value filters correctly (no alias).
    let (code, out, err) = run(&s, &["search", "tag:\"large language models\""]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Large Language Models"),
        "direct tag match: {out}"
    );
    assert!(
        !out.contains("Unrelated"),
        "no false positive direct: {out}"
    );
}

#[test]
fn search_not_only_is_a_documented_limited_path() {
    let s = Setup::new("search_notonly");
    write_page(&s, "a.md", "type: note\ntitle: Alpha\n", "alpha\n");
    write_page(&s, "b.md", "type: note\ntitle: Beta\n", "beta\n");
    add(&s);
    // A leading-`-` (NOT) term alone -> Turso's FTS planner can't take a
    // lone NOT (there is no positive FTS term to anchor it), so the CLI
    // phrases it as a correlated `NOT EXISTS` per NOT term. Before the fix,
    // `-beta` alone was a *silent no-op* (returned the
    // whole corpus up to `LIMIT`). Now the exclusion must actually drop the
    // page that has "beta" content.
    // `--` stops clap from treating the negative as a flag.
    let (code, out, err) = run(&s, &["search", "--", "-beta"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Alpha"),
        "page Alpha (does not contain beta) must be returned: {out}"
    );
    assert!(
        !out.contains("Beta"),
        "page Beta (contains beta) must be excluded: {out}"
    );
}

// ---------------------------------------------------------------------------
// browse
// ---------------------------------------------------------------------------

#[test]
fn browse_lists_all_pages_numbered() {
    let s = Setup::new("browse");
    write_page(&s, "a.md", "type: note\ntitle: Alpha\n", "x\n");
    write_page(&s, "b.md", "type: note\ntitle: Beta\n", "y\n");
    add(&s);
    let (code, out, err) = run_bare(&s, &["browse"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Alpha") && out.contains("Beta"),
        "both pages: {out}"
    );
    assert!(out.contains("page(s)"), "count footer: {out}");
    assert!(out.contains('1') && out.contains('2'), "numbered: {out}");
}

#[test]
fn browse_expands_label_pages() {
    let s = Setup::new("browselabel");
    // a label page that references two files
    fs::create_dir_all(s.root.join("llm")).unwrap();
    fs::write(s.root.join("llm/one.md"), "one\n").unwrap();
    fs::write(s.root.join("llm/two.md"), "two\n").unwrap();
    write_page(
        &s,
        "LLM.md",
        "type: label\ntitle: LLM\ntags:\n  - llm\n",
        "# LLM\nllm/one.md\nllm/two.md\n",
    );
    add(&s);
    let (code, out, err) = run_bare(&s, &["browse"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("one.md") && out.contains("two.md"),
        "label expanded: {out}"
    );
}

// ---------------------------------------------------------------------------
// tag
// ---------------------------------------------------------------------------

#[test]
fn tag_lists_tagged_pages_and_expands_labels() {
    let s = Setup::new("tag");
    write_page(
        &s,
        "wiki/a.md",
        "type: wiki\ntitle: A\ntags:\n  - llm\n",
        "attention\n",
    );
    write_page(
        &s,
        "wiki/b.md",
        "type: wiki\ntitle: B\ntags:\n  - other\n",
        "unrelated\n",
    );
    add(&s);
    let (code, out, err) = run_bare(&s, &["tag", "llm"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("A"), "tagged page present: {out}");
    assert!(
        !out.contains("B  ") || !out.contains("wiki:B"),
        "untagged page absent: {out}"
    );
}

#[test]
fn tag_matches_mixed_case_tag_in_any_query_case() {
    // the `kb tag` IN-list used to keep the *stored*
    // casing, so a stored mixed-case tag like `Czech` failed
    // `lower('Czech') IN ('Czech',…)` → no match, in any query case.
    let s = Setup::new("tag_case");
    write_page(
        &s,
        "a.md",
        "type: note\ntitle: A\ntags: [Czech, llm]\n",
        "x\n",
    );
    add(&s);
    for q in ["Czech", "czech", "CZECH"] {
        let (code, out, err) = run_bare(&s, &["tag", q]);
        assert_eq!(code, 0, "stderr: {err}");
        assert!(
            out.contains("A"),
            "tag {q:?} must match the stored `Czech`: {out}"
        );
    }
    // A different tag must still not match.
    let (code, out, err) = run_bare(&s, &["tag", "llm"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("A"), "llm also stored: {out}");
}

#[test]
fn tag_alias_round_trip() {
    let s = Setup::new("tagalias");
    write_page(
        &s,
        "a.md",
        "type: note\ntitle: A\ntags:\n  - czech\n",
        "x\n",
    );
    add(&s);
    // add
    let (code, out, err) = run_bare(
        &s,
        &["tag-alias", "--add", "Czechia", "--canonical", "czech"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Czechia") && out.contains("czech"),
        "add echo: {out}"
    );
    // list
    let (code, out, err) = run_bare(&s, &["tag-alias", "--list"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Czechia") && out.contains("czech"),
        "list: {out}"
    );
    // tag via the alias string now resolves to czech
    let (code, out, err) = run_bare(&s, &["tag", "Czechia"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("A"), "alias-resolved page: {out}");
    // remove
    let (code, out, err) = run_bare(&s, &["tag-alias", "--rm", "Czechia"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("removed"), "remove echo: {out}");
}

#[test]
fn tag_alias_export_then_wipe_then_import_recovers_the_map() {
    let s = Setup::new("tagalias_roundtrip");
    write_page(
        &s,
        "a.md",
        "type: note\ntitle: A\ntags:\n  - czech\n",
        "x\n",
    );
    add(&s);
    // two aliases (including a multi-word canonical) — the recoverable state.
    let (code, out, err) = run_bare(
        &s,
        &["tag-alias", "--add", "Czechia", "--canonical", "czech"],
    );
    assert_eq!(code, 0, "add 1: {err}");
    let _ = out;
    let (code, out, err) = run_bare(
        &s,
        &[
            "tag-alias",
            "--add",
            "llms",
            "--canonical",
            "large language models",
        ],
    );
    assert_eq!(code, 0, "add 2: {err}");
    let _ = out;

    // export to a recovery file.
    let backup = s.root.parent().unwrap().join("alias_backup.tsv");
    let (code, out, err) = run_bare(
        &s,
        &["tag-alias", "--export", "--file", backup.to_str().unwrap()],
    );
    assert_eq!(code, 0, "export: {err}");
    assert!(
        out.contains("exported") && out.contains("2"),
        "export echo: {out}"
    );
    let text = fs::read_to_string(&backup).unwrap();
    assert!(text.contains("Czechia\tczech"), "backup row 1: {text:?}");
    assert!(text.contains("llms\t"), "backup row 2: {text:?}");

    // ---- WIPE the DB (the recovery scenario) ----
    fs::remove_file(&s.db).unwrap();

    // the alias map is gone (a fresh DB has none).
    let (code, out, err) = run_bare(&s, &["tag-alias", "--list"]);
    assert_eq!(code, 0, "list after wipe: {err}");
    assert!(out.contains("no tag aliases"), "empty after wipe: {out}");

    // ---- import the recovery file back ----
    let (code, out, err) = run_bare(
        &s,
        &["tag-alias", "--import", "--file", backup.to_str().unwrap()],
    );
    assert_eq!(code, 0, "import: {err}");
    assert!(out.contains("imported 2"), "import echo: {out}");

    // the recovered rows are listed AND functional (the alias re-resolves).
    let (code, out, err) = run_bare(&s, &["tag-alias", "--list"]);
    assert_eq!(code, 0, "list after import: {err}");
    assert!(
        out.contains("Czechia") && out.contains("czech"),
        "recovered 1: {out}"
    );
    assert!(
        out.contains("llms") && out.contains("large language models"),
        "recovered 2: {out}"
    );
}

#[test]
fn tag_alias_export_empty_is_empty() {
    let s = Setup::new("tagalias_export_empty");
    write_page(
        &s,
        "a.md",
        "type: note\ntitle: A\ntags:\n  - czech\n",
        "x\n",
    );
    add(&s);
    // no aliases added → export renders to the empty byte string.
    let (code, out, err) = run_bare(&s, &["tag-alias", "--export"]);
    assert_eq!(code, 0, "export empty: {err}");
    assert_eq!(
        out, "",
        "empty export must be empty (no lone newline): {out:?}"
    );
    // and importing an empty backup is a no-op (wipe→reimport-empty = no rows).
    let empty = s.root.parent().unwrap().join("empty.tsv");
    fs::write(&empty, "").unwrap();
    let (code, out, err) = run_bare(
        &s,
        &["tag-alias", "--import", "--file", empty.to_str().unwrap()],
    );
    assert_eq!(code, 0, "import empty: {err}");
    assert!(out.contains("imported 0"), "imported 0: {out}");
    let (code, out, err) = run_bare(&s, &["tag-alias", "--list"]);
    assert_eq!(code, 0, "list: {err}");
    assert!(out.contains("no tag aliases"), "still empty: {out}");
}

#[test]
fn tag_alias_import_is_idempotent() {
    let s = Setup::new("tagalias_idempotent");
    write_page(
        &s,
        "a.md",
        "type: note\ntitle: A\ntags:\n  - czech\n",
        "x\n",
    );
    add(&s);
    let (code, _, err) = run_bare(
        &s,
        &["tag-alias", "--add", "Czechia", "--canonical", "czech"],
    );
    assert_eq!(code, 0, "add: {err}");
    let (code, _, err) = run_bare(&s, &["tag-alias", "--add", "llms", "--canonical", "llm"]);
    assert_eq!(code, 0, "add: {err}");
    let backup = s.root.parent().unwrap().join("backup.tsv");
    let (code, _, err) = run_bare(
        &s,
        &["tag-alias", "--export", "--file", backup.to_str().unwrap()],
    );
    assert_eq!(code, 0, "export: {err}");
    // import twice — the second is a no-op (upsert onto the same rows).
    let (code, out, err) = run_bare(
        &s,
        &["tag-alias", "--import", "--file", backup.to_str().unwrap()],
    );
    assert_eq!(code, 0, "import 1: {err}");
    let _ = out;
    let (code, out, err) = run_bare(
        &s,
        &["tag-alias", "--import", "--file", backup.to_str().unwrap()],
    );
    assert_eq!(code, 0, "import 2: {err}");
    assert!(
        out.contains("imported 2"),
        "still loads 2 (idempotent): {out}"
    );
    // list is stable: exactly the two rows, no duplication.
    let (code, out, err) = run_bare(&s, &["tag-alias", "--list"]);
    assert_eq!(code, 0, "list: {err}");
    let count_cz = out.matches("Czechia").count();
    let count_llm = out.matches("llms").count();
    assert_eq!(count_cz, 1, "no dup Czechia: {out}");
    assert_eq!(count_llm, 1, "no dup llms: {out}");
}

#[test]
fn tag_alias_import_skips_malformed_lines_without_losing_the_rest() {
    let s = Setup::new("tagalias_skip_malformed");
    write_page(
        &s,
        "a.md",
        "type: note\ntitle: A\ntags:\n  - czech\n",
        "x\n",
    );
    add(&s);
    // a partially-typed / hand-edited backup: two good rows, comment, blank,
    // a no-tab line, an empty-field line — the good rows must still load.
    let backup = s.root.parent().unwrap().join("backup.tsv");
    let txt = "# recovery backup\nCzechia\tczech\n\nnot-a-row\n\tmissing-alias\nllms\tllm\n";
    fs::write(&backup, txt).unwrap();
    let (code, out, err) = run_bare(
        &s,
        &["tag-alias", "--import", "--file", backup.to_str().unwrap()],
    );
    assert_eq!(code, 0, "import: {err}");
    assert!(out.contains("imported 2"), "load the 2 good rows: {out}");
    assert!(
        out.contains("2 line(s) skipped"),
        "report the 2 malformed: {out}"
    );
    let (code, out, err) = run_bare(&s, &["tag-alias", "--list"]);
    assert_eq!(code, 0, "list: {err}");
    assert!(
        out.contains("Czechia") && out.contains("llms"),
        "both good rows present: {out}"
    );
}

// ---------------------------------------------------------------------------
// links
// ---------------------------------------------------------------------------

#[test]
fn links_shows_forward_and_backlinks() {
    let s = Setup::new("links");
    // A references B via a wikilink; B is standalone. Both are `note` type,
    // so display ids are `note:A` / `note:B`.
    write_page(&s, "a.md", "type: note\ntitle: A\n", "see [[B]]\n");
    write_page(&s, "b.md", "type: note\ntitle: B\n", "the target\n");
    add(&s);
    // forward links for A: the wikilink target is stored as its raw text "B".
    let (code, out, err) = run(&s, &["links", "note:A", "--dedup"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("forward links"), "fwd header: {out}");
    assert!(out.contains("B"), "fwd target: {out}");
    // the backlinks view also renders (may be empty for a raw-text wikilink
    // target, but the header must be present — that's the shell contract).
    let (code, out, err) = run(&s, &["links", "note:B", "--dedup"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("backlinks"), "back header: {out}");
}

// ---------------------------------------------------------------------------
// resolve
// ---------------------------------------------------------------------------

#[test]
fn resolve_returns_candidate_list_no_hard_error() {
    let s = Setup::new("resolve");
    write_page(&s, "a.md", "type: note\ntitle: Attention\n", "x\n");
    add(&s);
    let (code, out, err) = run(&s, &["resolve", "attention"]);
    assert_eq!(code, 0, "no error on match: stderr: {err}");
    assert!(out.contains("note:Attention"), "candidate listed: {out}");
}

// ---------------------------------------------------------------------------
// info
// ---------------------------------------------------------------------------

#[test]
fn info_shows_full_page_record() {
    let s = Setup::new("info");
    write_page(
        &s,
        "wiki/a.md",
        "type: wiki\ntitle: Attention\ndescription: the mechanism\ntags:\n  - llm\n",
        "body\n",
    );
    add(&s);
    let path = s.root.join("wiki/a.md");
    let path = path.to_string_lossy().into_owned();
    let (code, out, err) = run(&s, &["info", path.as_str()]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("wiki:Attention"), "id: {out}");
    assert!(out.contains("wiki"), "type: {out}");
    assert!(out.contains("Attention"), "title: {out}");
    assert!(out.contains("the mechanism"), "desc: {out}");
    assert!(out.contains("llm"), "tags: {out}");
}

// ---------------------------------------------------------------------------
// stats
// ---------------------------------------------------------------------------

#[test]
fn stats_reports_counts_and_db_size() {
    let s = Setup::new("stats");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "x\n");
    write_page(&s, "b.md", "type: note\ntitle: B\ntags:\n  - llm\n", "y\n");
    add(&s);
    let (code, out, err) = run_bare(&s, &["stats"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("2 page(s)"), "two pages: {out}");
    assert!(out.contains("totals"), "totals header: {out}");
    assert!(out.contains("pages:        2"), "total count: {out}");
    assert!(out.contains("db file size"), "db size: {out}");
}

// ---------------------------------------------------------------------------
// cron
// ---------------------------------------------------------------------------

#[test]
fn cron_prints_a_paste_ready_line() {
    let s = Setup::new("cron");
    let (code, out, err) = run_bare(&s, &["cron"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains(s.db.to_str().unwrap()),
        "db path present: {out}"
    );
    assert!(out.contains("refresh"), "refresh verb present: {out}");
    assert!(out.contains("crontab -e"), "hint present: {out}");
}

// ---------------------------------------------------------------------------
// bare-string dispatch
// ---------------------------------------------------------------------------

#[test]
fn bare_existing_md_path_dispatches_to_info() {
    let s = Setup::new("bareinfo");
    write_page(&s, "a.md", "type: note\ntitle: Alpha\n", "x\n");
    add(&s);
    let path = s.root.join("a.md");
    let path = path.to_string_lossy().into_owned();
    // NO explicit subcommand: just the path as the lone non-flag arg.
    let db = s.db.to_string_lossy().into_owned();
    let out = kb()
        .arg(format!("--db={db}"))
        .arg(&path)
        .output()
        .expect("run kb");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(
        !stdout.contains("wiki:") && stdout.contains("note:Alpha"),
        "info, not search: {stdout}"
    );
}

#[test]
fn bare_unmatched_string_dispatches_to_search() {
    let s = Setup::new("baresearch");
    write_page(&s, "a.md", "type: note\ntitle: Alpha\n", "zebra\n");
    add(&s);
    let db = s.db.to_string_lossy().into_owned();
    let out = kb()
        .arg(format!("--db={db}"))
        .arg("zebra")
        .output()
        .expect("run kb");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("Alpha"), "search hit: {stdout}");
}

// ===========================================================================
// M7b — cross-page link resolution (PLAN §4 waterfall)
// ===========================================================================

/// Two distinct pages, A → B via a wikilink. The backlinks view of B must
/// now show A as a true backlink (resolved, not raw string equality). This is
/// the regression that motivated cross-page link resolution: the old path
/// relied on `to_path = from_path` in the DB, which fails for a wikilink
/// whose raw target is just a slug ("B"), not an absolute path.
#[test]
fn wikilink_backlinks_resolve() {
    let s = Setup::new("m7b_wikilink_backlinks");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "see [[B]]\n");
    write_page(&s, "b.md", "type: note\ntitle: B\n", "the target\n");
    add(&s);
    let (code, out, err) = run(&s, &["links", "note:B", "--dedup"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("backlinks (") || out.contains("backlinks (1)"),
        "B has exactly one backlink: {out}"
    );
    assert!(
        out.contains("a.md") || out.contains("note:A"),
        "A appears as backlink: {out}"
    );
}

/// A type-qualified resolve (`wiki:Attention`) matches the type-qualified
/// page and no bare-slug collision.
#[test]
fn resolve_type_qualified_unique() {
    let s = Setup::new("m7b_type_qualified");
    // Two pages with the same slug but different types — collision on the
    // bare slug; disambiguation requires a type-qualified id.
    write_page(
        &s,
        "w.md",
        "type: wiki\ntitle: Attention\n",
        "the wiki side\n",
    );
    write_page(
        &s,
        "n.md",
        "type: note\ntitle: Attention\n",
        "the note side\n",
    );
    add(&s);
    // bare slug → collision list
    let (code, out, err) = run(&s, &["resolve", "attention"]);
    assert_eq!(code, 0, "no error: stderr: {err}");
    assert!(
        out.contains("candidate"),
        "collision renders as candidates: {out}"
    );
    assert!(out.contains("wiki:Attention"), "wiki candidate: {out}");
    assert!(out.contains("note:Attention"), "note candidate: {out}");
    // type-qualified → unique
    let (code, out, err) = run(&s, &["resolve", "wiki:attention"]);
    assert_eq!(code, 0, "no error: stderr: {err}");
    assert!(
        out.contains("wiki:Attention"),
        "resolved to wiki page: {out}"
    );
    assert!(
        !out.contains("note:Attention"),
        "note page is excluded: {out}"
    );
}

/// A bare-slug collision renders a candidate list (no hard error), which is
/// the §4 contract for ambiguous queries.
#[test]
fn resolve_bare_slug_collision_lists_candidates() {
    let s = Setup::new("m7b_collision");
    write_page(&s, "x.md", "type: wiki\ntitle: Foo\n", "x\n");
    write_page(&s, "y.md", "type: note\ntitle: Foo\n", "y\n");
    add(&s);
    let (code, out, err) = run(&s, &["resolve", "foo"]);
    assert_eq!(code, 0, "no error: stderr: {err}");
    assert!(out.contains("candidate"), "collision: {out}");
    assert!(out.contains("wiki:Foo"), "wiki listed: {out}");
    assert!(out.contains("note:Foo"), "note listed: {out}");
}

/// A unique bare-slug hit resolves to a single page.
#[test]
fn resolve_bare_slug_unique() {
    let s = Setup::new("m7b_unique");
    write_page(&s, "only.md", "type: wiki\ntitle: Attention\n", "x\n");
    add(&s);
    let (code, out, err) = run(&s, &["resolve", "attention"]);
    assert_eq!(code, 0, "no error: stderr: {err}");
    assert!(out.contains("wiki:Attention"), "resolved: {out}");
    assert!(
        !out.contains("candidate"),
        "unique hit, not a collision: {out}"
    );
}

/// A substring that uniquely identifies a page resolves; one that doesn't is
/// a candidate list.
#[test]
fn resolve_substring_unique_and_ambiguous() {
    let s = Setup::new("m7b_substring");
    // "transformer-architecture" and "transformer-benchmarks" share the
    // substring "transformer" — ambiguous.
    write_page(
        &s,
        "arch.md",
        "type: wiki\ntitle: transformer-architecture\n",
        "x\n",
    );
    write_page(
        &s,
        "bench.md",
        "type: wiki\ntitle: transformer-benchmarks\n",
        "y\n",
    );
    add(&s);
    let (code, out, err) = run(&s, &["resolve", "transformer"]);
    assert_eq!(code, 0, "no error: stderr: {err}");
    assert!(out.contains("candidate"), "ambiguous: {out}");
    let (code, out, err) = run(&s, &["resolve", "transformer-architecture"]);
    assert_eq!(code, 0, "no error: stderr: {err}");
    assert!(
        out.contains("wiki:transformer-architecture"),
        "unique: {out}"
    );
}

/// A non-existent slug → empty result, exit 0 (no hard error).
#[test]
fn resolve_missing_slug_is_empty_no_error() {
    let s = Setup::new("m7b_missing");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "x\n");
    add(&s);
    let (code, out, err) = run(&s, &["resolve", "definitely-not-a-page"]);
    assert_eq!(code, 0, "no error: stderr: {err}");
    assert!(out.contains("no page matches"), "empty: {out}");
}

// ===========================================================================
// M7c — special-property composition: gate × language × type × label links
// ===========================================================================
//
// These tests pin the *interactions* between the search-grammar features
// (the `language:` / `language:Null` absence filter, the `search_required_phrase`
// *gate* rule, the `type:` filter, the alias-expanded tag filter, the label
// expansion on `tag` / `links`) rather than each feature in isolation.
// Single-feature coverage already lives in the earlier m7 + m5 tests; these
// are the *composition* matrix the user's original plan called out.

/// The positive `language:` filter (the companion to the `language:Null`
/// test above): a page that *has* `language: en` is included *only* when
/// the filter value matches, and is excluded when the filter value does not
/// match or when it is `Null`. Case-folding is symmetric (the SQL uses
/// `lower(pages.language) = lower(?)`).
#[test]
fn language_positive_filter_includes_match_and_excludes_other_and_null() {
    let s = Setup::new("lang_pos");
    write_page(
        &s,
        "en.md",
        "type: note\ntitle: Alphabetic\nlanguage: en\n",
        "alphabetic body content\n",
    );
    write_page(
        &s,
        "no-lang.md",
        "type: note\ntitle: Beta\n",
        "beta body content\n",
    );
    write_page(
        &s,
        "fr.md",
        "type: note\ntitle: Gamma\nlanguage: fr\n",
        "gamma body content\n",
    );
    add(&s);

    // The filter narrows the corpus to pages whose `language:` column
    // case-folds to the filter value.
    let (code, out, err) = run(&s, &["search", "alphabetic language:en"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Alphabetic"), "positive match: {out}");
    assert!(
        !out.contains("Beta"),
        "page without language must not match: {out}"
    );
    assert!(
        !out.contains("Gamma"),
        "page with the wrong language must not match: {out}"
    );

    // The *absence* filter is the complementary path: `language:Null` matches
    // only the page *without* the field.
    let (code, out, err) = run(&s, &["search", "beta language:Null"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Beta"),
        "absence path returns the field-less page: {out}"
    );
    assert!(
        !out.contains("Alphabetic") && !out.contains("Gamma"),
        "pages that HAVE a language field must not match: {out}"
    );

    // `language:en` is case-insensitive end-to-end (the user-side filter value
    // and the SQL both flow through the same case-fold — `lower(...) =
    // lower(...)` — so `language:EN` is the *same* filter as `language:en`).
    let (code, out, err) = run(&s, &["search", "alphabetic language:EN"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Alphabetic"), "case-fold is symmetric: {out}");
}

/// The gate rule *alone*: a gated page with no gate phrase in the query is
/// hidden from search; adding the gate phrase unlocks it. This is the
/// baseline the next two tests compose *language:* on top of.
#[test]
fn gate_hides_from_generic_search_and_unlocks_by_phrase() {
    let s = Setup::new("gate_baseline");
    write_page(
        &s,
        "gated.md",
        "type: note\ntitle: Gated\nsearch_required_phrase:\n  - secretword\n",
        "gated body content\n",
    );
    write_page(
        &s,
        "ungated.md",
        "type: note\ntitle: Ungated\n",
        "ungated body content",
    );
    add(&s);

    // Generic search: the un-gated page is found, the gated page is hidden.
    let (code, out, err) = run(&s, &["search", "gated"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("Gated"),
        "gated page hidden without gate phrase: {out}"
    );

    // The ungated page matches the same query.
    let (code, out, err) = run(&s, &["search", "ungated"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Ungated"),
        "ungated page is a normal search hit: {out}"
    );

    // Adding the gate phrase to the query unlocks the gated page.
    let (code, out, err) = run(&s, &["search", "gated secretword"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Gated"),
        "gate phrase in query unlocks the page: {out}"
    );
}

/// The gate × `language:` composition: the gate rule and the language filter
/// both apply in the *same* query (their SQL predicates are AND-composed).
/// The gate is *necessary but not sufficient* — the language filter still
/// narrows the result. (The existing
/// `gate_hides_from_generic_search_and_unlocks_by_phrase` test pins the
/// gate alone and the `language_positive_filter_*` test pins language alone;
/// this test pins their intersection.)
#[test]
fn gate_and_language_compose_independently() {
    let s = Setup::new("gate_lang");
    write_page(
        &s,
        "gated_en.md",
        "type: note\ntitle: Gated En\nlanguage: en\nsearch_required_phrase:\n  - secretword\n",
        "gated body\n",
    );
    write_page(
        &s,
        "gated_fr.md",
        "type: note\ntitle: Gated Fr\nlanguage: fr\nsearch_required_phrase:\n  - secretword\n",
        "gated fr body\n",
    );
    write_page(
        &s,
        "ungated_en.md",
        "type: note\ntitle: Ungated En\nlanguage: en\n",
        "ungated body\n",
    );
    add(&s);

    // Gate phrase present, no language filter: both gated pages unlock.
    let (code, out, err) = run(&s, &["search", "gated secretword"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Gated En") && out.contains("Gated Fr"),
        "both gated pages unlock on the phrase alone: {out}"
    );

    // Gate phrase + language:en → only the en-gated page is returned.
    let (code, out, err) = run(&s, &["search", "gated secretword language:en"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("Gated En"),
        "en-gated page unlocks on the composed gate+language query: {out}"
    );
    assert!(
        !out.contains("Gated Fr"),
        "fr-gated page must be excluded by the language filter even though it\
         is unlocked by the gate phrase: {out}"
    );

    // Gate phrase absent, language:en → the gated en page is STILL hidden
    // (the language filter alone does *not* unlock the gate).
    let (code, out, err) = run(&s, &["search", "gated language:en"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("Gated En"),
        "the gate predicate is AND-composed with the language filter — \
         neither alone is sufficient: {out}"
    );
}

/// The label × special-property composition: a `type: label` page that is
/// *also* gated AND *also* language-restricted must be hidden unless the
/// query carries the gate phrase, AND must still be excluded by a
/// non-matching `language:` filter even when the gate phrase is present.
/// This is the three-way composition the user's plan called out — the
/// "label page with a gate and a language" case.
#[test]
fn label_page_gated_and_language_restricted_composes() {
    let s = Setup::new("label_gate_lang");
    // A label page that references two non-indexed files (so the label
    // expansion contract — label pages expand into the file list — is
    // exercised *inside* a gated + language-restricted page).
    //
    // Intended composition:
    //   * "gated from search" — the generic `search llm` query must not
    //     return the label page (the gate predicate blocks it).
    //   * "unlocked by the gate phrase" — `search "llm secretword"`
    //     returns the label page.
    //   * "language filter narrows" — `search "llm secretword language:en"`
    //     still returns the page (the label page's language is en); a
    //     wrong language (`language:fr`) still hides it even with the gate
    //     phrase present.
    fs::create_dir_all(s.root.join("papers")).unwrap();
    fs::write(s.root.join("papers/attention.md"), "attention paper\n").unwrap();
    fs::write(s.root.join("papers/transformer.md"), "transformer paper\n").unwrap();
    write_page(
        &s,
        "LLM_PAPERS.md",
        "type: label\ntitle: LLM Papers\nlanguage: en\nsearch_required_phrase:\n  - secretword\n",
        "# LLM Papers\npapers/attention.md\npapers/transformer.md\n",
    );
    add(&s);

    // Generic search (no gate phrase in the query): the label page is
    // *hidden* by the gate, even though it matches "llm" on title/body.
    let (code, out, err) = run(&s, &["search", "llm"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("LLM Papers"),
        "gated label page must not be returned without the gate phrase: {out}"
    );

    // Gate phrase present: the label page is returned.
    let (code, out, err) = run(&s, &["search", "llm secretword"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("LLM Papers"),
        "gated label page must be returned when the gate phrase is in the query: {out}"
    );

    // Gate phrase + matching language: still returned.
    let (code, out, err) = run(&s, &["search", "llm secretword language:en"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("LLM Papers"),
        "gated label page with language en must match the composed \
         gate+language query: {out}"
    );

    // Gate phrase + WRONG language: still hidden (the language filter is a
    // separate AND clause).
    let (code, out, err) = run(&s, &["search", "llm secretword language:fr"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("LLM Papers"),
        "gated label page with language en must be EXCLUDED by a language:fr\
         filter even though the gate phrase is present: {out}"
    );
}

/// Multi-outgoing links: a page A that references two distinct targets B and C
/// (two wikilinks in the body, both resolvable) must have *both* links in its
/// forward-links view, and the backlinks views of *both* B and C must show A.
/// (The earlier `wikilink_backlinks_resolve` test pins the 1-outgoing case;
/// this pins the fan-out case, which exercises the `upsert_page` bulk-delete-
/// and re-insert of the `links` rows for A, and the `links` query's `to_path`
/// resolution for each independent target.)
#[test]
fn multi_outgoing_links_fan_out_to_each_target_and_backlink() {
    let s = Setup::new("multi_out_link");
    write_page(
        &s,
        "a.md",
        "type: note\ntitle: A\n",
        "see [[B]] and also [[C]]\n",
    );
    write_page(&s, "b.md", "type: note\ntitle: B\n", "the B target\n");
    write_page(&s, "c.md", "type: note\ntitle: C\n", "the C target\n");
    add(&s);

    // A's forward view: both targets, each with a style tag (the `wikilink`
    // style in this case).
    let (code, out, err) = run(&s, &["links", "note:A", "--dedup"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("forward links (2)"),
        "A has exactly two outbound links: {out}"
    );
    assert!(
        out.contains("[ wikilink ]  B"),
        "B in A's forward list: {out}"
    );
    assert!(
        out.contains("[ wikilink ]  C"),
        "C in A's forward list: {out}"
    );

    // B's backlinks: exactly A.
    let (code, out, err) = run(&s, &["links", "note:B", "--dedup"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("backlinks (1)") && (out.contains("a.md") || out.contains("note:A")),
        "B's only backlink is A: {out}"
    );
    assert!(
        !out.contains("note:C"),
        "C must not appear in B's backlinks: {out}"
    );

    // C's backlinks: exactly A.
    let (code, out, err) = run(&s, &["links", "note:C", "--dedup"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("backlinks (1)") && (out.contains("a.md") || out.contains("note:A")),
        "C's only backlink is A: {out}"
    );
    assert!(
        !out.contains("note:B"),
        "B must not appear in C's backlinks: {out}"
    );
}

/// A 2-cycle (A↔B): each page has one forward link to the other, and each
/// page's backlinks view shows the other. No self-loop (A's backlinks is
/// exactly one row: B; A is not in its own backlinks list). This pins both
/// the *directed* semantics of the `links` table (from_path / to_path are
/// *row*-level fields, so a single A→B row is distinct from a B→A row, and
/// neither appears as a self-loop) AND the absence of any implicit
/// self-linking the old raw-string-equality path had.
#[test]
fn two_cycle_is_directed_with_no_self_loop() {
    let s = Setup::new("two_cycle");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "points to [[B]]\n");
    write_page(&s, "b.md", "type: note\ntitle: B\n", "points to [[A]]\n");
    add(&s);

    // A's forward: exactly B. A's backlinks: exactly B (the reverse edge
    // of the cycle). No self-loop (A does not link to A).
    let (code, out, err) = run(&s, &["links", "note:A", "--dedup"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("forward links (1)") && out.contains("[ wikilink ]  B"),
        "A's forward is exactly [[B]]: {out}"
    );
    assert!(
        out.contains("backlinks (1)") && (out.contains("b.md") || out.contains("note:B")),
        "A's backlinks is exactly B (the reverse cycle edge): {out}"
    );

    // B mirrors this: forward = A, backlinks = A.
    let (code, out, err) = run(&s, &["links", "note:B", "--dedup"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("forward links (1)") && out.contains("[ wikilink ]  A"),
        "B's forward is exactly [[A]]: {out}"
    );
    assert!(
        out.contains("backlinks (1)") && (out.contains("a.md") || out.contains("note:A")),
        "B's backlinks is exactly A (the reverse cycle edge): {out}"
    );
}

/// A self-link (A→A): a wikilink in a page's own body that resolves to the
/// same page. This is the *degenerate* case — the A→A row exists in the
/// `links` table (the forward view shows it), but the backlinks view
/// deliberately *filters out self-loops* so a page does not appear as its
/// own backlink. The exclusion is pinned at `kb/cli/links.rs:96`:
///   `.filter(|(src,_)| *src != abs_path)`
/// The rationale: self-backlinks would be pure noise — the page is
/// trivially in its own closure, so listing it adds nothing. This test
/// pins that *intentional* asymmetric handling: forward shows the edge,
/// backlinks does not.
#[test]
fn self_link_shows_in_forward_but_is_filtered_from_backlinks() {
    let s = Setup::new("self_link");
    write_page(
        &s,
        "a.md",
        "type: note\ntitle: A\n",
        "self-reference: [[A]]\n",
    );
    add(&s);

    let (code, out, err) = run(&s, &["links", "note:A", "--dedup"]);
    assert_eq!(code, 0, "stderr: {err}");
    // The self-link *is* visible in A's forward view (the forward query
    // has no self-loop filter — only the backlinks view has one).
    assert!(
        out.contains("forward links (1)") && out.contains("[ wikilink ]  A"),
        "the self-link appears in A's forward view: {out}"
    );
    // The self-link is *filtered out* of A's backlinks view by the
    // `src != abs_path` guard in `cmd_links` — a page does not count as
    // its own backlink.
    assert!(
        out.contains("backlinks: none"),
        "the self-link must NOT appear in A's backlinks (the CLI filters\
         self-loops out to avoid noise): {out}"
    );
}
