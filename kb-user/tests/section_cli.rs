//! End-to-end tests for the M6 single-file CLI surface.
//!
//! Drives the real `kb` binary as a subprocess over a temp file and asserts
//! on stdout / exit codes / resulting bytes. The pure logic it exercises is
//! already pinned in `kb-core::section` (+ its Verus spec); here we pin the
//! *shell* contract: exit codes, the 3-way text source, etag concurrency,
//! dry-run, and the editor flow.

use std::path::PathBuf;
use std::process::Command;

fn kb() -> Command {
    Command::new(
        std::env::var("CARGO_BIN_EXE_kb").expect("CARGO_BIN_EXE_kb unset — run under cargo test"),
    )
}

/// RAII guard: a temp file that deletes itself on drop. `temp_md` used to
/// hand back a bare `PathBuf` the caller never cleaned up — every test run
/// leaked one file into `$TMPDIR` (thousands accumulated). `Deref` to
/// `PathBuf` means every call site (`f.to_str()`, `read_to_string(&f)`, …)
/// is unchanged.
struct TempFile(PathBuf);
impl std::ops::Deref for TempFile {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}
impl AsRef<std::path::Path> for TempFile {
    fn as_ref(&self) -> &std::path::Path {
        self.0.as_path()
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Write `text` to a fresh temp file in the test's scratch dir and return a
/// guard over it. Every test uses its own file, so runs are independent.
fn temp_md(name: &str, text: &str) -> TempFile {
    let mut p = std::env::temp_dir();
    p.push(format!("kb_cli_{}_{}.md", std::process::id(), name));
    std::fs::write(&p, text).unwrap();
    TempFile(p)
}

fn base_md() -> String {
    "# Root\n\n## Alpha\n\nalpha body\n\n### Alpha-Sub\n\nsub body\n\n## Beta\n\nbeta body\n"
        .to_string()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = kb().args(args).output().expect("failed to run kb");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------
// Read surface
// ---------------------------------------------------------------------------

#[test]
fn toc_lists_the_section_tree() {
    let f = temp_md("toc", &base_md());
    let (code, out, _) = run(&["toc", f.to_str().unwrap()]);
    assert_eq!(code, 0);
    // all three headings present, in order
    let pos_root = out.find("Root").unwrap();
    let pos_a = out.find("Alpha").unwrap();
    let pos_b = out.find("Beta").unwrap();
    assert!(
        pos_root < pos_a && pos_a < pos_b,
        "outline must be in doc order"
    );
    assert!(out.contains("ref 1") && out.contains("level 1"));
    assert!(out.contains("level 2"));
}

#[test]
fn toc_json_is_well_formed_envelope() {
    let f = temp_md("tocjson", &base_md());
    let (code, out, _) = run(&["toc", f.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0);
    // a single-line JSON object with a file field and a sections array
    let out = out.trim();
    assert!(
        out.starts_with('{') && out.ends_with('}'),
        "envelope: {out}"
    );
    assert!(out.starts_with("{\"file\":"), "envelope: {out}");
    assert!(out.contains("\"sections\":[{"), "sections array: {out}");
    assert!(out.contains("\"ref\":\"1\""), "root ref present: {out}");
    assert!(out.contains("\"level\":2"), "level field present: {out}");
    // T4: per-section metrics (line + char counts, full and body-only spans)
    assert!(out.contains("\"lines\":"), "lines field present: {out}");
    assert!(out.contains("\"chars\":"), "chars field present: {out}");
    assert!(out.contains("\"body_lines\":"), "body_lines present: {out}");
    assert!(out.contains("\"body_chars\":"), "body_chars present: {out}");
}

#[test]
fn show_scoped_section_minimal_context() {
    let f = temp_md("show", &base_md());
    let (code, out, _) = run(&["show", f.to_str().unwrap(), "Alpha"]);
    assert_eq!(code, 0);
    // scoped: contains the section, NOT the sibling (Beta) body
    assert!(out.contains("## Alpha"), "{out}");
    assert!(out.contains("alpha body"), "{out}");
    assert!(!out.contains("## Beta"), "{out}");
}

#[test]
fn show_full_includes_subsections() {
    let f = temp_md("showfull", &base_md());
    let (code, out, _) = run(&["show", f.to_str().unwrap(), "Alpha", "--full"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("Alpha-Sub"),
        "full span includes subsections: {out}"
    );
    assert!(out.contains("sub body"), "{out}");
}

#[test]
fn show_etag_is_16_hex() {
    let f = temp_md("showetag", &base_md());
    let (code, out, _) = run(&["show", f.to_str().unwrap(), "Alpha", "--etag"]);
    assert_eq!(code, 0);
    let tok = out.trim();
    assert_eq!(tok.len(), 16, "etag is 16 hex chars: {out:?}");
    assert!(tok.chars().all(|c| c.is_ascii_hexdigit()), "{out:?}");
}

#[test]
fn show_unknown_ref_exits_2() {
    let f = temp_md("shownope", &base_md());
    let (code, _, err) = run(&["show", f.to_str().unwrap(), "does-not-exist"]);
    assert_eq!(code, 2, "section/resolve errors use exit 2 (stderr: {err})");
}

// ---------------------------------------------------------------------------
// Write surface
// ---------------------------------------------------------------------------

#[test]
fn set_section_replaces_and_writes() {
    let f = temp_md("set", &base_md());
    let (code, _, _) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--text",
        "brand new body",
        "--yes",
    ]);
    assert_eq!(code, 0);
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("brand new body"), "{now}");
    assert!(!now.contains("alpha body"), "{now}");
    // set (without --keep-heading) replaces the section incl. its heading
    assert!(
        !now.contains("## Alpha"),
        "set default drops the heading: {now}"
    );
}

#[test]
fn set_keep_heading_preserves_heading() {
    let f = temp_md("setkeep", &base_md());
    let (code, _, _) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--text",
        "new body",
        "--keep-heading",
        "--yes",
    ]);
    assert_eq!(code, 0);
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("## Alpha"), "heading retained: {now}");
    assert!(now.contains("new body"), "{now}");
}

#[test]
fn append_section_before_next_heading() {
    let f = temp_md("append", &base_md());
    let (code, _, _) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "append",
        "Beta",
        "--text",
        "appended line",
        "--yes",
    ]);
    assert_eq!(code, 0);
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("appended line"), "{now}");
    // order preserved: beta body then the appended line
    assert!(
        now.find("beta body").unwrap() < now.find("appended line").unwrap(),
        "{now}"
    );
}

#[test]
fn insert_child_section() {
    let f = temp_md("insert", &base_md());
    let (code, _, _) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "insert",
        "--ref",
        "Alpha",
        "--title",
        "Fresh",
        "--text",
        "fresh body",
        "--yes",
    ]);
    assert_eq!(code, 0);
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("fresh body"), "{now}");
    // a new section (`Fresh`) must now appear in the outline
    let (code, out, _) = run(&["toc", f.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(out.contains("Fresh"), "inserted section in outline: {out}");
}

#[test]
fn replace_substring_reports_count_on_stderr() {
    let f = temp_md("repl", &base_md());
    let (code, _, err) = run(&[
        "replace",
        f.to_str().unwrap(),
        "--find",
        "alpha body",
        "--new",
        "alpha body v9",
        "--yes",
    ]);
    assert_eq!(code, 0);
    assert!(err.contains("1 replacement"), "{err}");
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("alpha body v9"), "{now}");
}

#[test]
fn replace_substring_not_found_exits_2() {
    let f = temp_md("replnf", &base_md());
    let (code, _, err) = run(&[
        "replace",
        f.to_str().unwrap(),
        "--find",
        "nope-not-here",
        "--new",
        "x",
        "--yes",
    ]);
    assert_eq!(
        code, 2,
        "nothing-to-replace is a typed error (stderr: {err})"
    );
    // file must be untouched
    assert_eq!(std::fs::read_to_string(&f).unwrap(), base_md());
}

// ---------------------------------------------------------------------------
// Diff conformance
// ---------------------------------------------------------------------------

/// The strongest proof of standard-`diff -u` conformance: emit our CLI's
/// own diff (via `--dry-run`), then hand it to **GNU `patch(1)`** and assert
/// the result byte-matches the binary's actual write. `patch(1)` is the
/// reference parser — if it consumes our output, so will any other
/// standard-compliant tool (or a human reading it).
#[test]
fn emitted_diff_is_reverse_applicable_by_patch_one() {
    let f = temp_md("conform", &base_md());
    let before = std::fs::read_to_string(&f).unwrap();

    // (1) the edit the binary will actually apply
    let (code, _, _) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--text",
        "the brand new alpha body\nsecond line",
        "--keep-heading",
        "--yes",
    ]);
    assert_eq!(code, 0);
    let after = std::fs::read_to_string(&f).unwrap();
    assert_ne!(before, after, "the edit must have changed the file");

    // (2) restore the *before* state so we can feed our dry-run diff to patch(1)
    std::fs::write(&f, &before).unwrap();

    // (3) capture the binary's dry-run diff (stdout only)
    let (code, diff_out, _) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--text",
        "the brand new alpha body\nsecond line",
        "--keep-heading",
        "--dry-run",
    ]);
    assert_eq!(code, 0);
    let path_s = f.to_str().unwrap().to_string();
    assert!(
        diff_out.contains("@@") && diff_out.contains("-alpha body"),
        "diff must have hunk header + removed lines: {diff_out}"
    );
    assert!(
        diff_out.starts_with(&format!("--- {path_s}\n+++ ")),
        "labels: {diff_out}"
    );

    // (4) feed that diff to patch(1); compare the result to the binary's write
    let tmp_dir = std::env::temp_dir();
    let patched = tmp_dir.join(format!("kb_cli_conform_patched_{}.md", std::process::id()));
    std::fs::write(&patched, &before).unwrap();

    let p1 = Command::new("patch")
        .arg("--quiet")
        .arg(&patched)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("patch(1) available");
    let out = {
        use std::io::Write;
        let mut child = p1;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(diff_out.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    assert!(
        out.status.success(),
        "patch(1) failed to consume our diff: {}\n--- diff was: ---\n{diff_out}",
        String::from_utf8_lossy(&out.stderr)
    );
    let patched_now = std::fs::read_to_string(&patched).unwrap();
    assert_eq!(
        patched_now, after,
        "patch(1) applying our diff must reproduce the binary's actual write"
    );
    let _ = std::fs::remove_file(&patched);
}

// ---------------------------------------------------------------------------
// Concurrency (etag) + dry-run + text sources
// ---------------------------------------------------------------------------

#[test]
fn if_unchanged_match_proceeds_mismatch_aborts_3() {
    let f = temp_md("etag", &base_md());
    // (a) capture the etag of the very section we will then mutate
    let (code, etag, _) = run(&["show", f.to_str().unwrap(), "Alpha", "--etag"]);
    assert_eq!(code, 0);
    let etag = etag.trim();

    // (b) unchanged target → guard passes
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--text",
        "alpha v2",
        "--keep-heading",
        "--if-unchanged",
        etag,
        "--yes",
    ]);
    assert_eq!(code, 0, "matching etag passes (stderr: {err})");

    // (c) repeat with the *stale* etag → guard trips with exit 3
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--text",
        "alpha v3",
        "--keep-heading",
        "--if-unchanged",
        etag,
        "--yes",
    ]);
    assert_eq!(code, 3, "stale etag → exit 3 (stderr: {err})");
}

/// Agent contract: `--json` promises stdout is exactly one JSON object on
/// *both* the success and failure paths (the prose "apply?" prompt is
/// suppressed under `--json`). An etag mismatch exits 3, and it must still
/// emit a machine-readable envelope on stdout — otherwise an agent
/// `json.loads(out.stdout)` silently returns `null` and the retry loop never
/// sees the etag mismatch. `section_cli` pins this at the shell level.
#[test]
fn if_unchanged_mismatch_with_json_emits_failure_envelope() {
    let f = temp_md("etagjson", &base_md());
    // Capture the *current* etag of the section we will then mutate, so the
    // value we pass in `--if-unchanged` is guaranteed to be stale by the
    // time the guard runs.
    let (code, cur_etag, _) = run(&["show", f.to_str().unwrap(), "Alpha", "--etag"]);
    assert_eq!(code, 0);
    let cur_etag = cur_etag.trim().to_string();
    assert_eq!(cur_etag.len(), 16, "etag is 16 hex chars: {cur_etag:?}");
    // Mutate first so the guard will definitely see a different etag.
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--text",
        "mutated",
        "--keep-heading",
        "--if-unchanged",
        cur_etag.as_str(),
        "--yes",
    ]);
    assert_eq!(
        code, 0,
        "first mutation with the matching etag must pass (stderr: {err})"
    );
    // Now run the same op again with the *pre*-mutation etag → guard trips.
    let (code, out, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--text",
        "mutated2",
        "--keep-heading",
        "--if-unchanged",
        cur_etag.as_str(),
        "--yes",
        "--json",
    ]);
    assert_eq!(
        code, 3,
        "stale etag → exit 3 even under --json (stderr: {err})"
    );
    // stdout must be a clean JSON object — not empty, not prose.
    assert!(
        !out.trim().is_empty(),
        "under --json the stdout envelope must be non-empty: {out:?}"
    );
    assert!(
        out.trim_start().starts_with('{'),
        "under --json stdout is one JSON object: {out:?}"
    );
    assert!(
        out.contains("\"ok\": false"),
        "envelope signals failure: {out:?}"
    );
    assert!(
        out.contains("\"applied\": false"),
        "nothing was written: {out:?}"
    );
    assert!(
        out.contains("\"code\": 3"),
        "code 3 in the envelope: {out:?}"
    );
    assert!(
        out.contains("\"op\": \"etag_check\""),
        "envelope names the op: {out:?}"
    );
    assert!(
        out.contains("\"scope\": \"Alpha\""),
        "scope is the section ref: {out:?}"
    );
    assert!(
        out.contains("\"etag_expected\": \"") && out.contains(&format!("\"{cur_etag}\"")),
        "both etag fields present: {out:?}"
    );
    // The file must be byte-for-byte unchanged (the guard blocked the write).
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(
        after.contains("mutated"),
        "the mutation was applied by pass (b); the stale etag op must not overwrite it: {after:?}"
    );
    // stderr still carries the human message (backwards compat for terminals).
    assert!(
        err.contains("etag mismatch"),
        "human message on stderr: {err:?}"
    );
}

#[test]
fn dry_run_prints_diff_but_writes_nothing() {
    let f = temp_md("dry", &base_md());
    let before = std::fs::read_to_string(&f).unwrap();
    let (code, out, _) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--text",
        "dry-run body",
        "--dry-run",
    ]);
    assert_eq!(code, 0);
    assert!(out.contains("@@"), "dry-run shows a unified diff: {out}");
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        before,
        "dry-run must not write"
    );
}

#[test]
fn text_source_stdin() {
    let f = temp_md("stdin", &base_md());
    let mut c = kb();
    c.args([
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--keep-heading",
        "--stdin",
        "--yes",
    ])
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"body-via-stdin\n")
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
    assert!(std::fs::read_to_string(&f)
        .unwrap()
        .contains("body-via-stdin"));
}

#[test]
fn text_source_from_file() {
    let f = temp_md("fromfile", &base_md());
    let src = temp_md("fromfile_src", "body-from-file\n");
    let (code, _, _) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--keep-heading",
        "--from-file",
        src.to_str().unwrap(),
        "--yes",
    ]);
    assert_eq!(code, 0);
    assert!(std::fs::read_to_string(&f)
        .unwrap()
        .contains("body-from-file"));
}

#[test]
fn no_text_source_is_exit_1() {
    let f = temp_md("nosource", &base_md());
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "Alpha",
        "--yes",
    ]);
    assert_eq!(code, 1, "generic error (stderr: {err})");
}

// ---------------------------------------------------------------------------
// Frontmatter surface (M6b) — `fm` / `fm.<key>` refs
// ---------------------------------------------------------------------------

/// A page that leads with a well-formed frontmatter frame.
fn fm_md() -> String {
    "---\ntype: wiki\ntitle: Foo\ntags:\n  - a\n  - b\n---\n# H\n\nbody\n".to_string()
}

#[test]
fn show_fm_prints_whole_frame() {
    let f = temp_md("fm_show", &fm_md());
    let (code, out, _) = run(&["show", f.to_str().unwrap(), "fm"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("---\n"), "frame starts with opener: {out}");
    assert!(
        out.contains("type: wiki") && out.contains("title: Foo"),
        "{out}"
    );
    assert!(out.contains("- b"), "{out}");
    // the body must NOT leak into the frame
    assert!(
        !out.contains("body\n# H") && !out.contains("# H"),
        "body excluded: {out}"
    );
}

#[test]
fn show_fm_key_scalar() {
    let f = temp_md("fm_key_s", &fm_md());
    let (code, out, _) = run(&["show", f.to_str().unwrap(), "fm.title"]);
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "title: Foo", "just the scalar field: {out:?}");
}

#[test]
fn show_fm_key_list_includes_items() {
    let f = temp_md("fm_key_l", &fm_md());
    let (code, out, _) = run(&["show", f.to_str().unwrap(), "fm.tags"]);
    assert_eq!(code, 0);
    assert!(out.contains("tags:"), "{out}");
    assert!(
        out.contains("- a") && out.contains("- b"),
        "list items included: {out}"
    );
}

#[test]
fn show_fm_unknown_key_is_empty() {
    let f = temp_md("fm_key_u", &fm_md());
    let (code, out, _) = run(&["show", f.to_str().unwrap(), "fm.nonexistent"]);
    assert_eq!(code, 0);
    assert!(out.trim().is_empty(), "absent key prints nothing: {out:?}");
}

#[test]
fn show_fm_no_frontmatter_exits_2() {
    let f = temp_md("fm_nofm", &base_md());
    let (code, _, err) = run(&["show", f.to_str().unwrap(), "fm"]);
    assert_eq!(code, 2, "no frame → exit 2 (stderr: {err})");
}

#[test]
fn fm_set_scalar_creates_or_replaces() {
    let f = temp_md("fm_set", &fm_md());
    // create a new scalar key
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "fm.description",
        "--value",
        "A short desc",
        "--yes",
    ]);
    assert_eq!(code, 0, "{err}");
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("description: A short desc"), "{now}");
    // replace an existing scalar key
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "fm.title",
        "--value",
        "Bar",
        "--yes",
    ]);
    assert_eq!(code, 0, "{err}");
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("title: Bar"), "{now}");
    assert!(!now.contains("title: Foo"), "{now}");
}

#[test]
fn fm_append_list_item() {
    let f = temp_md("fm_append", &fm_md());
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "append",
        "fm.tags",
        "--value",
        "c",
        "--yes",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, out, _) = run(&["show", f.to_str().unwrap(), "fm.tags"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("- a") && out.contains("- b") && out.contains("c"),
        "appended item present: {out}"
    );
}

#[test]
fn fm_rm_list_item() {
    let f = temp_md("fm_rmitem", &fm_md());
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "fm.tags",
        "a",
        "--yes",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, out, _) = run(&["show", f.to_str().unwrap(), "fm.tags"]);
    assert_eq!(code, 0);
    // the kernel collapses a rewritten block list to inline form, so assert
    // on the value rather than a specific `- b` block layout.
    assert!(
        !out.contains(" a,") && !out.contains("]a") && !out.contains("[a]"),
        "{out}"
    );
    assert!(out.contains("b"), "b retained: {out}");
}

#[test]
fn fm_rm_field() {
    let f = temp_md("fm_rmfield", &fm_md());
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "fm.title",
        "--yes",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = run(&["show", f.to_str().unwrap(), "fm.title"]);
    assert_eq!(code, 0, "{err}");
    // title now absent: showing it must print nothing
    let (code, out, _) = run(&["show", f.to_str().unwrap(), "fm.title"]);
    assert_eq!(code, 0);
    assert!(out.trim().is_empty(), "field removed: {out:?}");
}

#[test]
fn fm_rm_list_item_absent_exits_2() {
    let f = temp_md("fm_rmabs", &fm_md());
    let before = std::fs::read_to_string(&f).unwrap();
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "fm.tags",
        "zzz-not-there",
        "--yes",
    ]);
    assert_eq!(code, 2, "absent value → exit 2 (stderr: {err})");
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        before,
        "file untouched"
    );
}

#[test]
fn fm_ops_on_no_frame_exit_2() {
    let f = temp_md("fm_ops", &base_md());
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "fm.title",
        "--value",
        "x",
        "--yes",
    ]);
    assert_eq!(code, 2, "no frontmatter → exit 2 (stderr: {err})");
}

#[test]
fn fm_if_unchanged_mismatch_exits_3() {
    let f = temp_md("fm_etag", &fm_md());
    // the fm op etag contract is the *whole file* etag
    let (code, etag, _) = run(&["show", f.to_str().unwrap(), "fm.title", "--etag"]);
    assert_eq!(code, 0);
    let good = etag.trim();
    // a *wrong* etag trips the guard (exit 3) before any mutation
    let before = std::fs::read_to_string(&f).unwrap();
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "fm.title",
        "--value",
        "Nope",
        "--if-unchanged",
        "deadbeefdeadbeef",
        "--yes",
    ]);
    assert_eq!(code, 3, "stale/mismatch etag → exit 3 (stderr: {err})");
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        before,
        "file untouched"
    );
    let _ = good;
}

#[test]
fn fm_dry_run_writes_nothing() {
    let f = temp_md("fm_dry", &fm_md());
    let before = std::fs::read_to_string(&f).unwrap();
    let (code, out, _) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "set",
        "fm.description",
        "--value",
        "dry",
        "--dry-run",
    ]);
    assert_eq!(code, 0);
    assert!(out.contains("@@"), "dry-run shows a diff: {out}");
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        before,
        "dry-run must not write"
    );
}

// ---------------------------------------------------------------------------
// `kb section rm <section-ref>` — remove a content section (heading + body +
// its subsections), not just fm fields. Pins the *shell* contract for the
// new branch: exit codes, the value-argument rejection, etag concurrency,
// dry-run, and that the removal preserves the surrounding Markdown.
// ---------------------------------------------------------------------------

/// Content body (no frontmatter): `# Top / ## Mid / ## Bot`.
/// Dotted ref layout: `# Top` = 1, `## Mid` = 1.1, `## Bot` = 1.2.
fn sections_md() -> String {
    "# Top\ntop body\n\n## Mid\nmid body\n\n## Bot\nbot body\n".to_string()
}

#[test]
fn rm_middle_section() {
    let f = temp_md("rm_middle", &sections_md());
    let (code, out, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "1.1",
        "--yes",
        "--json",
    ]);
    assert_eq!(code, 0, "rc / err: {err}");
    assert!(out.contains("\"changed\": true") && out.contains("\"applied\": true"), "{out}");
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("# Top") && now.contains("top body"), "Top intact: {now}");
    assert!(!now.contains("## Mid") && !now.contains("mid body"), "Mid removed: {now}");
    assert!(now.contains("## Bot") && now.contains("bot body"), "Bot intact: {now}");
    // No doubled blank separators (the splice hygiene is preserved through commit).
    assert!(!now.contains("top body\n\n\n"), "separator doubled: {now:?}");
    assert!(now.ends_with('\n'), "newline-terminated: {now:?}");
}

#[test]
fn rm_last_section() {
    let f = temp_md("rm_last", &sections_md());
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "1.2",
        "--yes",
    ]);
    assert_eq!(code, 0, "{err}");
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("## Mid") && now.contains("mid body"), "Mid intact: {now}");
    assert!(!now.contains("## Bot") && !now.contains("bot body"), "Bot removed: {now}");
    // The removed last section's trailing blank is not left dangling.
    assert!(!now.ends_with("mid body\n\n\n"), "no dangling trailing blank: {now:?}");
    assert!(now.ends_with('\n'), "newline-terminated: {now:?}");
}

#[test]
fn rm_whole_root_section() {
    let f = temp_md("rm_root", &sections_md());
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "1",
        "--yes",
    ]);
    assert_eq!(code, 0, "{err}");
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(
        now.trim().is_empty(),
        "removing the root section empties the file: {now:?}"
    );
}

#[test]
fn rm_section_subtree_includes_its_children() {
    let md = "# Root\nroot\n\n## Alpha\nalpha\n\n### Alpha-Sub\nsub\n\n## Beta\nbeta\n";
    let f = temp_md("rm_subtree", md);
    // Refs: Root=1, Alpha=1.1, Alpha-Sub=1.1.1, Beta=1.2.
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "1.1",
        "--yes",
    ]);
    assert_eq!(code, 0, "{err}");
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("# Root") && now.contains("root"), "Root intact: {now}");
    assert!(!now.contains("## Alpha"), "Alpha removed: {now}");
    assert!(!now.contains("### Alpha-Sub"), "Alpha-Sub (child) removed too: {now}");
    assert!(!now.contains("sub body"), "child body removed: {now}");
    assert!(now.contains("## Beta") && now.contains("beta"), "Beta intact: {now}");
}

#[test]
fn rm_section_with_value_rejected() {
    // A `value` is meaningful only for an `fm.<list>` ref; a section ref
    // rejects it rather than silently ignoring it.
    let f = temp_md("rm_value", &sections_md());
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "1.1",
        "mid-body",
        "--yes",
    ]);
    assert_eq!(code, 2, "section ref + value → exit 2 (stderr: {err})");
    // File untouched.
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("## Mid"), "file untouched: {now}");
}

#[test]
fn rm_section_unknown_ref_exits_2() {
    let f = temp_md("rm_unknown", &sections_md());
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "9",
        "--yes",
    ]);
    assert_eq!(code, 2, "no such section → exit 2 (stderr: {err})");
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(now.contains("## Mid"), "file untouched: {now}");
}

#[test]
fn rm_section_etag_mismatch_exits_3() {
    // The section ref uses the *section* etag (the one `show <ref> --etag`
    // prints), not the whole-file etag — so a mismatched value trips the
    // guard before any mutation.
    let f = temp_md("rm_etag", &sections_md());
    let before = std::fs::read_to_string(&f).unwrap();
    // Grab the real 1.1 etag to confirm the guard is comparing against it.
    let (code, real_raw, _) = run(&["show", f.to_str().unwrap(), "1.1", "--etag"]);
    assert_eq!(code, 0);
    let real = real_raw.trim();
    assert!(!real.is_empty() && real.len() == 16, "got etag: {real:?}");
    // Mismatch (a dead value) → exit 3, file untouched.
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "1.1",
        "--if-unchanged",
        "deadbeefdeadbeef",
        "--yes",
    ]);
    assert_eq!(code, 3, "section-rm etag mismatch → exit 3 (stderr: {err})");
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        before,
        "file untouched"
    );
    // A *matching* etag goes through.
    let (code, _, err) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "1.1",
        "--if-unchanged",
        real,
        "--yes",
    ]);
    assert_eq!(code, 0, "matching etag → proceeds (stderr: {err})");
    let now = std::fs::read_to_string(&f).unwrap();
    assert!(!now.contains("## Mid"), "section actually removed: {now}");
}

#[test]
fn rm_section_dry_run_writes_nothing() {
    let f = temp_md("rm_dry", &sections_md());
    let before = std::fs::read_to_string(&f).unwrap();
    let (code, out, _) = run(&[
        "section",
        "--file",
        f.to_str().unwrap(),
        "rm",
        "1.1",
        "--dry-run",
    ]);
    assert_eq!(code, 0, "dry-run still exits 0");
    assert!(out.contains("@@"), "dry-run shows a diff: {out}");
    let now = std::fs::read_to_string(&f).unwrap();
    assert_eq!(now, before, "dry-run must not write");
    assert!(now.contains("## Mid"), "section still present after dry-run: {now}");
}

// ---------------------------------------------------------------------------
// `kb patch` — import an *external* unified diff (M6.5, §13f)
// ---------------------------------------------------------------------------

/// The pinned fixture set: `section_old.md` + `standard.diff` apply to
/// exactly `section_new.md` (proven against GNU `patch(1)` in the `diff.rs`
/// unit tests, so here we can trust the input and only need to check that
/// the *CLI plumbing* — arg parsing, byte-preservation, `commit()` — works.
fn fixture(name: &str) -> String {
    // The fixture lives in the shared source dir (`kb/testdata`); the `user`
    // package that compiles the test sits a level up from it.
    let path = format!("{}/../kb/testdata/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read_to_string(path).expect("fixture present")
}

#[test]
fn patch_applies_a_real_external_diff_byte_identical() {
    let f = std::env::temp_dir().join(format!("kb_patch_a_{}.md", std::process::id()));
    let base_s = fixture("section_old.md");
    let want_s = fixture("section_new.md");
    std::fs::write(&f, &base_s).unwrap();
    let (code, _, err) = run(&[
        "patch",
        f.to_str().unwrap(),
        "--patch-file",
        format!(
            "{}/../kb/testdata/standard.diff",
            env!("CARGO_MANIFEST_DIR")
        )
        .as_str(),
        "--yes",
    ]);
    assert_eq!(code, 0, "patch failed: {err}");
    let now = std::fs::read_to_string(&f).unwrap();
    assert_eq!(
        now, want_s,
        "patch output must byte-match the GNU-patch reference"
    );
    let _ = std::fs::remove_file(&f);
}

#[test]
fn patch_via_stdin_applies() {
    let f = std::env::temp_dir().join(format!("kb_patch_b_{}.md", std::process::id()));
    std::fs::write(&f, fixture("section_old.md")).unwrap();
    let want_s = fixture("section_new.md");
    let mut c = kb();
    c.args(["patch", f.to_str().unwrap(), "--patch-stdin", "--yes"]);
    c.stdin(std::process::Stdio::piped());
    c.stdout(std::process::Stdio::piped());
    c.stderr(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(fixture("standard.diff").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "patch --patch-stdin failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&f).unwrap(), want_s);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn patch_via_inline_text_applies() {
    let f = std::env::temp_dir().join(format!("kb_patch_c_{}.md", std::process::id()));
    std::fs::write(&f, fixture("section_old.md")).unwrap();
    let want_s = fixture("section_new.md");
    // Pass the patch inline. A real `diff -u` contains newlines — shell-
    // escape via clap's `--patch-text=...` (which accepts a literal value).
    let patch = fixture("standard.diff");
    let (code, _, err) = run(&[
        "patch",
        f.to_str().unwrap(),
        "--patch-text",
        patch.as_str(),
        "--yes",
    ]);
    assert_eq!(code, 0, "patch --patch-text failed: {err}");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), want_s);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn patch_dry_run_prints_diff_but_writes_nothing() {
    let f = std::env::temp_dir().join(format!("kb_patch_d_{}.md", std::process::id()));
    let base_s = fixture("section_old.md");
    std::fs::write(&f, &base_s).unwrap();
    let (code, out, _) = run(&[
        "patch",
        f.to_str().unwrap(),
        "--patch-file",
        format!(
            "{}/../kb/testdata/standard.diff",
            env!("CARGO_MANIFEST_DIR")
        )
        .as_str(),
        "--dry-run",
    ]);
    assert_eq!(code, 0);
    assert!(out.contains("@@"), "dry-run must show the diff: {out}");
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        base_s,
        "dry-run must not write"
    );
    let _ = std::fs::remove_file(&f);
}

#[test]
fn patch_json_is_well_formed_envelope() {
    let f = std::env::temp_dir().join(format!("kb_patch_e_{}.md", std::process::id()));
    std::fs::write(&f, fixture("section_old.md")).unwrap();
    let (code, out, _) = run(&[
        "patch",
        f.to_str().unwrap(),
        "--patch-file",
        format!(
            "{}/../kb/testdata/standard.diff",
            env!("CARGO_MANIFEST_DIR")
        )
        .as_str(),
        "--yes",
        "--json",
    ]);
    assert_eq!(code, 0);
    // exactly one JSON object on stdout with the uniform write envelope
    let out = out.trim();
    assert!(
        out.starts_with('{') && out.ends_with('}'),
        "envelope: {out}"
    );
    for k in [
        "\"file\":",
        "\"changed\":",
        "\"dry_run\":",
        "\"applied\":",
        "\"bytes_before\":",
        "\"bytes_after\":",
        "\"diff\":",
    ] {
        assert!(out.contains(k), "missing field {k} in: {out}");
    }
    assert!(
        out.contains("\"applied\": true"),
        "must report applied=true: {out}"
    );
    let _ = std::fs::remove_file(&f);
}

#[test]
fn patch_no_source_is_exit_1() {
    let f = std::env::temp_dir().join(format!("kb_patch_f_{}.md", std::process::id()));
    std::fs::write(&f, fixture("section_old.md")).unwrap();
    let (code, _, err) = run(&["patch", f.to_str().unwrap(), "--yes"]);
    assert_eq!(
        code, 1,
        "no source at all is a generic error (stderr: {err})"
    );
    let _ = std::fs::remove_file(&f);
}

#[test]
fn patch_two_sources_is_exit_1() {
    let f = std::env::temp_dir().join(format!("kb_patch_g_{}.md", std::process::id()));
    std::fs::write(&f, fixture("section_old.md")).unwrap();
    let pf = format!(
        "{}/../kb/testdata/standard.diff",
        env!("CARGO_MANIFEST_DIR")
    );
    let (code, _, err) = run(&[
        "patch",
        f.to_str().unwrap(),
        "--patch-file",
        pf.as_str(),
        "--patch-text",
        "x",
        "--yes",
    ]);
    assert_eq!(code, 1, "two sources → generic error (stderr: {err})");
    let _ = std::fs::remove_file(&f);
}

#[test]
fn patch_identity_is_a_noop_changeless() {
    // A patch that changes nothing (all ` ` context lines) is a no-op —
    // `commit()` reports "no changes" and exit 0, file untouched. We
    // synthesize this by diffing the file against itself.
    let f = std::env::temp_dir().join(format!("kb_patch_h_{}.md", std::process::id()));
    let base = fixture("section_old.md");
    std::fs::write(&f, &base).unwrap();
    // A zero-change patch: every line is a context line (` ` prefix) that
    // already matches `base`. Apply → result equals base → commit() reports
    // "no changes" (exit 0) and leaves the file alone. (The patch body must
    // match `base`, so we craft it from the actual first 3 lines of `base`.)
    let base_lines: Vec<&str> = base.lines().take(3).collect();
    let ctx: Vec<String> = base_lines.iter().map(|l| format!(" {l}")).collect();
    let patch = format!("--- a\n+++ b\n@@ -1,3 +1,3 @@\n{}", ctx.join("\n"));
    let (code, _out, err) = run(&["patch", f.to_str().unwrap(), "--patch-text", patch.as_str()]);
    assert_eq!(code, 0, "identity patch must succeed (stderr: {err})");
    let now = std::fs::read_to_string(&f).unwrap();
    assert_eq!(
        now, base,
        "an identity patch must be a no-op, file untouched"
    );
    let _ = std::fs::remove_file(&f);
}

// ---------------------------------------------------------------------------
// Multi-file namespace stubs
// ---------------------------------------------------------------------------

#[test]
fn multifile_namespace_is_a_declined_stub() {
    let (code, _, err) = run(&["add", "/tmp/nope"]);
    // declared but not yet implemented: a clean, non-zero generic error
    assert_eq!(code, 1, "multi-file stubs exit 1 (stderr: {err})");
}

// ---------------------------------------------------------------------------
// T6 — auto-refresh after a write (commit re-indexes the single page)
// ---------------------------------------------------------------------------

/// A throwaway root + throwaway DB, mirroring m7_multifile's harness. Drives
/// the real binary with `--db` so runs are independent of the user's DB.
struct T6 {
    root: std::path::PathBuf,
    db: std::path::PathBuf,
}
impl T6 {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("kb_t6_{pid}_{tag}", pid = std::process::id()));
        let root = base.join("root");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let db = base.join("kb.db");
        let _ = std::fs::remove_file(&db);
        T6 { root, db }
    }
    /// Run `kb --db <db> <args…>`, returning `(code, stdout, stderr)`.
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
    fn page(&self, rel: &str, title: &str, body: &str) {
        let fm = format!("type: note\ntitle: {title}\n");
        std::fs::write(self.root.join(rel), format!("---\n{fm}---\n{body}")).unwrap();
    }
}
impl Drop for T6 {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.root.parent().unwrap());
    }
}

/// A `replace` that introduces a new wikilink must *immediately* show up in a
/// fresh process's `links` view — no manual `kb refresh` in between. This is
/// the T6 contract: a write re-indexes the single page on commit.
#[test]
fn t6_replace_adds_link_visible_without_manual_refresh() {
    let s = T6::new("replace_link");
    // A has no links yet; B is the future target.
    s.page("a.md", "A", "hello anchor\n");
    s.page("b.md", "B", "the target\n");
    // Register the root (indexes the initial, link-free state).
    let (code, _out, err) = s.run(&["add", s.root.to_str().unwrap()]);
    assert_eq!(code, 0, "add: {err}");
    // Baseline: A has no forward links.
    let (code, out, err) = s.run(&["links", "note:A", "--dedup"]);
    assert_eq!(code, 0, "links baseline: {err}");
    assert!(
        !out.contains("note:B") && !out.contains("a.md → b.md"),
        "baseline must have no A→B link: {out}"
    );
    // A write that introduces the link (no `kb refresh` after this).
    let (code, _out, err) = s.run(&[
        "replace",
        s.root.join("a.md").to_str().unwrap(),
        "--find",
        "anchor",
        "--new",
        "see [[B]]",
        "--yes",
    ]);
    assert_eq!(code, 0, "replace: {err}");
    // Fresh process: the new link must already be in the index.
    let (code, out, err) = s.run(&["links", "note:A", "--dedup"]);
    assert_eq!(code, 0, "links after replace: {err}");
    assert!(
        out.contains("B") && out.contains("forward links"),
        "the [[B]] link introduced by the write must be indexed immediately (T6): {out}"
    );
}

/// Same T6 guarantee through the *section* write path (`commit` from a
/// `section set`): a body edit that adds a link is reflected in `links`
/// without a manual refresh.
#[test]
fn t6_section_set_adds_link_visible_without_manual_refresh() {
    let s = T6::new("section_link");
    s.page("a.md", "A", "# A\n\nintro\n");
    s.page("b.md", "B", "the target\n");
    let (code, _out, err) = s.run(&["add", s.root.to_str().unwrap()]);
    assert_eq!(code, 0, "add: {err}");
    // Section-op write: insert a new section on A whose body links to B.
    let (code, _out, err) = s.run(&[
        "section",
        "--file",
        s.root.join("a.md").to_str().unwrap(),
        "insert",
        "--title",
        "Refs",
        "--text",
        "see [[B]]",
        "--placement",
        "end",
        "--yes",
    ]);
    assert_eq!(code, 0, "section insert: {err}");
    let (code, out, err) = s.run(&["links", "note:A", "--dedup"]);
    assert_eq!(code, 0, "links after section write: {err}");
    assert!(
        out.contains("B") && out.contains("forward links"),
        "the [[B]] link introduced by the section write must be indexed immediately (T6): {out}"
    );
}
