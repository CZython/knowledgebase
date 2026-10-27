//! `kb create` — scaffold a new `.md` page and index it (PLAN §11).
//!
//! Drives the real `kb` binary as a subprocess against a throwaway root +
//! DB, mirroring `m7_multifile.rs`: the write contract (diff, confirm,
//! `--dry-run`, `--yes`, `--json`), the path rules (`--path`, `.md`
//! appending, existing-file refusal), the `-e` editor flow, and the
//! **best-effort indexing** rule (skip with a note when there are no roots,
//! when the page is outside every root, or when the final bytes are not
//! indexable — never an error).
//!
//! Run: `cargo test -p kb-user --test create_cli`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn kb() -> Command {
    Command::new(
        std::env::var("CARGO_BIN_EXE_kb").expect("CARGO_BIN_EXE_kb unset — run under cargo test"),
    )
}

struct Setup {
    root: PathBuf,
    db: PathBuf,
}

impl Setup {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("kbcreate_{pid}_{tag}", pid = std::process::id()));
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

/// Run `kb --db … <args…>` with cwd + env, returning `(code, stdout, stderr)`.
fn run_in(
    setup: &Setup,
    cwd: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> (i32, String, String) {
    let db = setup.db.to_string_lossy().into_owned();
    let mut cmd = kb();
    cmd.arg(format!("--db={db}"));
    for x in args {
        cmd.arg(x);
    }
    cmd.current_dir(cwd);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run kb");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run(setup: &Setup, args: &[&str]) -> (i32, String, String) {
    run_in(setup, &setup.root, args, &[])
}

/// Register the throwaway root.
fn add(setup: &Setup) {
    let root = setup.root.to_string_lossy().into_owned();
    let (code, _, err) = run(setup, &["add", root.as_str()]);
    assert_eq!(code, 0, "kb add failed: {err}");
}

/// Validate that `s` parses as a single JSON *object*.
fn assert_json(s: &str, what: &str) {
    // W6: the interpreter is `python3` on POSIX but `python` on Windows.
    let py = if cfg!(windows) { "python" } else { "python3" };
    let mut v = Command::new(py);
    v.arg("-c").arg(
        "import json,sys\n\
         d = json.loads(sys.stdin.read())\n\
         assert isinstance(d, dict), type(d).__name__\n\
         print(','.join(d.keys()))",
    );
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
// scaffold shape / path rules
// ---------------------------------------------------------------------------

#[test]
fn create_writes_the_full_field_scaffold() {
    let s = Setup::new("scaffold");
    let (code, out, err) = run(&s, &["create", "my-note", "--yes"]);
    assert_eq!(code, 0, "stderr: {err}");
    let p = s.root.join("my-note.md");
    let body = fs::read_to_string(&p).unwrap();
    // PLAN §4: `type:` + `title:` required in spirit (title = slug source),
    // the optional fields present-but-empty so they are ready to fill.
    assert!(body.starts_with("---\n"), "opens with a frame: {body:?}");
    assert!(
        body.contains("type: wiki\n"),
        "default type is wiki: {body:?}"
    );
    assert!(
        body.contains("title: my-note\n"),
        "title from the file stem"
    );
    assert!(body.contains("description:\n"), "empty description slot");
    assert!(body.contains("tags: []\n"), "empty tag list");
    assert!(
        body.contains("search_required_phrase: []\n"),
        "empty gate list"
    );
    assert!(
        body.contains("\n---\n\n# my-note\n\n"),
        "H1 after the frame"
    );
    assert!(out.contains("created"), "human note: {out}");
    assert!(
        out.starts_with("--- /dev/null\n"),
        "a new-file diff is shown: {out}"
    );
}

#[test]
fn create_appends_md_and_builds_missing_dirs() {
    let s = Setup::new("paths");
    let (code, _, err) = run(
        &s,
        &[
            "create",
            "deep/sub/note",
            "--path",
            s.root.to_str().unwrap(),
            "--type",
            "note",
            "--yes",
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(s.root.join("deep/sub/note.md").is_file());
    let body = fs::read_to_string(s.root.join("deep/sub/note.md")).unwrap();
    assert!(body.contains("type: note\n"), "{body}");
}

#[test]
fn create_type_is_case_insensitive_and_validated() {
    let s = Setup::new("type");
    let (code, _, err) = run(&s, &["create", "lab", "--type", "LABEL", "--yes"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(fs::read_to_string(s.root.join("lab.md"))
        .unwrap()
        .contains("type: label\n"));

    let (code, _, err) = run(&s, &["create", "bad", "--type", "wiki-page", "--yes"]);
    assert_eq!(code, 1, "unknown type must be rejected: {err}");
    assert!(!s.root.join("bad.md").exists());
}

#[test]
fn create_rejects_a_non_md_extension() {
    let s = Setup::new("ext");
    let (code, _, err) = run(&s, &["create", "notes.txt", "--yes"]);
    assert_eq!(code, 1, "stderr: {err}");
    assert!(err.contains(".md"), "explains the rule: {err}");
    assert!(!s.root.join("notes.txt").exists());
}

#[test]
fn create_refuses_to_clobber_an_existing_file() {
    let s = Setup::new("clobber");
    fs::write(s.root.join("keep.md"), "ORIGINAL\n").unwrap();
    let (code, _, err) = run(&s, &["create", "keep", "--yes"]);
    assert_eq!(code, 1, "stderr: {err}");
    assert!(err.contains("already exists"), "{err}");
    assert_eq!(
        fs::read_to_string(s.root.join("keep.md")).unwrap(),
        "ORIGINAL\n"
    );
}

// ---------------------------------------------------------------------------
// write contract: --dry-run / confirm / --json
// ---------------------------------------------------------------------------

#[test]
fn create_dry_run_writes_nothing() {
    let s = Setup::new("dryrun");
    let (code, out, err) = run(&s, &["create", "ghost", "--dry-run", "--yes"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(!s.root.join("ghost.md").exists());
    assert!(
        out.starts_with("--- /dev/null\n"),
        "diff still shown: {out}"
    );
    assert!(err.contains("dry-run"), "{err}");

    let (code, out, err) = run(&s, &["create", "ghost", "--dry-run", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_json(&out, "create --dry-run --json");
    assert!(out.contains("\"dry_run\": true"), "{out}");
    assert!(out.contains("\"created\": false"), "{out}");
    assert!(!s.root.join("ghost.md").exists());
}

#[test]
fn create_prompts_without_yes_and_leaves_nothing_behind() {
    let s = Setup::new("confirm");
    // Non-interactive stdin is never auto-approved (the `commit` contract).
    let (code, _, err) = run(&s, &["create", "nope"]);
    assert_eq!(code, 1, "stderr: {err}");
    assert!(err.contains("aborted"), "{err}");
    assert!(!s.root.join("nope.md").exists());
}

#[test]
fn create_json_is_one_object_on_stdout() {
    let s = Setup::new("json");
    let (code, out, err) = run(&s, &["create", "meta", "--json", "--yes"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_json(&out, "create --json");
    for k in [
        "file", "type", "title", "id", "created", "edited", "indexed", "dry_run", "bytes", "diff",
    ] {
        assert!(out.contains(&format!("\"{k}\":")), "key {k} present: {out}");
    }
    // No roots → the file exists, nothing is indexed, `id` is null.
    assert!(out.contains("\"indexed\": false"), "{out}");
    assert!(out.contains("\"id\": null"), "{out}");
    assert!(s.root.join("meta.md").is_file());
    // The `file` is absolute, so an agent does not re-anchor it on the cwd.
    assert!(
        out.contains(&format!(
            "\"file\": \"{}",
            s.root
                .join("meta.md")
                .to_str()
                .unwrap()
                .replace('\\', "\\\\")
        )),
        "{out}"
    );
}

// ---------------------------------------------------------------------------
// indexing: best-effort, never fatal
// ---------------------------------------------------------------------------

#[test]
fn create_indexes_a_page_under_a_registered_root() {
    let s = Setup::new("indexed");
    add(&s);
    let (code, out, err) = run(&s, &["create", "attention", "--json", "--yes"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_json(&out, "create --json");
    assert!(out.contains("\"indexed\": true"), "{out}");
    assert!(out.contains("\"id\": \"wiki:attention\""), "{out}");

    // …and the row is queryable through the normal verbs.
    let (code, out, err) = run(&s, &["search", "attention"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("wiki:attention"), "searchable: {out}");

    let (code, out, err) = run(&s, &["info", s.root.join("attention.md").to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("id:      wiki:attention"), "{out}");
}

#[test]
fn create_skips_indexing_when_no_roots_are_registered() {
    let s = Setup::new("noroots");
    let (code, out, err) = run(&s, &["create", "loose", "--json", "--yes"]);
    assert_eq!(code, 0, "skip must not be an error: {err}");
    assert_json(&out, "create --json");
    assert!(out.contains("\"indexed\": false"), "{out}");
    assert!(err.contains("no roots registered"), "stderr note: {err}");
    assert!(
        s.root.join("loose.md").is_file(),
        "the page is still written"
    );
}

#[test]
fn create_skips_indexing_outside_every_registered_root() {
    let s = Setup::new("outroot");
    add(&s);
    // A sibling directory that is *not* under the registered root.
    let outside = s.root.parent().unwrap().join("outside");
    let _ = fs::remove_dir_all(&outside);
    fs::create_dir_all(&outside).unwrap();
    let (code, out, err) = run_in(&s, &outside, &["create", "stray", "--json", "--yes"], &[]);
    let _ = fs::remove_dir_all(&outside);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("\"indexed\": false"), "{out}");
    assert!(err.contains("not under any registered root"), "{err}");
}

#[test]
#[cfg(feature = "editor")]
#[cfg(unix)]
fn create_indexes_the_bytes_the_editor_saved() {
    let s = Setup::new("edit");
    add(&s);
    // A stand-in editor: rewrite `type: wiki` → `note` and give it a tag,
    // so the *post-edit* frontmatter is what must reach the index.
    let script = s.root.parent().unwrap().join("ed.sh");
    fs::write(
        &script,
        "#!/bin/sh\nsed -i -e 's/^type: wiki$/type: note/' -e 's/^tags: \\[\\]$/tags:\\n  - edited/' \"$1\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ed = script.to_string_lossy().into_owned();
    let (code, _, err) = run_in(
        &s,
        &s.root,
        &["create", "edited-me", "-e", "--yes"],
        &[("EDITOR", ed.as_str())],
    );
    let _ = fs::remove_file(&script);
    assert_eq!(code, 0, "stderr: {err}");

    let body = fs::read_to_string(s.root.join("edited-me.md")).unwrap();
    assert!(
        body.contains("type: note\n"),
        "editor's bytes are on disk: {body}"
    );
    assert!(body.contains("- edited"), "{body}");

    // The index reflects the edited type + tag, not the scaffold.
    let (code, out, err) = run(&s, &["info", s.root.join("edited-me.md").to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("id:      note:edited-me"), "{out}");

    let (code, out, err) = run(&s, &["tag", "edited"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("edited-me"),
        "tag collected from the edited bytes: {out}"
    );
}

#[test]
#[cfg(feature = "editor")]
fn create_edit_failure_leaves_the_scaffold_and_indexes_nothing() {
    let s = Setup::new("editfail");
    add(&s);
    let (code, _, err) = run_in(
        &s,
        &s.root,
        &["create", "boom", "-e", "--yes"],
        &[("EDITOR", "false")],
    );
    assert_eq!(code, 1, "stderr: {err}");
    assert!(err.contains("exited"), "{err}");
    // The scaffold is the source of truth and survives; it is simply not
    // indexed yet (`kb refresh` will pick it up once it is fixed).
    assert!(s.root.join("boom.md").is_file());
    let (code, _, err) = run(&s, &["info", s.root.join("boom.md").to_str().unwrap()]);
    assert_ne!(code, 0, "must not be indexed: {err}");
}

#[test]
#[cfg(feature = "editor")]
fn create_edit_with_json_is_rejected() {
    let s = Setup::new("editjson");
    let (code, _, err) = run(&s, &["create", "nope", "-e", "--json", "--yes"]);
    assert_eq!(code, 1, "stderr: {err}");
    assert!(err.contains("--edit"), "names the offending flag: {err}");
    assert!(
        !s.root.join("nope.md").exists(),
        "rejected before any write"
    );
}

// NOTE — `create_e_flag_is_rejected_in_hardened_build` was removed from this
// file: it was `#[cfg(not(feature = "editor"))]` and therefore silently
// excluded from the gate's `cargo test --workspace` (kb-user defaults
// `editor` on). The invariant is now pinned in
// `kb-hardened/tests/m9_dispatch.rs::hardened_create_e_flag_is_rejected`,
// where `editor` is off by default and the test therefore runs on every
// gate invocation.
