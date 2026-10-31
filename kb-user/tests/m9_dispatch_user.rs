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
        let base =
            std::env::temp_dir().join(format!("kbdspu_{pid}_{tag}", pid = std::process::id()));
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

    assert_eq!(
        code, 0,
        "user build bare .md should route to info (rc 0); err: {err}"
    );
    assert!(
        out.contains("title:") || out.contains("type:"),
        "user build bare .md should print info metadata; got: {out}\nerr: {err}"
    );
    assert!(
        !out.contains("0 results"),
        "user build bare .md should NOT be treated as a search; got: {out}"
    );
}

// ==================== M10 completions surface (user build) ====================
//
// The `completions` feature is ON by default in `kb-user`, so `kb complete`
// is present here and absent in the hardened build (pinned in
// `kb-hardened/tests/m9_dispatch.rs`). These tests pin the user-side
// half of the divergence: the verb exists, emits a valid completion
// script for each accepted shell, rejects unknown shells, and — critically
// — the candidate graft for `search <query>` does NOT constrain the
// runtime free-text parser.

/// `kb complete bash` emits a bash completion script that names the `kb`
/// command's subcommands. The script must not be empty and must reference
/// the `complete` subcommand it is generating (clap_complete names the
/// program).
#[test]
fn complete_bash_emits_script() {
    let s = Setup::new("cmp_bash");
    let (code, out, err) = {
        let mut c = user_bin();
        run(&mut c, &s.db, &["complete", "bash"])
    };
    assert_eq!(code, 0, "`kb complete bash` should succeed; err: {err}");
    // A real bash completion script defines a function named after the
    // program. The program name is `kb`.
    assert!(
        out.contains("kb")
            && (out.contains("complete") || out.contains("_kb") || out.contains("compgen")),
        "expected a bash completion script body; got: {out}"
    );
}

/// `kb complete zsh` emits a zsh completion script.
#[test]
fn complete_zsh_emits_script() {
    let s = Setup::new("cmp_zsh");
    let (code, out, err) = {
        let mut c = user_bin();
        run(&mut c, &s.db, &["complete", "zsh"])
    };
    assert_eq!(code, 0, "`kb complete zsh` should succeed; err: {err}");
    // zsh completion scripts typically begin with a `#compdef` directive.
    assert!(
        out.contains("#compdef") || out.contains("kb"),
        "expected a zsh completion script (likely `#compdef`); got: {out}"
    );
}

/// The candidate set grafted onto the *completion* instance of the
/// `search <query>` positional must surface in at least one shell's output —
/// pinning that `cli_command()` actually decorates the parser (rather than
/// no-op'ing). We check the canonical `Null` sentinel is present.
#[test]
fn complete_includes_null_sentinel_candidate() {
    let s = Setup::new("cmp_null");
    let (code, out, err) = {
        let mut c = user_bin();
        run(&mut c, &s.db, &["complete", "zsh"])
    };
    assert_eq!(code, 0, "`kb complete zsh` should succeed; err: {err}");
    assert!(
        out.contains("Null"),
        "zsh completion should include the `Null` absence-sentinel candidates; got: {out}"
    );
}

/// `kb complete fish` is rejected — only `bash` and `zsh` are accepted
/// (the two shells the PLAN scoped). Exit 1 with a named-shell message.
#[test]
fn complete_rejects_unknown_shell() {
    let s = Setup::new("cmp_rej");
    let (code, _, err) = {
        let mut c = user_bin();
        run(&mut c, &s.db, &["complete", "fish"])
    };
    assert_eq!(
        code, 1,
        "unknown shell is a domain error (rc 1), not a usage error: {err}"
    );
    assert!(
        err.contains("fish"),
        "rejection should name the offending shell: {err}"
    );
}

/// `kb complete bash --file <path>` writes the script to the path and prints
/// nothing to stdout.
#[test]
fn complete_writes_file() {
    let s = Setup::new("cmp_file");
    let out_path = s.root.join("kb.bash");
    let out_str = out_path.to_str().unwrap().to_string();
    let (code, out, err) = {
        let mut c = user_bin();
        run(
            &mut c,
            &s.db,
            &["complete", "bash", "--file", out_str.as_str()],
        )
    };
    assert_eq!(
        code, 0,
        "`kb complete bash --file` should succeed; err: {err}"
    );
    assert!(
        out.trim().is_empty(),
        "with `--file` the script goes to the file, not stdout; got stdout: {out}"
    );
    assert!(
        out_path.exists(),
        "script file should have been written at {out_str}"
    );
    let body = fs::read_to_string(&out_path).expect("should read script file");
    assert!(
        body.contains("kb") && !body.is_empty(),
        "written script should be a non-trivial bash completion; got {len} bytes",
        len = body.len()
    );
}

/// The candidate graft is applied to the *completion* command instance only.
/// The **runtime** `search` parser stays free-text: an arbitrary `language:xx`
/// value + a search term must NOT be rejected as an unknown value.
#[test]
fn search_remains_free_text_in_user_build() {
    let s = Setup::new("cmp_free");
    let (code, out, err) = {
        let mut c = user_bin();
        run(&mut c, &s.db, &["search", "language:en transformers"])
    };
    // A free-text query against an empty index is a successful *search*
    // (0 results, rc 0) — NOT a clap/parse error (which would be rc 2 or a
    // named-value rejection on stderr).
    assert_eq!(
        code, 0,
        "free-text search must parse (the graft does not restrict the parser); err: {err}"
    );
    assert!(
        out.contains("0 results") || out.trim() == "[]",
        "free-text search with no matches should report 0 results; got: {out}"
    );
}
