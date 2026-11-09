use crate::*;

pub(crate) fn cmd_section(dbpath: Option<&Path>, file: &Path, op: &SectionOp) -> Kb {
    match op {
        SectionOp::Set {
            ref_: r,
            text,
            keep_heading,
            value,
            flags,
        } => {
            if is_fm_ref(r) {
                let key = fm_key(r).ok_or_else(|| (format!("`set` on whole-frontmatter ref {r:?} needs a key (fm.<key>)"), EXIT_SECTION))?;
                let val = fm_resolve_value(value, text, file)?;
                let orig = read_file(file)?;
                check_etag_whole(&orig, flags)?;
                let new = fm::set_scalar(&orig, key, &val).map_err(|e| (e.to_string(), EXIT_SECTION))?;
                commit(dbpath, file, orig, new, flags.dry_run, flags.yes, flags.json)
            } else {
                let orig = read_file(file)?;
                let doc = Doc::new(orig.clone());
                check_etag(&doc, Some(r.as_str()), flags)?;
                let text = read_text(text, file)?;
                let new = doc
                    .set_section(r, &text, *keep_heading)
                    .map_err(|e| (e.to_string(), EXIT_SECTION))?;
                commit(dbpath, file, orig, new, flags.dry_run, flags.yes, flags.json)
            }
        }
        SectionOp::Append { ref_: r, text, value, flags } => {
            if is_fm_ref(r) {
                let key: &str = fm_key(r).ok_or_else(|| (format!("`append` on whole-frontmatter ref {r:?} needs a key (fm.<key>)"), EXIT_SECTION))?;
                let val = fm_resolve_value(value, text, file)?;
                let orig = read_file(file)?;
                check_etag_whole(&orig, flags)?;
                let new = fm::append_list_item(&orig, key, &val).map_err(|e| (e.to_string(), EXIT_SECTION))?;
                commit(dbpath, file, orig, new, flags.dry_run, flags.yes, flags.json)
            } else {
                let orig = read_file(file)?;
                let doc = Doc::new(orig.clone());
                check_etag(&doc, Some(r.as_str()), flags)?;
                let text = read_text(text, file)?;
                let new = doc
                    .append_section(r, &text)
                    .map_err(|e| (e.to_string(), EXIT_SECTION))?;
                commit(dbpath, file, orig, new, flags.dry_run, flags.yes, flags.json)
            }
        }
        SectionOp::Insert {
            ref_: r,
            title,
            placement,
            level,
            text,
            flags,
        } => {
            let orig = read_file(file)?;
            let doc = Doc::new(orig.clone());
            check_etag(&doc, r.as_deref(), flags)?;
            let text = read_text(text, file)?;
            let place = parse_placement(placement)?;
            let (ins, chunk) = doc
                .insert_section(place, r.as_deref(), &text, title, *level)
                .map_err(|e| (e.to_string(), EXIT_SECTION))?;
            let new = splice(&doc.data, ins, ins, &chunk);
            commit(dbpath, file, orig, new, flags.dry_run, flags.yes, flags.json)
        }
        SectionOp::Replace { ref_: r, text, flags } => {
            let orig = read_file(file)?;
            let doc = Doc::new(orig.clone());
            check_etag(&doc, Some(r.as_str()), flags)?;
            let text = read_text(text, file)?;
            // `replace` = replace the section's body, keep its heading.
            let new = doc
                .set_section(r, &text, true)
                .map_err(|e| (e.to_string(), EXIT_SECTION))?;
            commit(dbpath, file, orig, new, flags.dry_run, flags.yes, flags.json)
        }
        #[cfg(feature = "editor")]
        SectionOp::Edit { ref_: r, flags } => {
            if flags.json {
                return Err((
                    "`section edit --json` is unsupported — `--edit` is an editor flow, not JSON (use `set --json` over a text source instead)".into(),
                    EXIT_SECTION,
                ));
            }
            let orig = read_file(file)?;
            let doc = Doc::new(orig.clone());
            check_etag(&doc, Some(r.as_str()), flags)?;
            let old_full = doc.full_bytes(r).map_err(|e| (e.to_string(), EXIT_SECTION))?;
            let edited = run_editor(old_full)?;
            if edited.as_slice() == old_full {
                println!("{}: no changes (section unchanged in editor)", file.display());
                return Ok(EXIT_OK);
            }
            let new_text = String::from_utf8_lossy(&edited).into_owned();
            // The editor had the complete section (heading + body + subsections);
            // hand the whole thing back to `set_section` (keep_heading = false)
            // so the full span is replaced exactly as edited.
            let new = doc
                .set_section(r, &new_text, false)
                .map_err(|e| (e.to_string(), EXIT_SECTION))?;
            commit(dbpath, file, orig, new, flags.dry_run, flags.yes, flags.json)
        }
        SectionOp::Rm { ref_: r, value, flags } => {
            if is_fm_ref(r) {
                let key: &str = fm_key(r).ok_or_else(|| (format!("`rm` on whole-frontmatter ref {r:?} needs a key (fm.<key>)"), EXIT_SECTION))?;
                let orig = read_file(file)?;
                check_etag_whole(&orig, flags)?;
                let new = match value {
                    Some(v) => fm::remove_list_item(&orig, key, v).map_err(|e| (e.to_string(), EXIT_SECTION))?,
                    None => fm::remove_field(&orig, key).map_err(|e| (e.to_string(), EXIT_SECTION))?,
                };
                commit(dbpath, file, orig, new, flags.dry_run, flags.yes, flags.json)
            } else {
                // `rm <section-ref> [value]` — remove the section (heading + body +
                // its subsections). A `value` argument is meaningful only for an
                // `fm.<list>` ref; a section ref accepts no list-item syntax, so we
                // error clearly rather than silently ignoring it.
                if value.is_some() {
                    return Err((
                        format!("`rm` on a section ref takes no <value> (got {r:?}); use `rm {r}` to delete the whole section"),
                        EXIT_SECTION,
                    ));
                }
                let orig = read_file(file)?;
                let doc = Doc::new(orig.clone());
                check_etag(&doc, Some(r.as_str()), flags)?;
                let new = doc
                    .remove_section(r)
                    .map_err(|e| (e.to_string(), EXIT_SECTION))?;
                commit(dbpath, file, orig, new, flags.dry_run, flags.yes, flags.json)
            }
        }
    }
}


pub(crate) fn cmd_replace(dbpath: Option<&Path>, file: &Path, find: &str, new: &str, scope: Option<&str>, count: Option<usize>, flags: &WriteFlags) -> Kb {
    let orig = read_file(file)?;
    let doc = Doc::new(orig.clone());
    check_etag(&doc, scope, flags)?;
    let (nb, n) = doc
        .replace_in(scope, find, new, count.unwrap_or(0))
        .map_err(|e| (e.to_string(), EXIT_SECTION))?;
    if !flags.dry_run {
        eprintln!("kb: {n} replacement(s)");
    }
    commit(dbpath, file, orig, nb, flags.dry_run, flags.yes, flags.json)
}


pub(crate) fn cmd_patch(
    dbpath: Option<&Path>,
    file: &Path,
    patch_file: Option<&Path>,
    patch_stdin: bool,
    patch_text: Option<&str>,
    flags: &WriteFlags,
) -> Kb {
    // Read the patch from exactly one source.
    //   `patch_file = Some(p)`       → read from that file (`-` = stdin)
    //   `patch_stdin = true`         → read from stdin
    //   `patch_text = Some(t)`       → use literal text
    // Multiple sources is an error (the caller must be deliberate); zero
    // is an error (the CLI's flags are not required — this is a runtime
    // invariant; the message names the fix).
    let mut sources = 0usize;
    if patch_file.is_some() { sources += 1; }
    if patch_stdin         { sources += 1; }
    if patch_text.is_some() { sources += 1; }
    if sources != 1 {
        return Err((
            format!(
                "one of --patch-file / --patch-stdin / --patch-text is required (got {sources})"
            ),
            EXIT_ERR,
        ));
    }
    let patch: String = if let Some(t) = patch_text {
        t.to_string()
    } else if let Some(p) = patch_file {
        if p == Path::new("-") {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| (format!("read stdin: {e}"), EXIT_ERR))?;
            s
        } else {
            std::fs::read_to_string(p).map_err(|e| (format!("read {}: {e}", p.display()), EXIT_ERR))?
        }
    } else {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| (format!("read stdin: {e}"), EXIT_ERR))?;
        s
    };

    // Apply. `apply_patch` is total over its input (either `Ok` with a
    // new-text `String`, or `Err(err_msg)`) — no `?`-able panic path.
    let orig = read_file(file)?;
    let doc = Doc::new(orig.clone());
    check_etag(&doc, None, flags)?;
    let orig_s = String::from_utf8_lossy(&orig).into_owned();
    let new_s = diff::apply_patch(&orig_s, &patch)
        .map_err(|e| (format!("apply patch: {e}"), EXIT_SECTION))?;
    let new = new_s.into_bytes();
    commit(dbpath, file, orig, new, flags.dry_run, flags.yes, flags.json)
}

