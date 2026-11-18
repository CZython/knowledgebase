//! End-to-end tests for `kb quick-search` / `kb qs` (the §v3 section-search
//! surface). Drives the real `kb` binary as a subprocess over a throwaway
//! root + DB and pins the *shell* contract: the single / ambiguous / empty
//! result shapes, `--force` top-1, and the `--json` envelope. The pure SQL
//! path (`section_search_sql` → live `sections` FTS read) is otherwise
//! untested — these tests exercise it for real against `add` → `qs`.
//!
//! A key regression here is the "nothing in the DB" case the user hit: a
//! live database whose `pages` rows predate the `sections` table returns
//! nothing from `qs`. That is fixed by the `backfill_sections` step in
//! `Db::migrate`; these tests prove `add` (which indexes with section rows)
//! and every subsequent `qs` hit the index.

use std::path::PathBuf;
use std::process::Command;

fn kb() -> Command {
    Command::new(
        std::env::var("CARGO_BIN_EXE_kb").expect("CARGO_BIN_EXE_kb unset — run under cargo test"),
    )
}

/// Throwaway root + throwaway DB (independent of the user's `~/.local/...`
/// DB), mirroring the `T6` harness in `section_cli.rs`. `--db` is a global
/// flag, so it is prepended before the subcommand + its flags.
struct Qs {
    root: PathBuf,
    db: PathBuf,
}
impl Qs {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("kb_qs_{pid}_{tag}", pid = std::process::id()));
        let root = base.join("root");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let db = base.join("kb.db");
        let _ = std::fs::remove_file(&db);
        Qs { root, db }
    }
    /// Run `kb --db <db> <args…>` and return `(code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let db = self.db.to_string_lossy().into_owned();
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
    /// Write a `type: note` page at `rel` whose body is `# <title>` + `body`.
    fn page(&self, rel: &str, title: &str, body: &str) {
        let content = format!("---\ntype: note\ntitle: {title}\n---\n{body}");
        std::fs::write(self.root.join(rel), content).unwrap();
    }
    /// Register `root` (indexes whatever pages are present).
    fn add(&self) -> (i32, String, String) {
        self.run(&["add", self.root.to_str().unwrap()])
    }
}
impl Drop for Qs {
    fn drop(&mut self) {
        if let Some(parent) = self.root.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

// ---------------------------------------------------------------------------
// `qs` — the search-verb surface (add → qs)
// ---------------------------------------------------------------------------

#[test]
fn qs_single_hit_returns_section_body() {
    let s = Qs::new("single");
    // A unique token (zygote) appears in exactly one section; the other page
    // must not interfere, so the result is the *single* shape.
    s.page("one.md", "One", "# One\nthe zygote divides\n");
    s.page("two.md", "Two", "# Two\nnothing zygote here\n");
    let (code, _o, err) = s.add();
    assert_eq!(code, 0, "add: {err}");

    let (code, out, err) = s.run(&["qs", "zygote", "divides"]);
    assert_eq!(code, 0, "qs single: {err}");
    // single shape: the matched section's own_body, verbatim, no "ambiguous".
    assert!(
        out.contains("zygote divides"),
        "single must return the section body: {out:?}"
    );
    // a two-line body → no narrowing list.
    assert!(
        !out.contains("ambiguous"),
        "a single match must not be the ambiguous list: {out:?}"
    );
    assert!(
        out.lines().count() <= 4,
        "single is terse (body only), got: {out:?}"
    );
}

#[test]
fn qs_ambiguous_returns_narrowing_list_and_force_takes_top1() {
    let s = Qs::new("amb");
    // The same token (zygote) in two sections on two pages → ambiguous.
    s.page("a.md", "A", "# A\nzygote alpha\n");
    s.page("b.md", "B", "# B\nzygote beta\n");
    let (code, _o, err) = s.add();
    assert_eq!(code, 0, "add: {err}");

    let (code, out, err) = s.run(&["qs", "zygote"]);
    assert_eq!(code, 0, "qs ambiguous: {err}");
    assert!(
        out.contains("ambiguous"),
        "a multi-token match is the narrowing list: {out:?}"
    );
    // the hint tells the user how to force a top-1.
    assert!(
        out.contains("--force"),
        "ambiguous list must hint --force: {out:?}"
    );

    // `--force` collapses to the top-1 body (no "ambiguous" marker).
    let (code, out_f, err_f) = s.run(&["qs", "zygote", "--force"]);
    assert_eq!(code, 0, "qs --force: {err_f}");
    assert!(
        !out_f.contains("ambiguous"),
        "--force must take the top-1, not the list: {out_f:?}"
    );
    // the top-1 body is one of the two (alpha or beta) zygote lines.
    assert!(
        out_f.contains("zygote") && !out_f.is_empty(),
        "--force returns a body: {out_f:?}"
    );
}

#[test]
fn qs_empty_result_is_exit_0_with_empty_message() {
    let s = Qs::new("empty");
    s.page("one.md", "One", "# One\nsome body\n");
    let (code, _o, err) = s.add();
    assert_eq!(code, 0, "add: {err}");

    let (code, out, err) = s.run(&["qs", "zzznotthere"]);
    assert_eq!(
        code, 0,
        "an empty qs result is a *clean* empty, not an error (err: {err})"
    );
    assert!(
        out.contains("no section matched") || out.contains("empty"),
        "empty must be signalled clearly: {out:?}"
    );
}

#[test]
fn qs_json_is_a_wellformed_envelope() {
    let s = Qs::new("json");
    s.page("one.md", "One", "# One\nthe zygote divides\n");
    let (code, _o, err) = s.add();
    assert_eq!(code, 0, "add: {err}");

    let (code, out, err) = s.run(&["qs", "--json", "zygote", "divides"]);
    assert_eq!(code, 0, "qs --json: {err}");
    let out = out.trim();
    assert!(
        out.starts_with('{') && out.ends_with('}'),
        "a single JSON object on stdout: {out:?}"
    );
    // envelope fields, regardless of single/ambiguous.
    for k in ["\"status\":", "\"query\":"] {
        assert!(out.contains(k), "missing {k} in: {out}");
    }
    // single → the match is present.
    assert!(
        out.contains("\"match\": {") || out.contains("\"match\":{"),
        "a single match must populate 'match': {out}"
    );
    assert!(
        out.contains("\"own_body\":"),
        "match must carry own_body: {out}"
    );
    assert!(
        out.contains("zygote"),
        "the matched body echoes the query token: {out}"
    );
}

#[test]
fn qs_rejects_a_nonsense_query_but_succeeds_on_a_known_one() {
    // Guard against a regression where *any* qs query errors (the original
    // "kb qs doesn't find anything / nothing in DB" report). A known token
    // must hit; an unknown one must be a clean empty (exit 0), not an I/O
    // error (exit 1 / non-zero).
    let s = Qs::new("known");
    s.page("one.md", "One", "# One\nquokka reference\n");
    let (code, _o, err) = s.add();
    assert_eq!(code, 0, "add: {err}");

    let (code, out, err) = s.run(&["qs", "quokka"]);
    assert_eq!(code, 0, "known token must succeed: {err}");
    assert!(out.contains("quokka"), "matched body: {out:?}");

    let (code, _o2, err2) = s.run(&["qs", "nonsense-word"]);
    assert_eq!(code, 0, "unknown token is a clean empty, exit 0: {err2}");
}

/// The qs page-identity fields use the **canonical** names shared by
/// `search`, `tag`, `browse`, and `resolve` (`uuid` / `uuid_short` / `id` /
/// `title`) — *not* `qs`-specific variants (`page_uuid` / `page_uuid_short` /
/// `page_display_id` / `page_title`). An agent that reads one `kb` surface
/// must be able to read every other: a single, uniform vocabulary.
///
/// This pins both directions for **both** the `match` shape (single) and the
/// `candidates` shape (ambiguous), and also that `own_hash` (index-time etag
/// material) is deliberately NOT surfaced in the match object — an
/// `if_unchanged` etag must come from `kb show`, not from qs.
/// Assert that canonical identity names are present (in both the spaced
/// `render()` form — used by the `match` object — and the no-space
/// `render_compact()` form — used by `candidates` entries), and that the
/// legacy `page_*` / `own_hash` family is absent in both forms.
fn assert_canonical_uuid_shape(out: &str) {
    // Each canonical key must appear in at least one of the two render styles.
    for k in ["uuid", "uuid_short", "id", "title"] {
        let spaced = format!("\"{k}\": ");
        let compact = format!("\"{k}\":");
        assert!(
            out.contains(&spaced) || out.contains(&compact),
            "qs JSON must emit the canonical key {k:?} (matching \
             search/tag/browse/resolve); got: {out}"
        );
    }
    // The legacy family must be absent in BOTH render styles.
    for k in [
        "page_uuid",
        "page_uuid_short",
        "page_display_id",
        "page_title",
        "own_hash",
    ] {
        let spaced = format!("\"{k}\": ");
        let compact = format!("\"{k}\":");
        assert!(
            !out.contains(&spaced) && !out.contains(&compact),
            "qs JSON must NOT emit the legacy / qs-specific key {k:?}; got: {out}"
        );
    }
}

#[test]
fn qs_json_single_match_uses_canonical_page_identity_names() {
    let s = Qs::new("identity_single");
    s.page("one.md", "One", "# One\nthe quokka burrows\n");
    let (code, _o, err) = s.add();
    assert_eq!(code, 0, "add: {err}");

    let (code, out, err) = s.run(&["qs", "--json", "quokka"]);
    assert_eq!(code, 0, "qs --json: {err}");
    assert!(
        out.contains("\"match\": {") || out.contains("\"match\":{"),
        "single-hit JSON must populate 'match': {out}"
    );
    assert_canonical_uuid_shape(&out);
}

#[test]
fn qs_json_ambiguous_candidates_use_canonical_page_identity_names() {
    let s = Qs::new("identity_amb");
    s.page("one.md", "One", "# One\nthe quokka burrows\n");
    s.page("two.md", "Two", "# Two\nthe quokka digs\n");
    let (code, _o, err) = s.add();
    assert_eq!(code, 0, "add: {err}");

    let (code, out, err) = s.run(&["qs", "--json", "quokka"]);
    assert_eq!(code, 0, "qs --json: {err}");
    assert!(
        out.contains("\"candidates\": [") || out.contains("\"candidates\":["),
        "ambiguous-hit JSON must populate 'candidates': {out}"
    );
    assert_canonical_uuid_shape(&out);
}
