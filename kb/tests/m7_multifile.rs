//! M7 — the multi-file (DB-backed) CLI namespace, end-to-end.
//!
//! Drives the real `kb` binary as a subprocess against a throwaway root and a
//! throwaway SQLite DB (`--db`). Pins the shell contract the single-file
//! namespace already pins (exit codes, output shape), but for the DB-backed
//! verbs: `add` / `roots` / `drop-root` / `refresh` / `search` / `browse` /
//! `tag` / `tag-alias` / `links` / `resolve` / `info` / `stats` / `cron`, plus
//! the bare-string dispatch ergonomics shortcut (PLAN §11).
//!
//! Run: `cargo test -p kb --test m7_multifile`

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn kb() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kb"))
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
    assert!(out.contains("indexed") || out.contains("root(s)"), "refresh note: {out}");
}

#[test]
fn roots_lists_registered_roots() {
    let s = Setup::new("roots");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "hi\n");
    add(&s);
    let (code, out, err) = run_bare(&s, &["roots"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains(s.root.to_str().unwrap()), "root path shown: {out}");
}

#[test]
fn drop_root_deregisters() {
    let s = Setup::new("droproot");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "hi\n");
    add(&s);
    let root = s.root.to_string_lossy().into_owned();
    let (code, out, err) = run(&s, &["drop-root", root.as_str()]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("dropped") || out.contains("dropped root"), "drop note: {out}");
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
    assert!(out.contains("1 root(s)") || out.contains("root(s)"), "summary: {out}");
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
    assert!(json.starts_with('[') && json.ends_with(']'), "array: {json}");
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
    let (code, out, err) = run_bare(
        &s,
        &["tag-alias", "--add", "models", "--canonical", "llm"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("llm"), "add echo: {out}");

    // searching tag:models self should expand to llm and hit the page
    let (code, out, err) = run(&s, &["search", "tag:models self"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Attention"), "alias-expanded tag match: {out}");
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
        &["tag-alias", "--add", "llm", "--canonical", "large language models"],
    );
    assert_eq!(code, 0, "stderr: {err}");

    // tag:"llm" — the quote makes `llm` a single token; canonicalize expands it
    // to "large language models" and the SQL join lands on the page.
    let (code, out, err) = run(&s, &["search", "tag:\"llm\""]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Large Language Models"), "multi-word alias hit: {out}");
    assert!(!out.contains("Unrelated"), "no false positive: {out}");

    // Also confirm the direct multi-word tag value filters correctly (no alias).
    let (code, out, err) = run(&s, &["search", "tag:\"large language models\""]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Large Language Models"), "direct tag match: {out}");
    assert!(!out.contains("Unrelated"), "no false positive direct: {out}");
}

#[test]
fn search_not_only_is_a_documented_limited_path() {
    let s = Setup::new("search_notonly");
    write_page(&s, "a.md", "type: note\ntitle: A\n", "alpha\n");
    write_page(&s, "b.md", "type: note\ntitle: B\n", "beta\n");
    add(&s);
    // A leading-`-` (NOT) term alone -> FTS5 can't take a lone NOT, so the
    // engine falls back to a full-scan MATCH-less query (documented limit).
    // `--` stops clap from treating the negative as a flag.
    let (code, _, err) = run(&s, &["search", "--", "-beta"]);
    assert_eq!(code, 0, "stderr: {err}");
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
    assert!(out.contains("Alpha") && out.contains("Beta"), "both pages: {out}");
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
    assert!(out.contains("one.md") && out.contains("two.md"), "label expanded: {out}");
}

// ---------------------------------------------------------------------------
// tag
// ---------------------------------------------------------------------------

#[test]
fn tag_lists_tagged_pages_and_expands_labels() {
    let s = Setup::new("tag");
    write_page(&s, "wiki/a.md", "type: wiki\ntitle: A\ntags:\n  - llm\n", "attention\n");
    write_page(&s, "wiki/b.md", "type: wiki\ntitle: B\ntags:\n  - other\n", "unrelated\n");
    add(&s);
    let (code, out, err) = run_bare(&s, &["tag", "llm"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("A"), "tagged page present: {out}");
    assert!(!out.contains("B  ") || !out.contains("wiki:B"), "untagged page absent: {out}");
}

#[test]
fn tag_alias_round_trip() {
    let s = Setup::new("tagalias");
    write_page(&s, "a.md", "type: note\ntitle: A\ntags:\n  - czech\n", "x\n");
    add(&s);
    // add
    let (code, out, err) = run_bare(&s, &["tag-alias", "--add", "Czechia", "--canonical", "czech"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Czechia") && out.contains("czech"), "add echo: {out}");
    // list
    let (code, out, err) = run_bare(&s, &["tag-alias", "--list"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Czechia") && out.contains("czech"), "list: {out}");
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
    write_page(&s, "a.md", "type: note\ntitle: A\ntags:\n  - czech\n", "x\n");
    add(&s);
    // two aliases (including a multi-word canonical) — the recoverable state.
    let (code, out, err) = run_bare(&s, &["tag-alias", "--add", "Czechia", "--canonical", "czech"]);
    assert_eq!(code, 0, "add 1: {err}");
    let _ = out;
    let (code, out, err) = run_bare(&s, &["tag-alias", "--add", "llms", "--canonical", "large language models"]);
    assert_eq!(code, 0, "add 2: {err}");
    let _ = out;

    // export to a recovery file.
    let backup = s.root.parent().unwrap().join("alias_backup.tsv");
    let (code, out, err) = run_bare(&s, &["tag-alias", "--export", "--file", backup.to_str().unwrap()]);
    assert_eq!(code, 0, "export: {err}");
    assert!(out.contains("exported") && out.contains("2"), "export echo: {out}");
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
    let (code, out, err) = run_bare(&s, &["tag-alias", "--import", "--file", backup.to_str().unwrap()]);
    assert_eq!(code, 0, "import: {err}");
    assert!(out.contains("imported 2"), "import echo: {out}");

    // the recovered rows are listed AND functional (the alias re-resolves).
    let (code, out, err) = run_bare(&s, &["tag-alias", "--list"]);
    assert_eq!(code, 0, "list after import: {err}");
    assert!(out.contains("Czechia") && out.contains("czech"), "recovered 1: {out}");
    assert!(out.contains("llms") && out.contains("large language models"), "recovered 2: {out}");
}

#[test]
fn tag_alias_export_empty_is_empty() {
    let s = Setup::new("tagalias_export_empty");
    write_page(&s, "a.md", "type: note\ntitle: A\ntags:\n  - czech\n", "x\n");
    add(&s);
    // no aliases added → export renders to the empty byte string.
    let (code, out, err) = run_bare(&s, &["tag-alias", "--export"]);
    assert_eq!(code, 0, "export empty: {err}");
    assert_eq!(out, "", "empty export must be empty (no lone newline): {out:?}");
    // and importing an empty backup is a no-op (wipe→reimport-empty = no rows).
    let empty = s.root.parent().unwrap().join("empty.tsv");
    fs::write(&empty, "").unwrap();
    let (code, out, err) = run_bare(&s, &["tag-alias", "--import", "--file", empty.to_str().unwrap()]);
    assert_eq!(code, 0, "import empty: {err}");
    assert!(out.contains("imported 0"), "imported 0: {out}");
    let (code, out, err) = run_bare(&s, &["tag-alias", "--list"]);
    assert_eq!(code, 0, "list: {err}");
    assert!(out.contains("no tag aliases"), "still empty: {out}");
}

#[test]
fn tag_alias_import_is_idempotent() {
    let s = Setup::new("tagalias_idempotent");
    write_page(&s, "a.md", "type: note\ntitle: A\ntags:\n  - czech\n", "x\n");
    add(&s);
    let (code, _, err) = run_bare(&s, &["tag-alias", "--add", "Czechia", "--canonical", "czech"]);
    assert_eq!(code, 0, "add: {err}");
    let (code, _, err) = run_bare(&s, &["tag-alias", "--add", "llms", "--canonical", "llm"]);
    assert_eq!(code, 0, "add: {err}");
    let backup = s.root.parent().unwrap().join("backup.tsv");
    let (code, _, err) = run_bare(&s, &["tag-alias", "--export", "--file", backup.to_str().unwrap()]);
    assert_eq!(code, 0, "export: {err}");
    // import twice — the second is a no-op (upsert onto the same rows).
    let (code, out, err) = run_bare(&s, &["tag-alias", "--import", "--file", backup.to_str().unwrap()]);
    assert_eq!(code, 0, "import 1: {err}");
    let _ = out;
    let (code, out, err) = run_bare(&s, &["tag-alias", "--import", "--file", backup.to_str().unwrap()]);
    assert_eq!(code, 0, "import 2: {err}");
    assert!(out.contains("imported 2"), "still loads 2 (idempotent): {out}");
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
    write_page(&s, "a.md", "type: note\ntitle: A\ntags:\n  - czech\n", "x\n");
    add(&s);
    // a partially-typed / hand-edited backup: two good rows, comment, blank,
    // a no-tab line, an empty-field line — the good rows must still load.
    let backup = s.root.parent().unwrap().join("backup.tsv");
    let txt = "# recovery backup\nCzechia\tczech\n\nnot-a-row\n\tmissing-alias\nllms\tllm\n";
    fs::write(&backup, txt).unwrap();
    let (code, out, err) = run_bare(&s, &["tag-alias", "--import", "--file", backup.to_str().unwrap()]);
    assert_eq!(code, 0, "import: {err}");
    assert!(out.contains("imported 2"), "load the 2 good rows: {out}");
    assert!(out.contains("2 line(s) skipped"), "report the 2 malformed: {out}");
    let (code, out, err) = run_bare(&s, &["tag-alias", "--list"]);
    assert_eq!(code, 0, "list: {err}");
    assert!(out.contains("Czechia") && out.contains("llms"), "both good rows present: {out}");
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
    let (code, out, err) = run(&s, &["links", "note:A"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("forward links"), "fwd header: {out}");
    assert!(out.contains("B"), "fwd target: {out}");
    // the backlinks view also renders (may be empty for a raw-text wikilink
    // target, but the header must be present — that's the shell contract).
    let (code, out, err) = run(&s, &["links", "note:B"]);
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
    assert!(out.contains(s.db.to_str().unwrap()), "db path present: {out}");
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
    assert!(!stdout.contains("wiki:") && stdout.contains("note:Alpha"), "info, not search: {stdout}");
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
    let (code, out, err) = run(&s, &["links", "note:B"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("backlinks (") || out.contains("backlinks (1)"),
        "B has exactly one backlink: {out}");
    assert!(out.contains("a.md") || out.contains("note:A"), "A appears as backlink: {out}");
}

/// A type-qualified resolve (`wiki:Attention`) matches the type-qualified
/// page and no bare-slug collision.
#[test]
fn resolve_type_qualified_unique() {
    let s = Setup::new("m7b_type_qualified");
    // Two pages with the same slug but different types — collision on the
    // bare slug; disambiguation requires a type-qualified id.
    write_page(&s, "w.md", "type: wiki\ntitle: Attention\n", "the wiki side\n");
    write_page(&s, "n.md", "type: note\ntitle: Attention\n", "the note side\n");
    add(&s);
    // bare slug → collision list
    let (code, out, err) = run(&s, &["resolve", "attention"]);
    assert_eq!(code, 0, "no error: stderr: {err}");
    assert!(out.contains("candidate"), "collision renders as candidates: {out}");
    assert!(out.contains("wiki:Attention"), "wiki candidate: {out}");
    assert!(out.contains("note:Attention"), "note candidate: {out}");
    // type-qualified → unique
    let (code, out, err) = run(&s, &["resolve", "wiki:attention"]);
    assert_eq!(code, 0, "no error: stderr: {err}");
    assert!(out.contains("wiki:Attention"), "resolved to wiki page: {out}");
    assert!(!out.contains("note:Attention"), "note page is excluded: {out}");
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
    assert!(!out.contains("candidate"), "unique hit, not a collision: {out}");
}

/// A substring that uniquely identifies a page resolves; one that doesn't is
/// a candidate list.
#[test]
fn resolve_substring_unique_and_ambiguous() {
    let s = Setup::new("m7b_substring");
    // "transformer-architecture" and "transformer-benchmarks" share the
    // substring "transformer" — ambiguous.
    write_page(&s, "arch.md", "type: wiki\ntitle: transformer-architecture\n", "x\n");
    write_page(&s, "bench.md", "type: wiki\ntitle: transformer-benchmarks\n", "y\n");
    add(&s);
    let (code, out, err) = run(&s, &["resolve", "transformer"]);
    assert_eq!(code, 0, "no error: stderr: {err}");
    assert!(out.contains("candidate"), "ambiguous: {out}");
    let (code, out, err) = run(&s, &["resolve", "transformer-architecture"]);
    assert_eq!(code, 0, "no error: stderr: {err}");
    assert!(out.contains("wiki:transformer-architecture"), "unique: {out}");
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
