#[cfg(feature = "editor")]
use crate::*;

#[cfg(feature = "editor")]
pub(crate) static EDITOR_BUFFER_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);


/// Run the user's editor (`$VISUAL`, then `$EDITOR`, then a platform
/// fallback) against a *temp buffer* seeded with `initial`, and return the
/// post-edit bytes. The buffer is removed before returning on every path
/// (success, non-zero exit, launch failure, read-back failure).
///
/// (A3 unification: the old `run_editor` and `edit_in_place` were two ~50-line
/// functions with the *same* editor-resolution / launch / cleanup bodies,
/// differing only in (a) whether the file is a temp buffer or an in-place
/// path and (b) the failure-message wording. That drifted: pre-A3 the launch
/// hint logic (`kb/main.rs:3131-3145`) lived only in `edit_in_place`, and a
/// non-zero exit in `run_editor` got the generic "editor exited" message. A
/// single body, branched only by "where the buffer lives", keeps the two
/// failure modes — *launch failed* vs *non-zero exit* — in one place.)
#[cfg(feature = "editor")]
pub(crate) fn run_editor(initial: &[u8]) -> Result<Vec<u8>, (String, i32)> {
    run_editor_in(initial, /* in_place */ None)
}


/// Run the user's editor **in place** against an already-written file. Used
/// by `kb create -e` (the scaffold on disk *is* the buffer) and the TUI `v`
/// shortcut. The file is left on disk as-is on every path, so success is
/// signalled as `Ok(Vec::new())` — the caller already holds the path.
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
///
/// (A3 unification: the old `edit_in_place` was a second copy of the
/// `run_editor` launch / hint / cleanup body with in-place semantics; the
/// two have now been merged below so the two failure branches stay in one
/// place.)
#[cfg(feature = "editor")]
pub(crate) fn edit_in_place(path: &Path) -> Result<(), (String, i32)> {
    run_editor_in(&[], Some(path)).map(|_| ())
}


/// The single editor-launch body that backs both public entrypoints (A3).
///
/// Behaviour is identical to the pre-A3 split of `run_editor` (a temp
/// buffer) + `edit_in_place` (an in-place file):
///   * pick the editor by the same `$VISUAL` → `$EDITOR` → `vi` chain,
///   * launch it against the chosen file,
///   * on **success**, read back the file's bytes and remove the temp buffer
///     (an in-place file is returned as `Ok(Vec::new())` — its bytes are the
///     file's, which the caller already has),
///   * on **non-zero exit** surface an explicit "editor ran but exited
///     non-zero — file left as-is" message,
///   * on **launch failure** surface an explicit "no editor found" hint that
///     names the env var that was actually tried,
///   * on **read-back failure** (success path, temp buffer) surface an
///     explicit "buffer vanished" message — B5: this used to be swallowed to `[]`.
#[cfg(feature = "editor")]
fn run_editor_in(
    initial: &[u8],
    in_place: Option<&Path>,
) -> Result<Vec<u8>, (String, i32)> {
    // Choose the target file: an in-place path is used verbatim; otherwise a
    // uniquely-named temp buffer is written so the editor sees a plain file.
    let tmp: Option<PathBuf> = match in_place {
        Some(_) => None,
        None => {
            let seq = EDITOR_BUFFER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let t = std::env::temp_dir().join(format!(
                "kb-edit-{}-{}-{}.md",
                std::process::id(),
                seq,
                nanos
            ));
            std::fs::write(&t, initial)
                .map_err(|e| (format!("write temp edit buffer: {e}"), EXIT_ERR))?;
            Some(t)
        }
    };
    let target: &Path = match (&tmp, in_place) {
        (_, Some(p)) => p,
        (Some(t), None) => t.as_path(),
        (None, None) => unreachable!("run_editor_in: tmp is Some iff in_place is None"),
    };
    let editor = pick_editor();
    // Note the launch-vs-exit distinction: `status()` returns `Err` when the
    // bin couldn't even be spawned (env var unset, binary missing, etc.) and
    // `Ok(Status)` — possibly a failure `Status` — when it ran. Both surface
    // distinct messages so the caller can act on the correct one.
    match std::process::Command::new(&editor).arg(target).status() {
        Ok(st) if st.success() => {
            let out = match &tmp {
                Some(t) => {
                    let bytes = std::fs::read(t).map_err(|e| (
                        format!("read back edit buffer {}: {e}", t.display()),
                        EXIT_ERR,
                    ))?;
                    let _ = std::fs::remove_file(t);
                    bytes
                }
                None => Vec::new(),
            };
            Ok(out)
        }
        Ok(st) => {
            if let Some(t) = &tmp {
                let _ = std::fs::remove_file(t);
            }
            // The wording differs by which case we're in: a temp buffer we
            // just removed, or an in-place file the caller owns. Both are
            // "you aborted the editor"; only the on-disk follow-up is
            // different.
            let msg = match &tmp {
                Some(_) => format!(
                    "editor {editor} ran but exited with {st} — {target} was a temporary buffer and has been removed; the page was not modified",
                    target = target.display()
                ),
                None => format!(
                    "editor {editor} ran but exited with {st} — {target} was left on disk as you found it (open it again to continue, or delete it)",
                    target = target.display()
                ),
            };
            Err((msg, EXIT_ERR))
        }
        Err(e) => {
            if let Some(t) = &tmp {
                let _ = std::fs::remove_file(t);
            }
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


/// Pick the editor: `$VISUAL`, then `$EDITOR`, then a platform-specific
/// fallback (`notepad` on Windows, `vi` elsewhere). Shared by `run_editor`
/// and `edit_in_place` so the chain and the fallback are defined exactly
/// once (A3: was duplicated in both pre-A3).
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
