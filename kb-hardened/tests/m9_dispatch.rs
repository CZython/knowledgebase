//! F4 — hardened build drops the `.md`→`info` heuristic.
//!
//! In the user (interactive) build, `kb <path>.md` (an existing file) is a
//! convenience shortcut for `kb info <path>.md`. In the hardened (agent)
//! build, that CWD-relative `Path::exists()` probe is dropped — every bare
//! token routes to `search`, giving a deterministic surface (F4, option b).
//!
//! These tests drive the real `kb-hardened` and `kb` binaries as subprocesses
//! to pin the divergence.
//!
//! Run: `cargo test -p kb-hardened --test m9_dispatch`

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn hardened_bin() -> Command {
    Command::new(
        std::env::var("CARGO_BIN_EXE_kb-hardened").expect("CARGO_BIN_EXE_kb-hardened unset"),
    )
}

struct Setup {
    root: PathBuf,
    db: PathBuf,
}

impl Setup {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("kbdsp_{pid}_{tag}", pid = std::process::id()));
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

fn run(
    cmd: &mut Command,
    db: &std::path::Path,
    args: &[&str],
) -> (i32, String, String) {
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

/// In the **hardened** build, a bare token that is an existing `.md` file
/// path routes to `search` (not `info`), so the output is a search result
/// list (`[]` or rows) — never the info metadata block. The FTS5 syntax
/// error on `/` in the query text *proves* the token reached the search
/// verb (info would have found it or returned a "no such page" error).
#[test]
fn hardened_bare_md_routes_to_search_not_info() {
    let s = Setup::new("hard_md");
    write_page(&s.root, "note.md", "some body text here\n");

    // Index the root so `info` *would* succeed (to isolate the routing).
    let _ = hardened_bin()
        .arg(format!("--db={}", s.db.display()))
        .arg("add")
        .arg(s.root.to_str().unwrap())
        .status();

    // `kb-hardened <absolute/path>.md` — the token is an existing .md file.
    // The hardened shortcut is dropped (F4 b), so this must be `search`.
    let file = s.root.join("note.md");
    let path_str = file.to_str().unwrap().to_string();
    let (code, out, err) = {
        let mut c = hardened_bin();
        run(&mut c, &s.db, &[path_str.as_str()])
    };

    // A search on an absolute path containing `/` will either:
    //   (a) succeed with 0 results (rc 0, "0 results"), or
    //   (b) fail with an FTS5 syntax error on `/` (rc != 0, "fts5").
    // Either way: it is NOT the info output (which would print
    // `title:`, `type:`, `desc:`, `links:`, etc.).
    let is_info = out.contains("title:") && out.contains("type:");
    assert!(
        !is_info,
        "hardened bare .md must NOT route to info — got output:\n{out}\nstderr: {err}"
    );
    let looks_like_search_err = err.contains("fts5") || err.contains("syntax error");
    let looks_like_search_0 = code == 0 && (out.contains("0 results") || out.trim() == "[]");
    assert!(
        looks_like_search_err || looks_like_search_0,
        "hardened bare .md must route to search; rc={code} out={out} err={err}"
    );
}

// NOTE — `user_bare_md_routes_to_info` was removed from this file: it was
// gated on `CARGO_BIN_EXE_kb`, which is only set when the `kb-user`
// package is part of the test build graph. `kb-hardened` does NOT depend
// on `kb-user`, so under `cargo test -p kb-hardened` (and even
// `--workspace`) the env var is unset and the test **silently early-
// returned**, counting as "passing" while asserting zero. The invariant
// is now pinned in `kb-user/tests/m9_dispatch_user.rs`, where
// `CARGO_BIN_EXE_kb` is always available and the test therefore runs on
// every gate invocation.

/// In **both** builds, a bare non-`.md` token (e.g. a search term) routes
/// to `search`.
#[test]
fn both_bare_non_md_routes_to_search() {
    let s = Setup::new("nonmd");
    write_page(&s.root, "note.md", "gatedword alpha beta\n");

    let mut add = hardened_bin();
    let _ = add
        .arg(format!("--db={}", s.db.display()))
        .arg("add")
        .arg(s.root.to_str().unwrap())
        .status();

    // Bare search term (no .md, no /) — should be `search`.
    let (code, out, err) = {
        let mut c = hardened_bin();
        run(&mut c, &s.db, &["gatedword"])
    };
    assert_eq!(code, 0, "search should succeed; err: {err}");
    assert!(
        out.contains("gatedword") || out.contains("alpha") || out.contains("note.md"),
        "search results should include the page; got: {out}"
    );
}

/// In the **hardened** build the `editor` feature is off, so the
/// `create -e` flag (which spawns `$VISUAL`/`$EDITOR`) is never
/// registered. clap must reject `-e` as an *unexpected argument*
/// (exit 2, before any file is written). The invariant was previously
/// pinned by `kb-user/tests/create_cli.rs::create_e_flag_is_rejected_in_
/// hardened_build`, but that test was `#[cfg(not(feature = "editor"))]`
/// and therefore silently excluded from the gate's `cargo test
/// --workspace` (kb-user defaults `editor` on). Pinning it here, where
/// `editor` is off by default, means the invariant is exercised on
/// every gate run.
#[test]
fn hardened_create_e_flag_is_rejected() {
    let s = Setup::new("create_e_rej");
    let (code, _, err) = {
        let mut c = hardened_bin();
        run(&mut c, &s.db, &["create", "noe", "-e", "--yes"])
    };
    assert_eq!(
        code, 2,
        "clap usage error is exit 2 — not a create failure: {err}"
    );
    assert!(
        err.contains("unexpected argument"),
        "clap should name the offending flag: {err}"
    );
    assert!(
        !s.root.join("noe.md").exists(),
        "rejected before any write"
    );
}

/// In the **hardened** build the `SectionOp::Edit` variant is
/// `#[cfg(feature = "editor")]`-gated out, so `kb-hardened section edit …`
/// must be rejected by clap as an unrecognized subcommand (the "editor
/// flow" is not part of the agent surface). The same invariant was
/// previously pinned by `kb-user/tests/m9_agent_surface.rs::section_edit_removed`,
/// but that test was `#[cfg(not(feature = "editor"))]` and therefore
/// silently excluded from the gate's `cargo test --workspace` (kb-user
/// defaults `editor` on). Pinning it here, where `editor` is off by
/// default, means the invariant is exercised on every gate run.
#[test]
fn hardened_section_edit_is_rejected() {
    let s = Setup::new("sec_edit");
    write_page(&s.root, "doc.md", "# Sec\n\nx\n");

    let file = s.root.join("doc.md");
    let file_str = file.to_str().unwrap().to_string();
    let (code, _, err) = {
        let mut c = hardened_bin();
        run(
            &mut c,
            &s.db,
            &[
                "section",
                "--file",
                file_str.as_str(),
                "edit",
                "Sec",
                "--yes",
            ],
        )
    };
    assert_ne!(
        code, 0,
        "`section edit` is not compiled into the hardened build; clap should reject it (rc 2 typically). stderr: {err}"
    );
    // Clap rejects the unknown subcommand by naming it.
    assert!(
        err.contains("edit") || err.contains("unrecognized") || err.contains("error"),
        "clap should mention 'edit' or 'unrecognized' in the rejection stderr; got: {err}"
    );
}
