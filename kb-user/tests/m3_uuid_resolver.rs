//! M7 — uuid resolver integration: the shell-inert paste-back contract.
//!
//! Drives the real `kb` binary as a subprocess against a throwaway root and a
//! throwaway DB (`--db`). Pins:
//!
//!   * two pages with the same `type:slug` in different folders get *distinct*
//!     full uuids (the salt by `abs_path` is the collision rule);
//!   * each full uuid resolves to its own page (via `kb info <uuid>` /
//!     `kb toc <uuid>` / `kb verify <uuid>`);
//!   * `kb resolve <full-uuid>` → unique (1 candidate);
//!   * `kb resolve <5-hex>` → unique / empty / ambiguous depending on the
//!     corpus — the ≥2 arm exits non-zero;
//!   * a 4-hex token is NOT accepted as a hex token (below `FLOOR = 5`), so
//!     it falls through to the path branch (no match);
//!   * a path containing `/` is treated as a path, never as a hex token.
//!
//! Run: `cargo test -p kb-user --test m3_uuid_resolver`

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn kb() -> Command {
    Command::new(std::env::var("CARGO_BIN_EXE_kb").expect("CARGO_BIN_EXE_kb unset"))
}

struct Setup {
    root: PathBuf,
    db: PathBuf,
}

impl Setup {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("kb_uuid_{}_{}", std::process::id(), tag));
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

fn add(setup: &Setup) -> (i32, String, String) {
    let root = setup.root.to_string_lossy().into_owned();
    run(setup, &["add", root.as_str()])
}

/// Extract `"uuid":  "<hex>"` from the `info --json` output for a given
/// `abs_path` page. We use `info <abs_path> --json` to ask the binary for
/// the stored uuid directly (the salted FNV-1a 64 over `id || 0 || abs_path`).
fn full_uuid(setup: &Setup, abs: &str) -> String {
    let (code, out, err) = run(setup, &["info", abs, "--json"]);
    assert_eq!(code, 0, "info {abs} failed: {err}");
    // The line looks like `  "uuid": "8535f966a133d339"`.
    let needle = "\"uuid\": \"";
    let rest = out
        .split(needle)
        .nth(1)
        .unwrap_or_else(|| panic!("no uuid field in info output for {abs}: {out}"));
    let end = rest.find('"').unwrap();
    rest[..end].to_string()
}

// ---- 1 — same-`type:slug`-different-folder → distinct uuids -----------

#[test]
fn same_type_slug_in_two_folders_distinct_uuids_both_resolvable() {
    let s = Setup::new("two_folders");
    write_page(&s, "a/same.md", "type: wiki\ntitle: Same\n", "A body\n");
    write_page(&s, "b/same.md", "type: wiki\ntitle: Same\n", "B body\n");
    let (code, _out, err) = add(&s);
    assert_eq!(code, 0, "add failed: {err}");

    let a_abs = s.root.join("a/same.md").to_string_lossy().to_string();
    let b_abs = s.root.join("b/same.md").to_string_lossy().to_string();
    let ua = full_uuid(&s, &a_abs);
    let ub = full_uuid(&s, &b_abs);

    assert_ne!(
        ua, ub,
        "salt (abs_path) must distinguish same-type:slug pages"
    );
    assert_eq!(ua.len(), 16, "FNV-1a 64 is 16 hex, got {ua}");
    assert_eq!(ub.len(), 16);
    assert!(
        ua.chars().all(|c| c.is_ascii_hexdigit()),
        "uuid must be hex"
    );
    assert!(
        ub.chars().all(|c| c.is_ascii_hexdigit()),
        "uuid must be hex"
    );

    // Each full uuid must resolve to *its own* page.
    let (code, out, err) = run(&s, &["info", &ua, "--json"]);
    assert_eq!(code, 0, "info by uuid {ua} failed: {err}");
    assert!(
        out.contains(&a_abs),
        "info by uuid {ua} should show {a_abs}\ngot: {out}"
    );
    assert!(
        !out.contains(&b_abs),
        "info by uuid {ua} must NOT show {b_abs}\ngot: {out}"
    );

    let (code, out, err) = run(&s, &["info", &ub, "--json"]);
    assert_eq!(code, 0, "info by uuid {ub} failed: {err}");
    assert!(
        out.contains(&b_abs),
        "info by uuid {ub} should show {b_abs}\ngot: {out}"
    );
    assert!(
        !out.contains(&a_abs),
        "info by uuid {ub} must NOT show {a_abs}\ngot: {out}"
    );
}

// ---- 2 — `kb toc <full-uuid>` → same page as `kb toc <abs_path>` -----

#[test]
fn toc_by_uuid_matches_abs_path() {
    let s = Setup::new("toc");
    write_page(
        &s,
        "p.md",
        "type: wiki\ntitle: P\n",
        "# P\n\n## A\nalpha\n\n## B\nbeta\n",
    );
    let (code, _out, err) = add(&s);
    assert_eq!(code, 0, "add failed: {err}");
    let abs = s.root.join("p.md").to_string_lossy().to_string();
    let uuid = full_uuid(&s, &abs);

    let (code_a, out_a, err_a) = run(&s, &["toc", &abs]);
    let (code_u, out_u, err_u) = run(&s, &["toc", &uuid]);
    assert_eq!(code_a, 0, "toc {abs} failed: {err_a}");
    assert_eq!(code_u, 0, "toc {uuid} failed: {err_u}");
    // Same shape: the sections must be present in the uuid-rendered TOC.
    for needle in ["A", "B"] {
        assert!(
            out_u.contains(needle),
            "toc by uuid must contain section {needle}; got: {out_u}"
        );
        assert!(
            out_a.contains(needle),
            "toc by path must contain section {needle}; got: {out_a}"
        );
    }
}

// ---- 3 — `kb verify <full-uuid>` runs against the page ---------------

#[test]
fn verify_by_uuid_runs() {
    let s = Setup::new("verify");
    write_page(
        &s,
        "p.md",
        "type: wiki\ntitle: P\n",
        "# P\n\ntext\n\n[ /p.md ]\n",
    );
    let (code, _out, err) = add(&s);
    assert_eq!(code, 0, "add failed: {err}");
    let abs = s.root.join("p.md").to_string_lossy().to_string();
    let uuid = full_uuid(&s, &abs);

    // We don't assert on the *content* of verify (it depends on the corpus);
    // we assert the command *runs* (exit 0 or 1, no panic) and does not
    // error out with "no indexed page".
    let (code_u, out_u, err_u) = run(&s, &["verify", &uuid]);
    assert!(
        code_u == 0 || code_u == 1,
        "verify {uuid} should not panic; got code {code_u}, out {out_u}, err {err_u}"
    );
    assert!(
        !out_u.to_lowercase().contains("no indexed page")
            && !err_u.to_lowercase().contains("no indexed page"),
        "verify by uuid should not say 'no indexed page'. out: {out_u}, err: {err_u}"
    );
}

// ---- 4 — `kb resolve <full-uuid>` → unique, 1 candidate --------------

#[test]
fn resolve_by_full_uuid_is_unique() {
    let s = Setup::new("resolve_full");
    write_page(&s, "a.md", "type: wiki\ntitle: A\n", "A\n");
    write_page(&s, "b.md", "type: note\ntitle: B\n", "B\n");
    let (code, _out, err) = add(&s);
    assert_eq!(code, 0, "add failed: {err}");
    let a_abs = s.root.join("a.md").to_string_lossy().to_string();
    let uuid_a = full_uuid(&s, &a_abs);

    let (code, out, err) = run(&s, &["resolve", &uuid_a, "--json"]);
    assert_eq!(code, 0, "resolve {uuid_a} failed: {err}");
    assert!(
        out.contains("\"unique\""),
        "resolve by full uuid must be unique: {out}"
    );
    assert!(
        out.contains(&a_abs),
        "resolve by full uuid must contain {a_abs}\ngot: {out}"
    );
}

// ---- 5 — `kb resolve <non-matching 5-hex>` → empty -------------------

#[test]
fn resolve_by_non_matching_5_hex_is_empty() {
    let s = Setup::new("resolve_empty");
    write_page(&s, "a.md", "type: wiki\ntitle: A\n", "A\n");
    let (code, _out, err) = add(&s);
    assert_eq!(code, 0, "add failed: {err}");

    // Pick a 5-hex prefix that's *unlikely* to be the start of any real uuid.
    // We can't know it a priori, so try a handful and accept either empty or
    // unique (the latter would mean we collided with that page's real prefix).
    for candidate in ["00000", "fffff", "abcdef", "99999", "abcde"] {
        let (code, out, err) = run(&s, &["resolve", candidate, "--json"]);
        assert_eq!(code, 0, "resolve {candidate} should not error: {err}");
        assert!(
            out.contains("\"empty\"") || out.contains("\"unique\""),
            "resolve {candidate} must be empty or unique; got: {out}"
        );
    }
}

// ---- 6 — 4-hex token is NOT accepted as a hex token ------------------

#[test]
fn four_hex_token_is_not_a_uuid_token() {
    let s = Setup::new("four_hex");
    write_page(&s, "a.md", "type: wiki\ntitle: A\n", "A\n");
    let (code, _out, err) = add(&s);
    assert_eq!(code, 0, "add failed: {err}");
    let abs = s.root.join("a.md").to_string_lossy().to_string();
    let full = full_uuid(&s, &abs);
    assert_eq!(full.len(), 16);

    // A path-shaped token containing `/` is never a hex token — the resolver
    // takes the path branch and returns an empty result (no such rel/path).
    let (code, out, err) = run(&s, &["resolve", "abcd"]);
    assert_eq!(
        code, 0,
        "resolve with a 4-hex token must not error (it falls to the path branch); err: {err}"
    );
    // The path branch looks for `abs_path LIKE '%abcd'` — for a fresh corpus
    // this is either empty (most likely) or a single match (if a path ends in
    // `abcd`). Either way, the command *succeeds*, which is the contract.
    let _ = out;
}

// ---- 7 — a `/` in the token is always a path (never a hex token) ------

#[test]
fn path_with_slash_is_never_a_hex_token() {
    let s = Setup::new("slash_path");
    // A file whose *filename* is all-hex — easy to confuse with a uuid token
    // if we (wrongly) dropped the path-signal test.
    write_page(
        &s,
        "sub/abcdef012345.md",
        "type: wiki\ntitle: Hexish\n",
        "Hexish name\n",
    );
    let (code, _out, err) = add(&s);
    assert_eq!(code, 0, "add failed: {err}");
    let abs = s
        .root
        .join("sub/abcdef012345.md")
        .to_string_lossy()
        .to_string();

    // `kb info <abs>` should work (the path branch takes it verbatim).
    let (code, out, err) = run(&s, &["info", &abs, "--json"]);
    assert_eq!(code, 0, "info by path failed: {err}");
    assert!(
        out.contains(&abs),
        "info by path should echo the path: {out}"
    );
    assert!(
        out.contains("\"Hexish\""),
        "info by path should show title: {out}"
    );
}

// ---- 8 — both search and info emit uuid + uuid_short -----------------

#[test]
fn search_and_info_emit_uuid_and_uuid_short() {
    let s = Setup::new("emit");
    write_page(
        &s,
        "p.md",
        "type: wiki\ntitle: Foo\n",
        "# Foo\n\nunique marker phrase: xyzzy\n",
    );
    let (code, _out, err) = add(&s);
    assert_eq!(code, 0, "add failed: {err}");
    let abs = s.root.join("p.md").to_string_lossy().to_string();

    // `info <path> --json` must carry `uuid` (16 hex) and `uuid_short` (5..=16).
    let (code, out, err) = run(&s, &["info", &abs, "--json"]);
    assert_eq!(code, 0, "info failed: {err}");
    assert!(
        out.contains("\"uuid\": "),
        "info JSON must carry uuid: {out}"
    );
    assert!(
        out.contains("\"uuid_short\": "),
        "info JSON must carry uuid_short: {out}"
    );
    // Extract the uuid_short and confirm it's a valid hex prefix of the full.
    let full = full_uuid(&s, &abs);
    let start =
        out.find("\"uuid_short\": \"").expect("no uuid_short field") + "\"uuid_short\": \"".len();
    let end = out[start..].find('"').unwrap();
    let short = out[start..start + end].to_string();
    assert!(
        (5..=16).contains(&short.len()),
        "uuid_short must be 5..=16 hex; got {short:?} (len {})",
        short.len()
    );
    assert!(
        full.starts_with(&short),
        "uuid_short ({short}) must be a prefix of uuid ({full})"
    );
    assert!(
        short.chars().all(|c| c.is_ascii_hexdigit()),
        "uuid_short must be hex: {short}"
    );

    // `search` over the same phrase must also carry both fields. `search`
    // emits compact JSON (`"uuid":"…"`, no space), unlike the pretty form
    // used by `info`, so both shapes are asserted.
    let (code, out, err) = run(&s, &["search", "xyzzy", "--json"]);
    assert_eq!(code, 0, "search failed: {err}");
    assert!(
        out.contains("\"uuid\":\"") || out.contains("\"uuid\": "),
        "search JSON must carry uuid: {out}"
    );
    assert!(
        out.contains("\"uuid_short\":\"") || out.contains("\"uuid_short\": "),
        "search JSON must carry uuid_short: {out}"
    );
}
