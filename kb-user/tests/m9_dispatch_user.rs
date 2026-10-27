//! F4 — user build keeps the `.md`→`info` bare-token shortcut.
//!
//! Companion to `kb-hardened/tests/m9_dispatch.rs`, where the same
//! invariant is pinned in reverse: the *hardened* build drops the CWD-
//! relative `Path::exists()` probe entirely, so a bare token always
//! routes to `search` (F4 option b). In the *user* build, the shortcut
//! is the interactive convenience — `kb notes/foo.md` opens that file's
//! `info`.
//!
//! Why this lives in `kb-user/tests/`: the test drives the `kb` binary,
//! which is only available via `CARGO_BIN_EXE_kb` when the `kb-user`
//! package is the build subject. Earlier it lived in
//! `kb-hardened/tests/m9_dispatch.rs` with an `if user_bin().is_none()
//! { return; }` guard — but `kb-hardened` does NOT depend on `kb-user`,
//! so the guard fired on every `cargo test --workspace` run and the
//! test silently early-returned (counting as "passing" while asserting
//! zero). Moving it here removes the guard: `CARGO_BIN_EXE_kb` is
//! always set, the test always runs, and the gate exercises the user-
//! side half of the divergence on every invocation.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn user_bin() -> Command {
    Command::new(std::env::var("CARGO_BIN_EXE_kb").expect("CARGO_BIN_EXE_kb unset"))
}

struct Setup {
    root: PathBuf,
    db: PathBuf,
}

impl Setup {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("kbdspu_{pid}_{tag}", pid = std::process::id()));
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
        if let Some(parent) = self.root.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

fn run(cmd: &mut Command, db: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let db_str = db.to_string_lossy().into_owned();
    cmd.arg(format!("--db={db_str}"));
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().expect("failed to run kb");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write_page(root: &std::path::Path, name: &str, body: &str) {
    fs::write(
        root.join(name),
        format!("---\ntype: note\ntitle: {name}\n---\n{body}"),
    )
    .unwrap();
}

/// Pinned here (in `kb-user/tests/`) rather than in
/// `kb-hardened/tests/m9_dispatch.rs` so that `CARGO_BIN_EXE_kb` is
/// always available and the test runs on every gate invocation.
#[test]
fn user_bare_md_routes_to_info() {
    let s = Setup::new("usr_md");
    write_page(&s.root, "note.md", "some body text\n");

    let mut add = user_bin();
    let _ = add
        .arg(format!("--db={}", s.db.display()))
        .arg("add")
        .arg(s.root.to_str().unwrap())
        .status();

    let file = s.root.join("note.md");
    let path_str = file.to_str().unwrap().to_string();
    let mut c = user_bin();
    let (code, out, err) = run(&mut c, &s.db, &[path_str.as_str()]);

    assert_eq!(code, 0, "user build bare .md should route to info (rc 0); err: {err}");
    assert!(
        out.contains("title:") || out.contains("type:"),
        "user build bare .md should print info metadata; got: {out}\nerr: {err}"
    );
    assert!(
        !out.contains("0 results"),
        "user build bare .md should NOT be treated as a search; got: {out}"
    );
}
