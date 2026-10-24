#[cfg(feature = "editor")]
use crate::*;

#[cfg(feature = "editor")]
pub(crate) static EDITOR_BUFFER_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);


#[cfg(feature = "editor")]
pub(crate) fn run_editor(initial: &[u8]) -> Result<Vec<u8>, (String, i32)> {
    // Open `initial` in $VISUAL, then $EDITOR, then `vi`, in a uniquely-named
    // temp file; return the post-edit bytes.
    let seq = EDITOR_BUFFER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!(
        "kb-edit-{}-{}-{}.md",
        std::process::id(),
        seq,
        nanos
    ));
    std::fs::write(&tmp, initial).map_err(|e| (format!("write temp edit buffer: {e}"), EXIT_ERR))?;
    let editor = pick_editor();
    let st = std::process::Command::new(&editor)
        .arg(&tmp)
        .status()
        .map_err(|e| (format!("launch {editor}: {e}"), EXIT_ERR))?;
    if !st.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err((format!("editor {editor} exited with {st}"), EXIT_ERR));
    }
    // A read failure after a successful editor exit is real (e.g. the buffer
    // vanished), so surface it as an error rather than silently treating it as
    // "no changes" (B5: the old `unwrap_or_default()` swallowed it to `[]`).
    let bytes = std::fs::read(&tmp).map_err(|e| (format!("read back edit buffer {}: {e}", tmp.display()), EXIT_ERR))?;
    let _ = std::fs::remove_file(&tmp);
    Ok(bytes)
}


#[cfg(feature = "editor")]
pub(crate) fn pick_editor() -> String {
    for k in ["VISUAL", "EDITOR"] {
        if let Some(v) = std::env::var(k).ok().filter(|v| !v.trim().is_empty()) {
            return v.split_whitespace().next().unwrap_or("vi").to_string();
        }
    }
    // W3: the hard fallback is platform-appropriate. `vi` is not present on a
    // stock Windows install, but `notepad` (invoked as `notepad`) is; on POSIX
    // the conventional fallback is `vi`.
    if cfg!(windows) {
        "notepad".to_string()
    } else {
        "vi".to_string()
    }
}


/// The *name* of the env var that chose the current editor, for a clear
/// "set it to something you have" hint. Returns `None` when the editor is
/// the hard `vi` fallback (neither `$VISUAL` nor `$EDITOR` was set).
#[cfg(feature = "editor")]
pub(crate) fn editor_source() -> Option<&'static str> {
    if std::env::var("VISUAL").ok().filter(|v| !v.trim().is_empty()).is_some() {
        Some("VISUAL")
    } else if std::env::var("EDITOR").ok().filter(|v| !v.trim().is_empty()).is_some() {
        Some("EDITOR")
    } else {
        None
    }
}


/// Open an already-written file in the user's editor **in place** ($VISUAL,
/// then $EDITOR, then `vi`). Used by `kb create -e` (the scaffold on disk
/// *is* the buffer) and the TUI `v` shortcut.
///
/// The two failure modes are reported distinctly because they mean
/// different things: **launch failed** (the editor binary was not found —
/// "you don't have `$VISUAL`/`$EDITOR`/`vi` installed") names the env var
/// that was used and leaves the file untouched; a **non-zero exit** means
/// the editor *ran* and aborted, leaving the file on disk as you found it.
///
/// SECURITY_REPORT 3.1: gates with `run_editor`/`pick_editor` — in a
/// hardened build the whole editor surface (incl. `-e`, which calls here
/// via `cmd_create`) is compiled out; clap rejects `-e` as unexpected.
#[cfg(feature = "editor")]
pub(crate) fn edit_in_place(path: &Path) -> Result<(), (String, i32)> {
    let editor = pick_editor();
    let res = std::process::Command::new(&editor)
        .arg(path)
        .status();
    match res {
        Ok(st) if st.success() => Ok(()),
        Ok(st) => Err((
            format!(
                "editor {editor} ran but exited with {st} — {} was left on disk as you found it (open it again to continue, or delete it)",
                path.display()
            ),
            EXIT_ERR,
        )),
        Err(e) => {
            let hint = match editor_source() {
                Some(k) => {
                    format!(
                        "{k}={editor} was not found / could not be launched. Set VISUAL or EDITOR to an editor you have installed (and make sure it is on the PATH), then retry."
                    )
                }
                None => "no editor available: VISUAL and EDITOR are unset and the fallback editor is not on the PATH. Set VISUAL or EDITOR to an editor you have installed, then retry.".to_string(),
            };
            Err((
                format!("could not launch {editor}: {e} — {hint}"),
                EXIT_ERR,
            ))
        }
    }
}

