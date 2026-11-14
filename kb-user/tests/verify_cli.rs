//! `kb verify <FILE>` — page lint + link check (Phase 1).
//!
//! Drives the real `kb` binary against a throwaway file tree, mirroring
//! `create_cli.rs`. Covers the human + `--json` render paths, the exit
//! code contract, and every check (a) markdown ext, (b) frontmatter
//! framing, (c) required fields. Exit codes:
//!   - `0` — ok or (non-strict) ok-with-warnings
//!   - `1` — error (a/b/c failure) OR strict-escalated warning
//!   - `2` — usage error (directory, missing file)
//!
//! Run: `cargo test -p kb-user --test verify_cli`

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn kb() -> Command {
    Command::new(
        std::env::var("CARGO_BIN_EXE_kb").expect("CARGO_BIN_EXE_kb unset — run under cargo test"),
    )
}

struct Setup {
    root: PathBuf,
}

impl Setup {
    fn new(tag: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("kbverify_{}_{}_{}", std::process::id(), seq, tag));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        Setup { root: base }
    }
    fn file(&self, name: &str, content: &str) -> PathBuf {
        let p = self.root.join(name);
        fs::write(&p, content).unwrap();
        p
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Run `kb <args…>` in the setup dir. Returns `(code, stdout, stderr)`.
/// Every kb invocation (including the default-DB `open_db`) is redirected
/// under the per-test `root` so parallel test runs never share the real
/// `~/.local/share/knowledgebase/kb.db`.
fn run(setup: &Setup, args: &[&str]) -> (i32, String, String) {
    let mut cmd = kb();
    for x in args {
        cmd.arg(x);
    }
    cmd.current_dir(&setup.root);
    let data = setup.root.join("xdg");
    fs::create_dir_all(&data).unwrap();
    cmd.env("XDG_DATA_HOME", &data);
    let out = cmd.output().expect("failed to run kb");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Validate that `s` parses as a single JSON *object*. Mirrors the helper
/// in `create_cli.rs` (no new runtime dependencies allowed).
fn assert_json_object(s: &str, what: &str) {
    let py = if cfg!(windows) { "python" } else { "python3" };
    let mut v = Command::new(py);
    v.arg("-c")
        .arg("import json,sys\nd = json.loads(sys.stdin.read())\nassert isinstance(d, dict), type(d).__name__\nprint(','.join(d.keys()))");
    use std::io::Write;
    let mut child = v
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("a Python interpreter is available for JSON validation");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(s.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{what}: stdout is not one JSON object: {s:?}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// exit-code contract
// ---------------------------------------------------------------------------

#[test]
fn verify_ok_exit_zero_human() {
    let s = Setup::new("ok");
    let p = s.file(
        "page.md",
        "---\ntype: note\ntitle: Good page\ndescription: short desc\n---\n\n# Good page\n\nBody.\n",
    );
    let (code, out, _) = run(&s, &["verify", p.to_str().unwrap()]);
    assert_eq!(code, 0, "verify of a good page exits 0");
    assert!(
        out.contains("verdict: ok"),
        "human render should show verdict: ok; got {out:?}"
    );
}

#[test]
fn verify_error_missing_type_exit_one() {
    let s = Setup::new("no_type");
    let p = s.file("no_type.md", "---\ntitle: x\n---\n\n# x\nBody.\n");
    let (code, out, _) = run(&s, &["verify", p.to_str().unwrap()]);
    assert_eq!(code, 1, "missing type is an error");
    assert!(
        out.contains("missing-type"),
        "human render should list missing-type; got {out:?}"
    );
}

#[test]
fn verify_error_unknown_type_exit_one() {
    let s = Setup::new("bad_type");
    let p = s.file("bad.md", "---\ntype: labbel\ntitle: x\n---\n\n# x\n");
    let (code, out, _) = run(&s, &["verify", p.to_str().unwrap()]);
    assert_eq!(code, 1, "unknown type is an error");
    assert!(
        out.contains("labbel"),
        "human render should surface the raw type value; got {out:?}"
    );
}

#[test]
fn verify_error_no_frontmatter_exit_one() {
    let s = Setup::new("no_fm");
    let p = s.file("plain.md", "# Just a title\n\nBody.\n");
    let (code, out, _) = run(&s, &["verify", p.to_str().unwrap()]);
    assert_eq!(code, 1, "no frontmatter is an error");
    assert!(
        out.contains("no-frontmatter"),
        "human render should mention no-frontmatter; got {out:?}"
    );
}

#[test]
fn verify_non_md_exit_one() {
    let s = Setup::new("txt");
    let p = s.file("page.txt", "---\ntype: note\ntitle: x\n---\n\n# x\n");
    let (code, out, _) = run(&s, &["verify", p.to_str().unwrap()]);
    assert_eq!(code, 1, "non-md is an error");
    assert!(
        out.contains("non-md"),
        "human render should flag non-md extension; got {out:?}"
    );
}

#[test]
fn verify_directory_exit_two() {
    let s = Setup::new("dir");
    let (code, _, err) = run(&s, &["verify", "."]);
    assert_eq!(code, 2, "directory is a usage error");
    assert!(
        err.to_lowercase().contains("directory"),
        "stderr should explain: {err:?}"
    );
}

#[test]
fn verify_missing_exit_two() {
    let s = Setup::new("ghost");
    let (code, _, err) = run(&s, &["verify", "does_not_exist.md"]);
    assert_eq!(code, 2, "missing file is a usage error");
    assert!(
        !err.is_empty(),
        "stderr should carry an error message: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// --strict: warnings escalate
// ---------------------------------------------------------------------------

#[test]
fn verify_strict_escalates_description_warning() {
    let s = Setup::new("strict");
    let long = "x".repeat(300);
    let p = s.file(
        "long.md",
        &format!("---\ntype: note\ntitle: Page\ndescription: {long}\n---\n\n# Page\n"),
    );
    let (loose, _, _) = run(&s, &["verify", p.to_str().unwrap()]);
    assert_eq!(loose, 0, "warning (without --strict) is still 0");

    let (strict, out, _) = run(&s, &["verify", "--strict", p.to_str().unwrap()]);
    assert_eq!(strict, 1, "--strict escalates to 1");
    assert!(
        out.contains("description-too-long"),
        "human render should list the warning; got {out:?}"
    );
    assert!(
        out.contains("ok-with-warnings"),
        "verdict should be ok-with-warnings; got {out:?}"
    );
}

// ---------------------------------------------------------------------------
// slug-fallback: H1 and filename
// ---------------------------------------------------------------------------

#[test]
fn verify_via_h1_fallback() {
    let s = Setup::new("h1");
    let p = s.file("untitled.md", "---\ntype: note\n---\n\n# My Page\n");
    let (code, out, _) = run(&s, &["verify", p.to_str().unwrap()]);
    assert_eq!(code, 0, "H1 title satisfies slug");
    assert!(
        out.contains("My Page"),
        "human render should surface the slug; got {out:?}"
    );
    assert!(
        out.contains("(h1)"),
        "should mark the source as h1; got {out:?}"
    );
}

#[test]
fn verify_via_filename_fallback() {
    let s = Setup::new("fname");
    let p = s.file(
        "my-note.md",
        "---\ntype: note\n---\n\nJust a body, no H1, no title field.\n",
    );
    let (code, out, _) = run(&s, &["verify", p.to_str().unwrap()]);
    assert_eq!(code, 0, "filename stem satisfies slug");
    assert!(
        out.contains("my-note"),
        "human render should surface the slug; got {out:?}"
    );
    assert!(
        out.contains("(filename)"),
        "should mark the source as filename; got {out:?}"
    );
}

// ---------------------------------------------------------------------------
// --json
// ---------------------------------------------------------------------------

#[test]
fn verify_json_is_one_well_formed_object() {
    let s = Setup::new("jsonok");
    let p = s.file(
        "page.md",
        "---\ntype: wiki\ntitle: JSON Test\ndescription: d\n---\n\n# JSON Test\n",
    );
    let (code, out, err) = run(&s, &["verify", "--json", p.to_str().unwrap()]);
    assert_eq!(code, 0, "exit 0; stderr: {err}");
    assert_json_object(&out, "verify --json");
    // The key shape is documented in FEATURES-verify.md.
    for key in ["file", "verdict", "exit", "checks"] {
        let needle = format!("\"{key}\"");
        assert!(
            out.contains(needle.as_str()),
            "missing key {key} in JSON: {out:?}"
        );
    }
    assert!(
        out.contains("\"verdict\": \"ok\""),
        "verdict should be 'ok' in JSON: {out:?}"
    );
}

#[test]
fn verify_json_error_lists_errors_array() {
    let s = Setup::new("jsonerr");
    let p = s.file("bad.md", "---\ntype: labbel\n---\n\n# x\n"); // unknown-type
    let (code, out, err) = run(&s, &["verify", "--json", p.to_str().unwrap()]);
    assert_eq!(code, 1, "stderr: {err}");
    assert_json_object(&out, "verify --json (error case)");
    assert!(
        out.contains("\"verdict\": \"error\""),
        "verdict should be 'error': {out:?}"
    );
    assert!(
        out.contains("\"unknown-type\""),
        "errors array should list unknown-type: {out:?}"
    );
}

#[test]
fn verify_json_warning_lists_warnings_array() {
    let s = Setup::new("jsonwarn");
    let long = "x".repeat(300);
    let p = s.file(
        "warn.md",
        &format!("---\ntype: note\ntitle: Page\ndescription: {long}\n---\n\n# Page\n"),
    );
    let (code, out, err) = run(&s, &["verify", "--json", p.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_json_object(&out, "verify --json (warning case)");
    assert!(
        out.contains("\"verdict\": \"ok-with-warnings\""),
        "verdict: {out:?}"
    );
    assert!(
        out.contains("\"description-too-long\""),
        "warnings array: {out:?}"
    );
}

#[test]
fn verify_strict_json_bumps_exit() {
    let s = Setup::new("strictjson");
    let long = "x".repeat(300);
    let p = s.file(
        "warn.md",
        &format!("---\ntype: note\ntitle: Page\ndescription: {long}\n---\n\n# Page\n"),
    );
    let (code, out, err) = run(&s, &["verify", "--json", "--strict", p.to_str().unwrap()]);
    assert_eq!(code, 1, "strict + warning exits 1; stderr: {err}");
    assert!(
        out.contains("\"exit\": 1"),
        "JSON should echo the exit code: {out:?}"
    );
}

// ---------------------------------------------------------------------------
// --db flag (Phase 2 uses it to load the corpus for link resolution)
// ---------------------------------------------------------------------------

#[test]
fn verify_accepts_a_db_flag() {
    let s = Setup::new("dbflag");
    let p = s.file("page.md", "---\ntype: note\ntitle: Page\n---\n\n# Page\n");
    let (code, _out, err) = run(
        &s,
        &[
            "verify",
            "--db",
            "/tmp/kb_verify_nonexistent.db",
            p.to_str().unwrap(),
        ],
    );
    assert_eq!(
        code, 0,
        "--db is parsed and feeds the corpus load; stderr: {err}"
    );
}

// ---------------------------------------------------------------------------
// Phase 2 — check d (links) + e (label)
// ---------------------------------------------------------------------------

/// Register the setup root in a fresh per-test DB so the link resolver
/// has a corpus to look against. Returns nothing; the DB lives at
/// `root/test.db` and is torn down with the setup dir.
fn with_corpus(s: &Setup) -> PathBuf {
    let db = s.root.join("test.db");
    let root = s.root.to_str().unwrap();
    // `add` subcommand first, then `--db` after it (clap 4.5 global-flag
    // quirk: `--db X sub …` with the space form doesn't apply the value).
    let (code, _out, err) = run(s, &["add", root, "--db", db.to_str().unwrap()]);
    assert_eq!(code, 0, "kb add must succeed on the test tree: {err}");
    db
}

/// The DB path is a local file, so `run`'s positional layout is `--db <p> <verb> …`.
fn with_db(s: &Setup, args: &[&str]) -> (i32, String, String) {
    let db = s.root.join("test.db");
    // Reorder: put `--db` after the subcommand (clap 4.5 global-flag
    // quirk with space-style `--db X sub …` doesn't work;
    // `sub --db X …` does).
    let subcmd = args[0];
    let rest = &args[1..];
    let mut full: Vec<&str> = Vec::new();
    full.push(subcmd);
    full.push("--db");
    full.push(db.to_str().unwrap());
    full.extend_from_slice(rest);
    let mut cmd = kb();
    for x in &full {
        cmd.arg(x);
    }
    cmd.current_dir(&s.root);
    let out = cmd.output().expect("failed to run kb");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn verify_phase2_dead_link_warns() {
    let s = Setup::new("p2dead");
    let page = s.file(
        "page.md",
        "---\ntype: wiki\ntitle: P2 Dead\n---\n\n# P2 Dead\n\nSee [[does-not-exist]] here.\n",
    );
    with_corpus(&s);
    let (code, out, _err) = with_db(&s, &["verify", page.to_str().unwrap()]);
    assert_eq!(code, 0, "dead link is a warning, not an error; got {out:?}");
    assert!(
        out.contains("dead-link"),
        "warnings should include dead-link; got {out:?}"
    );
    assert!(
        out.contains("does-not-exist"),
        "human render should name the dead target; got {out:?}"
    );
    assert!(
        out.contains("ok-with-warnings"),
        "verdict should be ok-with-warnings; got {out:?}"
    );
}

#[test]
fn verify_phase2_strict_escalates_dead_link() {
    let s = Setup::new("p2strict");
    let page = s.file(
        "page.md",
        "---\ntype: wiki\ntitle: P2 Strict\n---\n\n# P2 Strict\n\nSee [[nonexistent-thing]].\n",
    );
    with_corpus(&s);
    let (code, out, _err) = with_db(&s, &["verify", "--strict", page.to_str().unwrap()]);
    assert_eq!(
        code, 1,
        "--strict must escalate dead-link to exit 1; got {out:?}"
    );
}

#[test]
fn verify_phase2_resolved_link_does_not_warn() {
    let s = Setup::new("p2ok");
    s.file(
        "target.md",
        "---\ntype: wiki\ntitle: Resolved Target\n---\n\n# Resolved Target\n",
    );
    let page = s.file(
        "linker.md",
        "---\ntype: wiki\ntitle: Linker\n---\n\n# Linker\n\nThis points to [[Resolved Target]].\n",
    );
    with_corpus(&s);
    let (code, out, _err) = with_db(&s, &["verify", page.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(
        !out.contains("dead-link"),
        "a resolved link must not produce a dead-link warning; got {out:?}"
    );
    assert!(
        out.contains("1 resolved"),
        "human render should count the resolved link; got {out:?}"
    );
}

#[test]
fn verify_phase2_ambiguous_link_warns() {
    let s = Setup::new("p2amb");
    // Two pages with the same slug `Same` → `[[Same]]` is ambiguous.
    s.file(
        "same-a.md",
        "---\ntype: wiki\ntitle: Same\n---\n\n# Same A\n",
    );
    s.file(
        "same-b.md",
        "---\ntype: wiki\ntitle: Same\n---\n\n# Same B\n",
    );
    let page = s.file(
        "linker.md",
        "---\ntype: wiki\ntitle: Linker\n---\n\n# Linker\n\nSee [[Same]].\n",
    );
    with_corpus(&s);
    let (code, out, _err) = with_db(&s, &["verify", page.to_str().unwrap()]);
    assert_eq!(code, 0, "ambiguous is a warning, not an error; got {out:?}");
    assert!(
        out.contains("ambiguous-link"),
        "warnings should include ambiguous-link; got {out:?}"
    );
}

#[test]
fn verify_phase2_label_files_present() {
    let s = Setup::new("p2label");
    s.file("real1.txt", "x\n");
    let label = s.file(
        "proxy.md",
        "---\ntype: label\ntitle: Proxy\n---\n\n# Proxy\n\n## Files\nreal1.txt\n",
    );
    with_corpus(&s);
    let (code, out, _err) = with_db(&s, &["verify", label.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "a label naming an existing file is clean; got {out:?}"
    );
    assert!(
        out.contains("label:") && out.contains("files"),
        "human render should report the label as applying to files; got {out:?}"
    );
    assert!(
        out.contains("all present"),
        "label render should note all files present; got {out:?}"
    );
}

#[test]
fn verify_phase2_label_missing_file_warns() {
    let s = Setup::new("p2labelmiss");
    let label = s.file(
        "proxy.md",
        "---\ntype: label\ntitle: Proxy\n---\n\n# Proxy\n\n## Files\nnope.txt\n",
    );
    with_corpus(&s);
    let (code, out, _err) = with_db(&s, &["verify", label.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "label-file-missing is a warning, not an error; got {out:?}"
    );
    assert!(
        out.contains("label-file-missing:nope.txt"),
        "warning should name the missing file; got {out:?}"
    );
    assert!(
        out.contains("missing: nope.txt"),
        "label render should list missing; got {out:?}"
    );
    let (scode, sout, _err) = with_db(&s, &["verify", "--strict", label.to_str().unwrap()]);
    assert_eq!(
        scode, 1,
        "--strict escalates label-file-missing; got {sout:?}"
    );
}

#[test]
fn verify_phase2_label_folder_wide() {
    let s = Setup::new("p2folder");
    s.file("seed.txt", "seed\n");
    let label = s.file(
        "proxy.md",
        "---\ntype: label\ntitle: Folder proxy\n---\n\n# Folder proxy\n\nAll the files under me.\n",
    );
    with_corpus(&s);
    let (code, out, _err) = with_db(&s, &["verify", label.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(
        out.contains("folder-wide"),
        "label render should note folder-wide; got {out:?}"
    );
}

#[test]
fn verify_phase2_json_links_shape() {
    let s = Setup::new("p2json");
    let page = s.file(
        "page.md",
        "---\ntype: wiki\ntitle: P2 JSON\n---\n\n# P2 JSON\n\nOne [[missing-thing]].\n",
    );
    with_corpus(&s);
    let (code, out, err) = with_db(&s, &["verify", "--json", page.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_json_object(&out, "p2 link json");
    for k in [
        "\"links\"",
        "\"total\"",
        "\"resolved\"",
        "\"dead\"",
        "\"ambiguous\"",
    ] {
        assert!(out.contains(k), "missing {k} in p2 link JSON: {out:?}");
    }
    assert!(
        out.contains("\"missing-thing\""),
        "dead-link JSON should name the target; got {out:?}"
    );
    assert!(
        out.contains("\"style\":\"wikilink\""),
        "dead-link JSON should include the style; got {out:?}"
    );
}

#[test]
fn verify_phase2_json_label_shape() {
    let s = Setup::new("p2labeljson");
    let label = s.file(
        "proxy.md",
        "---\ntype: label\ntitle: P2 Label JSON\n---\n\n# P2 Label JSON\n\n## Files\nnope.txt\n",
    );
    with_corpus(&s);
    let (code, out, err) = with_db(&s, &["verify", "--json", label.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_json_object(&out, "p2 label json");
    assert!(out.contains("\"label\""), "missing label key: {out:?}");
    assert!(
        out.contains("\"applies\": \"files\""),
        "label JSON should say files; got {out:?}"
    );
    assert!(
        out.contains("\"missing\"") && out.contains("nope.txt"),
        "label JSON should list the missing file; got {out:?}"
    );
}

#[test]
fn verify_phase2_json_strict_bumps_exit() {
    let s = Setup::new("p2strictjson");
    let page = s.file(
        "page.md",
        "---\ntype: wiki\ntitle: P2 Strict JSON\n---\n\n# P2 Strict JSON\n\nOne [[nope]] link.\n",
    );
    with_corpus(&s);
    let (code, out, err) = with_db(
        &s,
        &["verify", "--json", "--strict", page.to_str().unwrap()],
    );
    assert_eq!(code, 1, "stderr: {err}");
    assert!(
        out.contains("\"exit\": 1"),
        "JSON should echo exit 1; got {out:?}"
    );
}

/// Verify the *non-label* branch on a label page (`label: applies: na` is
/// impossible but the type:label + no-listing case → applies: folder)
#[test]
fn verify_phase2_label_empty_body_is_folder() {
    let s = Setup::new("p2emptylabel");
    let label = s.file("empty.md", "---\ntype: label\ntitle: Empty label\n---\n\n");
    with_corpus(&s);
    let (code, out, _err) = with_db(&s, &["verify", label.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(
        out.contains("folder-wide"),
        "a type:label page with no ## files listing is folder-wide; got {out:?}"
    );
}
