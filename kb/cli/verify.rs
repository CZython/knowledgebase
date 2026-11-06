use crate::*;
use kb_core::identity;
use kb_core::meta::NoteType;

pub(crate) const VERIFY_OK: i32 = 0;
pub(crate) const VERIFY_ERROR: i32 = 1;
pub(crate) const VERIFY_USAGE: i32 = 2;

pub(crate) fn cmd_verify(
    file: &Path,
    strict: bool,
    json: bool,
    dbpath: Option<&Path>,
) -> Kb {
    let meta = std::fs::metadata(file).map_err(|e| {
        (format!("cannot stat {}: {e}", file.display()), VERIFY_USAGE)
    })?;
    if meta.is_dir() {
        return Err((
            format!("{}: is a directory — kb verify takes one file, not a directory", file.display()),
            VERIFY_USAGE,
        ));
    }
    let data: Vec<u8> = std::fs::read(file).map_err(|e| {
        (format!("read {}: {e}", file.display()), VERIFY_USAGE)
    })?;
    let text: String = String::from_utf8_lossy(&data).into_owned();

    let ext: String = file
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let md_ok: bool = matches!(ext.as_str(), "md" | "markdown" | "mdx");

    let fm_frame = fm::find_fm(&data);
    let fm_present: bool = fm_frame.is_some();
    let first_line_dashes: bool = text
        .lines()
        .next()
        .map(|l| l.trim() == "---")
        .unwrap_or(false);
    let framing: &str = if fm_present {
        "ok"
    } else if !first_line_dashes {
        "no-frontmatter"
    } else {
        "unterminated-frontmatter"
    };

    let parsed = fm::parse(&text);

    let type_present: bool = parsed.type_raw.is_some();
    let type_known: bool = parsed.page_type.is_some();
    let type_value: &Option<String> = &parsed.type_raw;

    let fname: String = file
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let h1: Option<&str> = identity::find_h1(&parsed.body);
    let fname_opt: Option<&str> = if fname.trim().is_empty() {
        None
    } else {
        Some(&fname)
    };
    let slug: Option<&str> = identity::resolve_slug(
        parsed.title.as_deref(),
        h1,
        fname_opt,
    );
    let slug_source: &Option<&str> = &if parsed
        .title
        .as_deref()
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false)
    {
        Some("title")
    } else if h1.is_some() {
        Some("h1")
    } else if fname_opt.is_some() {
        Some("filename")
    } else {
        None
    };

    let description_present: bool = parsed
        .description
        .as_deref()
        .map(|d| !d.trim().is_empty())
        .unwrap_or(false);
    let description_chars: i64 = parsed
        .description
        .as_deref()
        .map(|d| d.chars().count() as i64)
        .unwrap_or(0);
    let description_over_limit: bool = parsed.description_too_long;

    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if !md_ok {
        errors.push("non-md-file".into());
    }
    if !fm_present {
        errors.push("unterminated-frontmatter".into());
    }
    if !type_present {
        errors.push("missing-type".into());
    } else if !type_known {
        errors.push("unknown-type".into());
    }
    if slug.is_none() {
        errors.push("no-slug".into());
    }
    if description_over_limit {
        warnings.push("description-too-long".into());
    }

    // ---- check d: are all links resolved? (Phase 2) -----------------------
    // One corpus load feeds the resolver; `annotated_links` is the *same*
    // fn the TUI picker + `kb links --list` use, so `verify` can never
    // disagree with them about which ref is dead vs ambiguous.
    let db = open_db(dbpath)?;
    let pages = load_corpus(&db)?;
    let corpus = annotated_links(&parsed.body, &pages);

    let mut links_resolved: usize = 0;
    let mut links_dead: Vec<(usize, usize, &kb_core::links::LinkStyle, &str)> = Vec::new();
    let mut links_ambiguous: Vec<(usize, &str, Vec<String>)> = Vec::new();
    for l in &corpus {
        if l.resolves_to.is_some() {
            links_resolved += 1;
        } else if l.candidates.is_empty() {
            links_dead.push((l.line, l.col, &l.style, l.target.as_str()));
            warnings.push("dead-link".into());
        } else {
            links_ambiguous.push((l.line, l.target.as_str(), l.candidates.clone()));
            warnings.push("ambiguous-link".into());
        }
    }
    let links_total: usize = corpus.len();
    let links_external: usize = 0; // v1 `annotated_links` yields no `<url>` refs

    // ---- check e: `type: label` — what does it point at? (Phase 2) --------
    let is_label = matches!(parsed.page_type, Some(NoteType::Label));
    let mut label_applies: &str = "na"; // "na" | "files" | "folder"
    let mut label_files_total: usize = 0;
    let mut label_missing: Vec<String> = Vec::new();
    let mut label_entries: usize = 0;
    if is_label {
        let base = file
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        match kb_core::labels::classify(&parsed.body) {
            kb_core::labels::LabelRef::Files(refs) => {
                label_applies = "files";
                label_files_total = refs.len();
                for r in &refs {
                    if !base.join(r).exists() {
                        label_missing.push(r.clone());
                        warnings.push(format!("label-file-missing:{r}"));
                    }
                }
            }
            kb_core::labels::LabelRef::Folder => {
                label_applies = "folder";
                label_entries = std::fs::read_dir(&base)
                    .map(|rd| rd.filter(|e| e.is_ok()).count())
                    .unwrap_or(0);
            }
        }
    }

    let (verdict, exit): (&str, i32) = if errors.is_empty() && warnings.is_empty() {
        ("ok", VERIFY_OK)
    } else if errors.is_empty() {
        if strict {
            ("ok-with-warnings", VERIFY_ERROR)
        } else {
            ("ok-with-warnings", VERIFY_OK)
        }
    } else {
        ("error", VERIFY_ERROR)
    };

    let file_disp: String = file.display().to_string();
    let type_label: &str = match parsed.page_type {
        Some(NoteType::Label) => "label",
        Some(NoteType::Wiki) => "wiki",
        Some(NoteType::Note) => "note",
        None => type_value.as_deref().unwrap_or("(none)"),
    };
    let required_pass: bool = type_known && slug.is_some();

    if json {
        let mut checks = JObject::new();

        let mut mkd = JObject::new();
        mkd.b("pass", md_ok);
        checks.raw("markdown", mkd.render());

        let mut fm_o = JObject::new();
        fm_o.b("pass", fm_present);
        fm_o.s("framing", framing);
        checks.raw("frontmatter", fm_o.render());

        let mut req = JObject::new();
        req.b("pass", required_pass);

        let mut t = JObject::new();
        t.b("present", type_present);
        if let Some(v) = type_value {
            t.s("value", v);
        }
        t.b("known", type_known);
        req.raw("type", t.render());

        let mut s = JObject::new();
        s.b("resolved", slug.is_some());
        if let Some(src) = slug_source {
            s.s("source", src);
        }
        if let Some(val) = slug {
            s.s("value", val);
        }
        req.raw("slug", s.render());

        let mut d = JObject::new();
        d.b("present", description_present);
        d.i("chars", description_chars);
        d.b("over_limit", description_over_limit);
        req.raw("description", d.render());

        checks.raw("required", req.render());

        let mut lk = JObject::new();
        lk.i("total", links_total as i64);
        lk.i("resolved", links_resolved as i64);
        lk.i("external", links_external as i64);
        let dead_items: Vec<String> = links_dead
            .iter()
            .map(|(line, col, style, target)| {
                let mut e = JObject::new();
                e.i("line", *line as i64);
                e.i("col", *col as i64);
                e.s("style", style.as_str());
                e.s("target", target);
                e.render_compact()
            })
            .collect();
        lk.raw("dead", block_array(&dead_items));
        let amb_items: Vec<String> = links_ambiguous
            .iter()
            .map(|(line, target, cands)| {
                let mut e = JObject::new();
                e.i("line", *line as i64);
                e.s("target", target);
                e.raw("candidates", inline_array(cands));
                e.render_compact()
            })
            .collect();
        lk.raw("ambiguous", block_array(&amb_items));
        checks.raw("links", lk.render());

        let mut lb = JObject::new();
        lb.s("type", type_label);
        lb.s("applies", label_applies);
        if is_label && label_applies == "files" {
            lb.i("files", label_files_total as i64);
            lb.raw("missing", inline_array(&label_missing.clone()));
        }
        if is_label && label_applies == "folder" {
            lb.i("entries", label_entries as i64);
        }
        checks.raw("label", lb.render());

        let mut o = JObject::new();
        o.s("file", &file_disp);
        o.s("verdict", verdict);
        o.i("exit", exit as i64);
        if !warnings.is_empty() {
            o.raw(
                "warnings",
                inline_array(&warnings.iter().map(|w| w.to_string()).collect::<Vec<_>>()),
            );
        }
        if !errors.is_empty() {
            o.raw(
                "errors",
                inline_array(&errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()),
            );
        }
        o.raw("checks", checks.render());
        println!("{}", o.render());
        return Ok(exit);
    }

    println!("{file_disp}");
    println!(
        "  markdown:      {}",
        if md_ok { "ok" } else { "non-md extension" }
    );
    println!("  frontmatter:   {framing}");
    println!("  type:          {type_label}");
    match (slug, slug_source.as_ref().map(|&x| x)) {
        (Some(v), Some(src)) => println!("  slug:          {v}  ({src})"),
        (Some(v), None) => println!("  slug:          {v}"),
        (None, _) => println!("  slug:          unresolved"),
    }
    if description_over_limit {
        println!("  description:   {description_chars} chars (over 255 limit)");
    } else if description_present {
        println!("  description:   {description_chars} chars");
    }

    println!(
        "  links:         {links_total} link(s) — {links_resolved} resolved, {links_external} external, {} dead, {} ambiguous",
        links_dead.len(),
        links_ambiguous.len()
    );
    for (line, col, style, target) in &links_dead {
        println!(
            "  warn:  dead link L{line} [{}] '{target}' → no page in the corpus",
            style.as_str()
        );
        let _ = col;
    }
    for (line, target, cands) in &links_ambiguous {
        println!(
            "  warn:  ambiguous link L{line} '{target}' → {} candidates: {}",
            cands.len(),
            cands.join(" | ")
        );
    }

    if is_label {
        match label_applies {
            "files" => {
                let suffix = if label_missing.is_empty() {
                    " (all present)".to_string()
                } else {
                    format!(" (missing: {})", label_missing.join(", "))
                };
                println!(
                    "  label:       files — {label_files_total} ref(s){suffix}"
                );
            }
            "folder" => {
                println!("  label:       folder-wide — {label_entries} entr{} under the note's directory",
                    if label_entries == 1 { "y" } else { "ies" });
            }
            _ => unreachable!("label_applies was set for a label page"),
        }
    } else {
        println!("  label:       n/a (type: {type_label})");
    }

    for e in &errors {
        println!("  error: {e}");
    }
    for w in &warnings {
        println!("  warn:  {w}");
    }
    println!("  verdict: {verdict}  (exit {exit})");

    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A throwaway scratch dir per test, cleaned up on drop.
    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "kb_verify_{}_{}_{}",
                std::process::id(),
                seq,
                tag
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
        fn file(&self, name: &str, content: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, content).unwrap();
            p
        }
    }
    /// Run `cmd_verify` with the per-test throwaway DB (so the default
    /// `~/.local/share` DB — which is already migrated — is never shared).
    fn verify(
        tmp: &Tmp,
        path: &Path,
        strict: bool,
        json: bool,
    ) -> Result<i32, (String, i32)> {
        let db = tmp.0.join("test.db");
        cmd_verify(path, strict, json, Some(db.as_path()))
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn verify_ok_with_title_and_description() {
        let tmp = Tmp::new("ok");
        let path = tmp.file(
            "sample.md",
            "---\ntype: note\ntitle: Sample\ndescription: short\n---\n\n# Sample\n\nBody text.\n",
        );
        assert_eq!(verify(&tmp, &path, false, false).unwrap(), 0);
    }

    #[test]
    fn verify_error_missing_type() {
        let tmp = Tmp::new("no_type");
        let path = tmp.file("no_type.md", "---\ntitle: Sample\n---\n\nBody.\n");
        assert_eq!(verify(&tmp, &path, false, false).unwrap(), 1);
    }

    #[test]
    fn verify_error_unknown_type() {
        let tmp = Tmp::new("bad_type");
        let path = tmp.file("bad.md", "---\ntype: labbel\ntitle: Sample\n---\n\nBody.\n");
        assert_eq!(verify(&tmp, &path, false, false).unwrap(), 1);
    }

    #[test]
    fn verify_error_no_frontmatter() {
        let tmp = Tmp::new("no_fm");
        let path = tmp.file("plain.md", "# Just a title\n\nBody.\n");
        assert_eq!(verify(&tmp, &path, false, false).unwrap(), 1);
    }

    #[test]
    fn verify_non_md_is_error() {
        let tmp = Tmp::new("txt");
        let path = tmp.file(
            "sample.txt",
            "---\ntype: note\ntitle: Sample\n---\n\nBody.\n",
        );
        assert_eq!(verify(&tmp, &path, false, false).unwrap(), 1);
    }

    #[test]
    fn verify_strict_escalates_warning() {
        let tmp = Tmp::new("long");
        let long_desc = "x".repeat(300);
        let path = tmp.file(
            "long.md",
            &format!("---\ntype: note\ntitle: Sample\ndescription: {long_desc}\n---\n\nBody.\n"),
        );
        assert_eq!(verify(&tmp, &path, false, false).unwrap(), 0);
        assert_eq!(verify(&tmp, &path, true, false).unwrap(), 1);
    }

    #[test]
    fn verify_directory_is_usage_error() {
        let tmp = Tmp::new("dir");
        let err = verify(&tmp, &tmp.0, false, false).unwrap_err();
        assert_eq!(err.1, 2);
    }

    #[test]
    fn verify_missing_file_is_usage_error() {
        let tmp = Tmp::new("ghost");
        let err = verify(&tmp, &tmp.0.join("does_not_exist.md"), false, false).unwrap_err();
        assert_eq!(err.1, 2);
    }

    #[test]
    fn verify_slug_from_h1_falls_back() {
        let tmp = Tmp::new("h1");
        let path = tmp.file("h1only.md", "---\ntype: note\n---\n\n# The H1 Title\n\nBody.\n");
        assert_eq!(verify(&tmp, &path, false, false).unwrap(), 0);
    }

    #[test]
    fn verify_slug_falls_back_to_filename() {
        let tmp = Tmp::new("fname");
        let path = tmp.file(
            "my-page.md",
            "---\ntype: note\n---\n\nJust body, no heading, no title field.\n",
        );
        assert_eq!(verify(&tmp, &path, false, false).unwrap(), 0);
    }
}
