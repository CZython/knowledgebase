use crate::*;

pub(crate) fn cmd_add(dbpath: Option<&Path>, path: &Path, ext: &[String], include_hidden: bool, json: bool) -> Kb {
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| (format!("{path:?}: {e} (must exist and be a directory)"), EXIT_ERR))?;
    if !canonical.is_dir() {
        return Err((format!("{}: not a directory", canonical.display()), EXIT_ERR));
    }
    let extra_exts = if ext.is_empty() { None } else { Some(ext.join("\n")) };
    let db = open_db(dbpath)?;
    db.add_root(&canonical.to_string_lossy(), extra_exts.as_deref(), include_hidden)
        .map_err(|e| (format!("add_root: {e}"), EXIT_ERR))?;
    if json {
        let summary = kb_refresh::refresh(&db)
            .map_err(|e| (format!("refresh: {e}"), EXIT_ERR))?;
        println!(
            "{{\n  \"registered\": {},\n  \"refresh\": {}\n}}",
            jstr(&canonical.to_string_lossy()),
            refresh_summary_json(false, &summary),
        );
        return Ok(EXIT_OK);
    }
    println!("root registered: {}", canonical.display());
    refresh_all(&db, false, false)
}

