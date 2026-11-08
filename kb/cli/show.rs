use crate::*;

pub(crate) fn cmd_toc(file: &Path, json: bool) -> Kb {
    let data = read_file(file)?;
    let doc = Doc::new(data);
    if json {
        let secs: Vec<String> = doc
            .sections
            .iter()
            .map(|s| {
                format!(
                    "{{\"ref\":{},\"level\":{},\"title\":{},\"line\":{},\"lines\":{},\"chars\":{},\"body_lines\":{},\"body_chars\":{}}}",
                    jstr(&s.ref_),
                    s.level,
                    jstr(&s.title),
                    s.start_line + 1,
                    s.end_line.saturating_sub(s.start_line),
                    s.end.saturating_sub(s.start),
                    s.own_end_line.saturating_sub(s.start_line),
                    s.own_end.saturating_sub(s.start)
                )
            })
            .collect();
        println!(
            "{{\"file\":{},\"sections\":[{}]}}",
            jstr(&file.display().to_string()),
            secs.join(",")
        );
        return Ok(EXIT_OK);
    }
    if doc.sections.is_empty() {
        println!("{}: (no sections)", file.display());
        return Ok(EXIT_OK);
    }
    for s in &doc.sections {
        let indent = "  ".repeat((s.level.saturating_sub(1)) as usize);
        println!(
            "{indent}{}   ref {}  (level {})  [{} lines, {} chars]",
            s.title,
            s.ref_,
            s.level,
            s.end_line.saturating_sub(s.start_line),
            s.end.saturating_sub(s.start)
        );
    }
    Ok(EXIT_OK)
}


pub(crate) fn cmd_show(file: &Path, ref_: Option<&str>, body: bool, full: bool, json: bool, show_etag: bool) -> Kb {
    let data = read_file(file)?;
    let doc = Doc::new(data);

    let Some(r) = ref_ else {
        // No ref → whole file.
        if json {
            println!(
                "{{\"file\":{},\"lines\":{},\"bytes\":{}}}",
                jstr(&file.display().to_string()),
                doc.total_lines,
                doc.data.len()
            );
        } else {
            print!("{}", String::from_utf8_lossy(&doc.data));
        }
        return Ok(EXIT_OK);
    };

    if is_fm_ref(r) {
        return cmd_show_fm(&doc.data, r, json, show_etag);
    }

    if body && full {
        return Err(("--body and --full are mutually exclusive".into(), EXIT_ERR));
    }
    let use_full = full;
    let bytes: Vec<u8> = (if use_full {
        doc.full_bytes(r).map_err(|e| (e.to_string(), EXIT_SECTION))
    } else {
        doc.own_bytes(r).map_err(|e| (e.to_string(), EXIT_SECTION))
    })?
    .to_vec();

    if show_etag {
        println!("{}", etag_of(&bytes));
        return Ok(EXIT_OK);
    }

    let idx = doc.resolve(r).map_err(|e| (e.to_string(), EXIT_SECTION))?;
    let s = &doc.sections[idx];
    if json {
        let span = if use_full { "full" } else { "body" };
        let end_line = if use_full { s.end_line } else { s.own_end_line };
        println!(
            "{{\"ref\":{},\"title\":{},\"level\":{},\"span\":\"{}\",\"line\":{},\"lines\":{},\"etag\":{},\"text\":{}}}",
            jstr(&s.ref_),
            jstr(&s.title),
            s.level,
            span,
            s.start_line + 1,
            end_line.saturating_sub(s.start_line),
            jstr(&etag_of(&bytes)),
            jstr(&String::from_utf8_lossy(&bytes))
        );
    } else {
        print!("{}", String::from_utf8_lossy(&bytes));
    }
    Ok(EXIT_OK)
}

/// Is `r` a frontmatter ref (`fm` or `fm.<key>`)?
pub(crate) fn is_fm_ref(r: &str) -> bool {
    r == "fm" || r.starts_with("fm.")
}


/// Byte-level line splitter — inverse of `commit`'s `split_inclusive('\n')`
/// on valid UTF-8. Local helper (the kernel's is private to `kb-core`).
pub(crate) fn lines_of_str(data: &[u8]) -> Vec<String> {
    let s = std::str::from_utf8(data).unwrap_or("");
    s.split_inclusive('\n').map(|l| l.to_string()).collect()
}


/// Show a frontmatter frame or a single field in `data`.
/// `fm` prints the whole `---\n...\n---` block including its delimiters.
/// `fm.<key>` prints *just* that field's line(s).
pub(crate) fn cmd_show_fm(
    data: &[u8],
    r: &str,
    json: bool,
    show_etag: bool,
) -> Kb {
    let fm = match fm::find_fm(data) {
        Some(f) => f,
        None => return Err(("no frontmatter frame found".into(), EXIT_SECTION)),
    };
    let lines = lines_of_str(data);
    // Include both delimiters (open..=close) in the "frame" view.
    let frame: Vec<&str> = lines
        .iter()
        .skip(fm.open_line)
        .take(fm.close_line - fm.open_line + 1)
        .map(|s| s.as_str())
        .collect();
    let interior: Vec<&str> = if frame.len() >= 2 {
        frame[1..frame.len() - 1].to_vec()
    } else {
        Vec::new()
    };
    let key: Option<&str> = r.strip_prefix("fm.").map(|s| s.trim());
    let text: String = match key {
        None => frame.concat(),
        Some(k) => {
            // Find the `key:` line, then emit it plus any trailing contiguous
            // `- item` / blank / comment lines (list fields). Interior lines
            // already carry their trailing newline (split_inclusive), so we
            // push them as-is.
            let mut out = String::new();
            let mut i = 0usize;
            while i < interior.len() {
                let t = interior[i].trim_start_matches('\u{feff}').trim();
                let Some(colon) = t.find(':') else { i += 1; continue; };
                if t[..colon].trim() == k {
                    out.push_str(interior[i]);
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    let mut j = i + 1;
                    while j < interior.len() {
                        let nt = interior[j].trim_start_matches('\u{feff}').trim();
                        if nt.starts_with('-') || nt.is_empty() || nt.starts_with('#') {
                            out.push_str(interior[j]);
                            if !out.ends_with('\n') {
                                out.push('\n');
                            }
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    break;
                }
                i += 1;
            }
            out
        }
    };
    if show_etag {
        println!("{}", etag_of(text.as_bytes()));
        return Ok(EXIT_OK);
    }
    if json {
        println!(
            "{{\"ref\":{},\"etag\":{},\"text\":{}}}",
            jstr(r),
            jstr(&etag_of(text.as_bytes())),
            jstr(&text)
        );
    } else {
        print!("{text}");
    }
    Ok(EXIT_OK)
}


/// Extract the `<key>` part of a `fm.<key>` ref.
pub(crate) fn fm_key(r: &str) -> Option<&str> {
    r.strip_prefix("fm.")
}


/// Resolve the fm value from the optional `--value` flag, falling back to the
/// text source.
pub(crate) fn fm_resolve_value(value: &Option<String>, text: &TextSource, file: &Path) -> Result<String, (String, i32)> {
    if let Some(v) = value {
        return Ok(v.clone());
    }
    read_text(text, file)
}

