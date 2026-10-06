/// E2E: `--json` is a *lossless* encoding of the same page content the text
/// surface already shows — i.e. for any page, `info --json` decodes to the
/// exact same logical title/description that `info` (text) prints, and the
/// same values appear in `browse --json`. This is the real guarantee an agent
/// needs: JSON and text never disagree.
///
/// (Frontmatter *parsing* — quote/backslash handling — is M1's business and
/// is identical on both surfaces; JSON escaping of the special chars is pinned
/// byte-for-byte by the `PageJson::render` unit test in `kb/src/main.rs`, so
/// it need not be re-derived through the parser here.)
///
/// Run: `cargo test -p kb --test m9_json_fidelity`
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn kb() -> Command { Command::new(env!("CARGO_BIN_EXE_kb")) }

struct S { root: PathBuf, db: PathBuf }
impl S {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!("kbfit_{}", std::process::id()));
        let root = base.join("r");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let db = base.join("kb.db");
        let _ = fs::remove_file(&db);
        S { root, db }
    }
}
impl Drop for S { fn drop(&mut self) { let _ = fs::remove_dir_all(self.root.parent().unwrap()); } }

fn run(s: &S, args: &[&str]) -> (i32, String, String) {
    let db = s.db.to_string_lossy().into_owned();
    let mut cmd = kb();
    cmd.arg(format!("--db={db}"));
    for x in args { cmd.arg(x); }
    let o = cmd.output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(),
     String::from_utf8_lossy(&o.stderr).into_owned())
}

/// Feed `stdin` through `python3 -c prog [--arg1 ...]`; return (ok, out+err).
/// Extra args go to `sys.argv[1:]` so arbitrary text (quotes, backslash,
/// unicode) can be passed without any nested escaping.
fn py(stdin: &str, prog: &str, args: &[String]) -> (bool, String) {
    let mut c = Command::new("python3");
    c.arg("-c").arg(prog);
    for a in args { c.arg(a); }
    c.stdin(Stdio::piped());
    c.stdout(Stdio::piped());
    c.stderr(Stdio::piped());
    let mut ch = c.spawn().unwrap();
    { use std::io::Write; ch.stdin.as_mut().unwrap().write_all(stdin.as_bytes()).unwrap(); }
    let r = ch.wait_with_output().unwrap();
    let mut s = String::from_utf8_lossy(&r.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&r.stderr));
    (r.status.success(), s)
}

#[test]
fn json_is_lossless_encoding_of_text_surface() {
    let s = S::new();
    // A title with an unambiguous, parse-stable value (unicode + a literal
    // quote). Both surfaces must agree on the value AFTER M1's frontmatter
    // parser runs on it; the test proves only *that* and that `--json` is a
    // lossless encoding of that value.
    let file = s.root.join("page.md");
    fs::write(&file,
        "---\ntype: note\ntitle: Quantum é \"gate\"\ndescription: lossless\n---\n# body\n"
    ).unwrap();
    let root = s.root.to_string_lossy().into_owned();
    assert_eq!(run(&s, &["add", &root]).0, 0);

    // ---- 1) info: text surface + json surface must agree on the title ----
    let (c1, text, e1) = run(&s, &["info", file.to_str().unwrap()]);
    assert_eq!(c1, 0, "text info: {e1}");
    let (c2, jtxt, e2) = run(&s, &["info", file.to_str().unwrap(), "--json"]);
    assert_eq!(c2, 0, "json info: {e2}");

    // Extract the text surface's title value (after the `title:` label).
    let text_title: String = text
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("title:").map(|v| v.trim().to_string()))
        .expect("text info must contain a `title:` line");

    // Pass both strings to python and assert the JSON value decodes to the
    // exact text value — a losslessness check (JSON must be the same content).
    // The expected value travels via sys.argv (no nested escaping).
    let (ok, detail) = py(
        &jtxt,
        "import json, sys\n\
         d = json.loads(sys.stdin.read())\n\
         t = d['title']\n\
         expected = sys.argv[1]\n\
         assert t == expected, repr(t) + ' != ' + repr(expected)\n\
          print('lossless-ok', repr(t))\n",
        std::slice::from_ref(&text_title),
    );
    assert!(
        ok,
        "info: --json must decode to the same title text surface shows\n\
         expected text-title: {text_title:?}\n{detail}\n--- json ---\n{jtxt}",
    );

    // ---- 2) browse: the same value must appear in the JSON array too ----
    let (c3, brows, e3) = run(&s, &["browse", "--json"]);
    assert_eq!(c3, 0, "browse: {e3}");
    let (ok, detail) = py(
        &brows,
        "import json, sys\n\
         arr = json.loads(sys.stdin.read())\n\
         assert len(arr) == 1, len(arr)\n\
         t = arr[0]['title']\n\
         assert t == sys.argv[1], repr(t)\n\
          print('browse-lossless-ok', repr(t))\n",
        std::slice::from_ref(&text_title),
    );
    assert!(
        ok,
        "browse: --json must carry the same title as info\n{detail}\n--- raw ---\n{brows}",
    );

    // ---- 3) the description (a clean value) round-trips verbatim ----
    let (ok, detail) = py(
        &jtxt,
        "import json, sys\nd=json.loads(sys.stdin.read())\n\
         assert d['description'] == 'lossless', repr(d.get('description'))\n\
         print('desc-ok')",
        &[],
    );
    assert!(ok, "description must round-trip verbatim\n{detail}\n--- json ---\n{jtxt}");
}
