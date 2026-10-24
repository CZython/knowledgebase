use crate::*;

/// Under `--json` an etag mismatch must still leave exactly one JSON object
/// on stdout: the agent contract promises stdout is always a clean JSON
/// document (success *or* failure) and `--json` suppresses the prose prompt.
/// Emit the failure envelope here; the caller then returns `Err` as before so
/// `run` prints the human message to stderr and the process exits `EXIT_ETAG`.
/// `scope` disambiguates a section ref (`"1.2"`) from the whole file (`"*"`).
pub(crate) fn emit_etag_error_json(scope: &str, expected: &str, actual: &str) {
    println!(
        "{{\n  \"ok\": false,\n  \"applied\": false,\n  \"op\": \"etag_check\",\n  \"scope\": {},\n  \"code\": {},\n  \"error\": {},\n  \"etag_expected\": {},\n  \"etag_actual\": {}\n}}",
        jstr(scope),
        EXIT_ETAG,
        jstr(&format!("etag mismatch: expected {expected}, got {actual} (section/file changed since read)")),
        jstr(expected),
        jstr(actual),
    );
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

