use crate::*;

use std::time::SystemTime;

/// the single owner of the
/// "unique, unguessable temp file name inside `dir`" recipe.
///
/// Pre-this the logic lived in **three** places with divergent hardening:
/// the editor buffer (pid+seq+nanos, hardened in F8), `atomic_write` (a
/// *fixed* `.{name}.kb~tmp` + `create`/`O_CREAT|O_TRUNC`), and the alias
/// `--export` (bare `std::fs::write`). Because they were separate, the
/// editor path got hardened while the other two did not. One owner, one
/// answer, in the same "single-owner invariant" style as `sql_quote_lower`
/// (3.4/C4): a fix to the naming discipline now propagates to every caller.
///
/// The name carries `pid` (process-unique), a per-process monotonic `seq`
/// (in-process uniqueness), and the sub-second wall-clock `nanos`
/// (cross-process + time uniqueness), so a hostile actor in a shared
/// multi-user directory can't pre-plant a symlink at a *guessable* path in
/// the window between the write and the subsequent `rename` (the 3.2 /
/// 3.19 / 3.20 class).
pub(crate) static KB_TMP_SEQ: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Build a same-`dir` temp path that is unique to this process/invocation.
///
/// `dir` is the target's parent (so the subsequent `rename` is guaranteed to
/// be within one filesystem), and `base` is a human-readable stem (e.g. the
/// target file name, or `kb-edit`). The returned path is *always* fresh —
/// two callers, or the same caller twice, never share a name.
pub(crate) fn unique_tmp_in(dir: &Path, base: &str) -> PathBuf {
    let seq = KB_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("{base}.kb-{}-{}-{nanos}~tmp", std::process::id(), seq))
}

/// Under `--json` an etag mismatch must still leave exactly one JSON object
/// on stdout: the agent contract promises stdout is always a clean JSON
/// document (success *or* failure) and `--json` suppresses the prose prompt.
/// Emit the failure envelope here; the caller then returns `Err` as before so
/// `run` prints the human message to stderr and the process exits `EXIT_ETAG`.
/// `scope` disambiguates a section ref (`"1.2"`) from the whole file (`"*"`).
pub(crate) fn emit_etag_error_json(scope: &str, expected: &str, actual: &str) {
    let mut o = JObject::new();
    o.b("ok", false);
    o.b("applied", false);
    o.s("op", "etag_check");
    o.s("scope", scope);
    o.i("code", EXIT_ETAG as i64);
    o.s("error", &format!("etag mismatch: expected {expected}, got {actual} (section/file changed since read)"));
    o.s("etag_expected", expected);
    o.s("etag_actual", actual);
    println!("{}", o.render());
}


/// Enforce `--if-unchanged` over the *whole file* (the fm-op etag contract).
pub(crate) fn check_etag_whole(data: &[u8], flags: &WriteFlags) -> Kb {
    let Some(expected) = flags.if_unchanged.as_deref() else {
        return Ok(EXIT_OK);
    };
    let cur = etag_of(data);
    if cur != expected {
        if flags.json { emit_etag_error_json("*", expected, &cur); }
        return Err((
            format!("etag mismatch: expected {expected}, got {cur} (file changed since read)"),
            EXIT_ETAG,
        ));
    }
    Ok(EXIT_OK)
}


/// Enforce `--if-unchanged`: the target section's etag must still equal the
/// value the caller captured earlier. The etag is FNV-1a 64-bit over the
/// target section's own-body bytes (or the whole document when no ref is
/// given, e.g. `insert --placement top|end`).
pub(crate) fn check_etag(doc: &Doc, ref_: Option<&str>, flags: &WriteFlags) -> Kb {
    let Some(expected) = flags.if_unchanged.as_deref() else {
        return Ok(EXIT_OK);
    };
    let bytes = match ref_ {
        Some(r) => doc.own_bytes(r).map_err(|e| (e.to_string(), EXIT_SECTION))?.to_vec(),
        None => doc.data.clone(),
    };
    let cur = etag_of(&bytes);
    if cur != expected {
        let scope = ref_.unwrap_or("*");
        if flags.json { emit_etag_error_json(scope, expected, &cur); }
        return Err((
            format!("etag mismatch: expected {expected}, got {cur} (section changed since read)"),
            EXIT_ETAG,
        ));
    }
    Ok(EXIT_OK)
}

