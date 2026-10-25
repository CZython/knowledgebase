use crate::*;

/// `kb create <NAME>` — scaffold a new `.md` page and index it (PLAN §11).
///
/// Disk stays authoritative: the scaffold (plus whatever `-e` leaves behind)
/// is the source of truth, and the DB row is written with the *same*
/// `build_page`/`upsert_page` pair `kb refresh` uses, so a page created here
/// reconciles byte-identically on the next refresh. Indexing is **skipped
/// with a note, never an error** when: no roots are registered, the new page
/// sits outside every registered root, or the final bytes are not indexable
/// (§3/§4 `type:` warning contract).
pub(crate) fn cmd_create(dbpath: Option<&Path>, filename: &str, a: &CreateArgs) -> Kb {
    let dir = a.path.as_deref();
    let type_arg = &a.type_;
    let dry_run = a.dry_run;
    let yes = a.yes;
    let json = a.json;
    // The editor inherits our stdio, so it could pollute stdout — same
    // contract as `section edit --json` (§13p): an editor flow is not JSON.
    // SECURITY_REPORT 3.1: only reachable with `--edit`, which is itself an
    // editor-feature surface (a hardened build would reject it in clap);
    // gate the `edit` bindings so a hardened compile has no dangling reads.
    #[cfg(feature = "editor")]
    {
        let edit = a.edit;
        if edit && json {
            return Err((
                "`create --edit --json` is unsupported — `--edit` is an editor flow, not JSON (drop one of the two flags)".into(),
                EXIT_ERR,
            ));
        }
    }
    let kind = match NoteType::parse(type_arg) {
        Some(NoteType::Wiki) => "wiki",
        Some(NoteType::Note) => "note",
        Some(NoteType::Label) => "label",
        None => {
            return Err((
                format!("--type must be wiki|note|label (got {type_arg:?})"),
                EXIT_ERR,
            ))
        }
    };

    // ---- target path: [--path DIR] + NAME, `.md` appended when absent ----
    let name = filename.trim();
    if name.is_empty() {
        return Err(("create: NAME must not be empty".into(), EXIT_ERR));
    }
    if name.chars().any(|c| c == '\0' || c == '\n' || c == '\r') {
        return Err(("create: NAME must not contain newlines or NUL bytes".into(), EXIT_ERR));
    }
    let mut target = match dir {
        Some(d) => d.join(name),
        None => PathBuf::from(name),
    };
    match target.extension().and_then(|e| e.to_str()) {
        Some("md") => {}
        Some("") | None => {
            let mut os = target.clone().into_os_string();
            os.push(".md");
            target = PathBuf::from(os);
        }
        Some(other) => {
            return Err((
                format!(
                    "create: {name:?} is not a .md page (extension {other:?}) — PLAN §3 indexes `.md` only"
                ),
                EXIT_ERR,
            ));
        }
    }
    let title = target
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(("create: NAME must yield a non-empty page title".into(), EXIT_ERR));
    }
    // Work in absolute terms from here on: the diff label, the JSON `file`
    // and the `pages.abs_path` key all want the same string, and an agent
    // reading a relative `file` would have to re-anchor it on the cwd.
    if !target.is_absolute() {
        let cwd = std::env::current_dir()
            .map_err(|e| (format!("current dir: {e}"), EXIT_ERR))?;
        target = cwd.join(&target);
    }
    if target.exists() {
        return Err((format!("create: {} already exists", target.display()), EXIT_ERR));
    }

    // ---- the scaffold (PLAN §4: `type:` + `title:`, no H1 required) -------
    let scaffold = format!(
        "---\ntype: {kind}\ntitle: {title}\ndescription:\ntags: []\nsearch_required_phrase: []\n---\n\n# {title}\n\n"
    );
    let d = diff::unified_diff("", &scaffold, "/dev/null", &target.display().to_string());

    // Human render of the new-file diff: stdout normally, stderr under
    // `--json` (stdout must carry exactly one JSON document — §13p).
    if !d.is_empty() {
        if json {
            eprint!("{d}");
        } else {
            print!("{d}");
        }
    }

    if dry_run {
        if json {
            let r = CreateReport {
                target: target.as_path(),
                kind,
                title: title.as_str(),
                id: None,
                created: false,
                edited: false,
                indexed: false,
                dry_run: true,
                bytes: scaffold.len(),
                diff: d.as_str(),
            };
            println!("{}", r.to_json());
        } else {
            eprintln!("kb: dry-run — {} not created", target.display());
        }
        return Ok(EXIT_OK);
    }

    confirm("create?", yes || json)?;

    // ---- write (atomic: same-dir temp + rename) ---------------------------
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| (format!("create {}: {e}", parent.display()), EXIT_ERR))?;
        }
    }
    atomic_write(&target, scaffold.as_bytes())
        .map_err(|e| (format!("write {}: {e}", target.display()), EXIT_ERR))?;
    if !json {
        println!("kb: created {} ({} bytes)", target.display(), scaffold.len());
    }

    // ---- optional in-place edit: the saved bytes are what we index --------
    // SECURITY_REPORT 3.1: `-e` / `a.edit` are editor-feature surface —
    // the `edit` field on `CreateArgs` only exists when the feature is on,
    // so in a hardened build clap rejects `-e` as an unexpected argument
    // before `cmd_create` sees it. The `edited` binding is a constant
    // `false` in that mode; the editor fork never happens.
    #[cfg(feature = "editor")]
    let edited = if a.edit {
        edit_in_place(&target)?;
        true
    } else {
        false
    };
    #[cfg(not(feature = "editor"))]
    let edited = false;

    // ---- index (best-effort; disk is authoritative, `kb refresh` repairs) --
    let db = open_db(dbpath)?;
    let roots = roots_with_seq(&db)?;
    let mut indexed = false;
    let mut id: Option<String> = None;
    let mut note: Option<String> = None;
    if roots.is_empty() {
        note = Some(
            "no roots registered — skipped indexing (run `kb add <dir>` first)".to_string(),
        );
    } else {
        let abs = std::fs::canonicalize(&target)
            .map_err(|e| (format!("resolve {}: {e}", target.display()), EXIT_ERR))?;
        let abs_s = abs.to_string_lossy().to_string();
        match kb_db::walk_snapshot::winning_root(&roots, &abs_s) {
            None => {
                note = Some(format!(
                    "{} is not under any registered root — skipped indexing (run `kb add <dir>`)",
                    abs.display()
                ));
            }
            Some((root, rel)) => match kb_db::index::build_page(&abs, &root, &rel, true) {
                Ok(rec) => match kb_db::index::upsert_page(&db, &rec) {
                    Ok(_) => {
                        id = Some(rec.id.clone());
                        indexed = true;
                    }
                    Err(e) => note = Some(format!("indexed skipped (DB upsert: {e})")),
                },
                Err(e) => note = Some(format!(
                    "not indexed ({e}) — fix the frontmatter, then `kb refresh`"
                )),
            },
        }
    }

    if json {
        let r = CreateReport {
            target: target.as_path(),
            kind,
            title: title.as_str(),
            id: id.as_deref(),
            created: true,
            edited,
            indexed,
            dry_run: false,
            bytes: scaffold.len(),
            diff: d.as_str(),
        };
        println!("{}", r.to_json());
    } else if indexed {
        if let Some(i) = &id {
            println!("kb: indexed as {i}");
        }
    }
    if let Some(n) = note {
        eprintln!("kb: {n}");
    }
    Ok(EXIT_OK)
}


/// The `create` result envelope — one JSON object on stdout (§13p), keys
/// stable across the dry-run / write paths so an agent can branch on
/// `indexed` / `dry_run` without parsing prose.
pub(crate) struct CreateReport<'a> {
    pub(crate) target:&'a Path,
    pub(crate) kind:&'a str,
    pub(crate) title:&'a str,
    pub(crate) id:Option<&'a str>,
    pub(crate) created:bool,
    pub(crate) edited:bool,
    pub(crate) indexed:bool,
    pub(crate) dry_run:bool,
    pub(crate) bytes:usize,
    pub(crate) diff:&'a str,
}


impl CreateReport<'_> {
    pub(crate) fn to_json(&self) -> String {
        let mut o = JObject::new();
        o.s("file", &self.target.display().to_string());
        o.s("type", self.kind);
        o.s("title", self.title);
        let id_opt: Option<String> = self.id.map(|s| s.to_string());
        o.opt("id", &id_opt);
        o.b("created", self.created);
        o.b("edited", self.edited);
        o.b("indexed", self.indexed);
        o.b("dry_run", self.dry_run);
        o.i("bytes", self.bytes as i64);
        o.s("diff", self.diff);
        o.render()
    }
}

