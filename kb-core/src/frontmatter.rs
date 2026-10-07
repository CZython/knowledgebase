//! frontmatter — the *exec* YAML-subset parser for a page's metadata.
//!
//! Plain Rust, no `verus!` block, no side effects. The verified invariants
//! about this contract (case-insensitive `type:`, the ≤255 description
//! bound, non-empty tag / search-term entries) live in `spec/fm_spec.rs`
//! and are discharged by `./verify.sh`. This module provides the *code*
//! that the unit tests below and the `spec/fm_spec.rs` lemmas both point at.
//!
//! ## Contract (PLAN.md §3, §4, §7)
//!
//! - **Permissive YAML-subset.** Supported in v1:
//!     - scalar fields: `key: value`
//!     - inline lists:  `key: [a, b, "quoted"]`
//!     - block lists:   `key:` followed by `- item` lines
//!     - `#` comments:  full-line and trailing (`value  # note`)
//!
//!   **Nested maps are not supported** (no `key:` sub-maps).
//! - **Frontmatter framing:** the file must open with a `---` line (line 1,
//!   after trailing whitespace). The block ends at the next `---` line.
//!   Anything before the first `---` is a *no-frontmatter* page and yields
//!   `Frontmatter::from_page(&Page::none)`. (BOM tolerated at byte 0.)
//! - **`type:` is the gating field.** A page with no recognized `type:` is
//!   *parseable* but *not indexable* — see `Frontmatter::is_indexable`.
//!   Matched case-insensitively; the parser does not warn here, the caller
//!   (M5 indexer) does.
//! - **`description:`** — optional one-line summary. Hard limit **≤ 255
//!   characters**. Over-limit values are *retained in the parse result*
//!   (so the caller can warn) but flagged via `description_too_long`.
//! - **`tags:`** — trimmed, deduplicated (first-occurrence order), empty
//!   entries dropped. Surrounding quotes (single or double) are stripped
//!   from each entry.
//! - **`search_terms:`** — entries kept **verbatim** (a multi-word phrase
//!   like `"token bucket"` retains its double quotes in this field). The
//!   *gate* (M3) compares the unquoted whole text case-insensitively.
//!
//! ## What `from_page` returns
//!
//! The parser never panics on user-controlled input; it returns a
//! `Frontmatter` record plus a `body: String` (the remainder of the file
//! after the closing `---`). Errors are *warnings-as-fields* (e.g.
//! `unknown_type_present`) rather than `Result`s, because a partially
//! valid file (missing `type:`) is a legitimate corpus entry that the
//! indexer may want to count-and-skip (§3: "report the count of skipped
//! (no-type) files as a warning, not an error").

use super::meta::NoteType;

/// The parsed view of one page. Field-by-field contract above.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub page_type: Option<NoteType>,
    /// True if a `type:` key was present but its value was not one of the
    /// three known kinds. Callers can distinguish "missing" (no-type) from
    /// "wrong" (typo). Both are non-indexable, but the warning text differs.
    pub unknown_type_present: bool,
    /// The raw `type:` scalar as written (trimmed, quotes stripped). `None`
    /// when absent. Kept so `kb refresh` can surface it in the skip warning.
    pub type_raw: Option<String>,
    /// Optional. Not length-checked here — see `description_too_long`.
    pub title: Option<String>,
    pub description: Option<String>,
    /// True when `description` is present and over the 255-char bound
    /// (PLAN.md §3 — "violations are warned and the field is dropped; the
    /// page still indexes" — the *drop* is applied by the caller, not here,
    /// so the caller can emit the warning).
    pub description_too_long: bool,
    /// Trimmed, deduped, non-empty. Quote-wrapped entries have their
    /// wrapping quotes removed. Order = first occurrence.
    pub tags: Vec<String>,
    /// Verbatim (quotes retained where present), trimmed. Non-empty only.
    pub search_terms: Vec<String>,
    /// The raw frontmatter block (between the two `---` delimiters), newlines
    /// preserved. Stored for the FTS `frontmatter_text` column (§8).
    pub frontmatter_text: String,
    /// The page body *after* the closing `---` (or the whole file when no
    /// frontmatter was found). Leading blank lines are kept as-is; the
    /// indexer / `kb section` (M4) handle their own trim.
    pub body: String,
}

impl Frontmatter {
    /// A *no-frontmatter* page: nothing to index by frontmatter, `body`
    /// is the whole file, `page_type` is `None`.
    pub fn none(text: &str) -> Self {
        Frontmatter {
            page_type: None,
            unknown_type_present: false,
            type_raw: None,
            title: None,
            description: None,
            description_too_long: false,
            tags: Vec::new(),
            search_terms: Vec::new(),
            frontmatter_text: String::new(),
            body: text.to_string(),
        }
    }

    /// A page is indexable **iff** its `type:` resolved to one of the three
    /// known kinds. (§4 "type: is required to be indexed.")
    #[inline]
    pub fn is_indexable(&self) -> bool {
        self.page_type.is_some()
    }

    /// True if the description is present and over the 255-char bound.
    /// The parser does NOT drop it (see field docs); the *indexer* applies
    /// the drop + warning. This helper just re-exposes the check so two
    /// call sites don't re-implement the arithmetic.
    #[inline]
    pub fn description_over_limit(&self) -> bool {
        self.description_too_long
    }
}

/// Parse YAML-subset frontmatter from a page's full text.
///
/// # Rules (documented in the module header)
///
/// - If the first line (after optional BOM, before trailing whitespace) is
///   not exactly `---`, return `Frontmatter::none(text)` — the body is the
///   whole file, no frontmatter fields.
/// - Otherwise read lines until the next `---`. Build fields per the
///   contract.
///
/// # Returns
///
/// A `Frontmatter` with `.body` set to the text after the closing `---`.
/// No `Result`: malformed lines are silently dropped (a lenient parser is
/// easier to ship than to recover from; the M5 indexer reports *which*
/// pages had warnings via `type_raw` / `unknown_type_present`).
pub fn parse(text: &str) -> Frontmatter {
    let text = text.strip_prefix("\u{feff}").unwrap_or(text);

    // Split into lines (preserve trailing newline count for body).
    // We need the closing delimiter + the body that follows it.
    let mut lines = text.split_inclusive('\n');
    let first = match lines.next() {
        Some(l) => l,
        None => return Frontmatter::none(text),
    };
    if first.trim() != "---" {
        return Frontmatter::none(text);
    }

    // The lines that follow the opening `---` (each element INCLUDES its
    // trailing `\n` thanks to `split_inclusive`). We partition them:
    //   * `raw`  — the frontmatter block (before the closing `---`), and
    //   * `body` — everything after the closing `---` (verbatim, incl. newlines).
    let rest_lines: Vec<&str> = lines.collect();
    let close_idx = rest_lines.iter().position(|l| l.trim() == "---");

    let (raw, body) = match close_idx {
        Some(idx) => {
            let raw_text: String = rest_lines[..idx].join("");
            let body: String = rest_lines[idx + 1..].join("");
            (raw_text, body)
        }
        None => {
            // Lenient fallback: no closing delimiter found → the whole remainder
            // *is* the frontmatter block; the body is empty.
            (rest_lines.join(""), String::new())
        }
    };

    // Now parse `raw` (a String) into the field set.
    let fm_lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
    let mut fm = Frontmatter {
        page_type: None,
        unknown_type_present: false,
        type_raw: None,
        title: None,
        description: None,
        description_too_long: false,
        tags: Vec::new(),
        search_terms: Vec::new(),
        // Filled in after we're done reading `fm_lines` (which borrows `raw`).
        frontmatter_text: String::new(),
        body,
    };

    let n = fm_lines.len();
    let mut i = 0usize;

    while i < n {
        let line = fm_lines[i].trim();
        if line.is_empty() || line.starts_with('#') {
            i += 1;
            continue;
        }

        // Try: `key: value`
        if let Some(colon) = line.find(':') {
            let key = line[..colon].trim();
            let rest = line[colon + 1..].trim();
            if rest.is_empty() {
                // Might be a block list: consume subsequent `- item` lines.
                let mut items: Vec<String> = Vec::new();
                let mut j = i + 1;
                while j < n {
                    let nxt = fm_lines[j].trim();
                    if nxt.starts_with('-') {
                        items.push(nxt.trim_start_matches('-').trim().to_string());
                        j += 1;
                    } else if nxt.is_empty() || nxt.starts_with('#') {
                        j += 1;
                    } else {
                        break;
                    }
                }
                if !key.is_empty() {
                    apply_key(&mut fm, key, KeyKind::List, items);
                }
                i = j;
                continue;
            } else if rest.starts_with('[') {
                // Inline list: `[a, b, "quoted"]`
                let inner = inner_of_inline_list(rest);
                let items = split_top_commas(inner)
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>();
                if !key.is_empty() {
                    apply_key(&mut fm, key, KeyKind::List, items);
                }
                i += 1;
                continue;
            } else {
                // Scalar. Strip trailing comment.
                let scalar = strip_trailing_comment(rest);
                if !key.is_empty() {
                    apply_key(&mut fm, key, KeyKind::Scalar, vec![scalar]);
                }
                i += 1;
                continue;
            }
        }

        // Line didn't match any rule (`- item` not under a key, or a stray
        // non-comment line). Lenient: skip it.
        i += 1;
    }

    // Enforce the description bound *as a flag* — the parse result still
    // carries the value, so the caller can warn and drop it if desired.
    if let Some(d) = fm.description.as_ref() {
        if d.chars().count() > 255 {
            fm.description_too_long = true;
        }
    }

    // Now that `fm_lines` has been fully consumed, we can move the raw block.
    drop(fm_lines);
    fm.frontmatter_text = raw;

    fm
}

#[derive(PartialEq, Eq)]
enum KeyKind {
    Scalar,
    List,
}

fn apply_key(fm: &mut Frontmatter, key: &str, kind: KeyKind, items: Vec<String>) {
    match (key, kind) {
        ("type", KeyKind::Scalar) => {
            let v = items.first().cloned().unwrap_or_default();
            let v = v.trim().to_string();
            fm.type_raw = Some(v.clone());
            match NoteType::parse(&v) {
                Some(t) => {
                    fm.page_type = Some(t);
                    fm.unknown_type_present = false;
                }
                None => {
                    fm.page_type = None;
                    fm.unknown_type_present = true;
                }
            }
        }
        ("title", KeyKind::Scalar) => {
            let v = items.first().cloned().unwrap_or_default();
            let v = v.trim().to_string();
            if !v.is_empty() {
                fm.title = Some(v);
            }
        }
        ("description", KeyKind::Scalar) => {
            let v = items.first().cloned().unwrap_or_default();
            let v = v.trim().to_string();
            if !v.is_empty() {
                fm.description = Some(v);
            }
        }
        ("tags", KeyKind::List) => {
            // Trim, unquote, drop empties, dedupe first-occurrence.
            let mut seen: Vec<String> = Vec::new();
            for raw in items {
                let cleaned = unquote(raw.trim());
                if cleaned.is_empty() {
                    continue;
                }
                if !seen.contains(&cleaned) {
                    seen.push(cleaned);
                }
            }
            fm.tags = seen;
        }
        ("search_terms", KeyKind::List) => {
            // Verbatim: trim, drop empties, keep quotes. (Dedupe is *not*
            // required for gates; the M3 matching compares the whole text,
            // so duplicates would just be redundant work, not wrong.)
            let mut out: Vec<String> = Vec::new();
            for raw in items {
                let t = raw.trim().to_string();
                if !t.is_empty() {
                    out.push(t);
                }
            }
            fm.search_terms = out;
        }
        _ => {
            // Unknown key — ignored. Lenient.
        }
    }
}

/// Remove a single matching pair of surrounding quotes (single or double).
/// No-op if not quoted or unbalanced.
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"')
            || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Strip a trailing ` # comment`. A `#` only starts a comment when it's the
/// first char or preceded by whitespace. (Frontmatter YAML semantics; we
/// don't need the more aggressive "inline `#` inside quoted string" rule in
/// v1 — if you want one, put the `#` inside quotes and it survives because
/// the leading-whitespace check gates the strip.)
fn strip_trailing_comment(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len());
    let mut in_single = false;
    let mut in_double = false;
    for (i, c) in bytes.iter().enumerate() {
        match *c {
            b'"' if !in_single => in_double = !in_double,
            b'\'' if !in_double => in_single = !in_single,
            b'#'
                if !in_double && !in_single && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') =>
            {
                break;
            }
            _ => out.push(*c),
        }
    }
    let r = String::from_utf8_lossy(&out).to_string();
    // Trim the trailing spaces we left behind when we truncated a comment.
    r.trim_end().to_string()
}

/// Strip a leading `[` and (if present) a trailing `]` from an inline-list
/// string, returning the inner region. (Kept as a small helper so tests can
/// exercise it standalone.)
fn inner_of_inline_list(s: &str) -> &str {
    let t = s.trim();
    if t.starts_with('[') && t.ends_with(']') && t.len() >= 2 {
        &t[1..t.len() - 1]
    } else if let Some(stripped) = t.strip_prefix('[') {
        stripped
    } else {
        t
    }
}

/// Split on top-level commas, respecting single/double quotes.
fn split_top_commas(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_double = !in_double;
                cur.push(c);
            }
            '\'' => {
                in_single = !in_single;
                cur.push(c);
            }
            ',' if !in_double && !in_single => {
                out.push(cur.clone());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// =============================================================================
// Byte-level frontmatter field kernel (M6.5 — the "easy add/remove/edit any
// frontmatter field" surface). Pure: no IO, no `std::fs`, no panics.
//
// Design (locked by the user):
//
//   * **No new subcommand.** Frontmatter is addressable through the *same*
//     `kb section` / `kb show` commands via a `fm` / `fm.<key>` ref namespace.
//   * All writes are byte-level splices over `&[u8]`, so the rest of the
//     file is preserved exactly — the same invariants `splice` pins.
//   * Every function has a **total** contract: for any input it either
//     returns `Ok(…)` / `NoFrontmatter` / `FieldAbsent` / `ValueAbsent`
//     (named, `Result`/`Option`) — **never panics, never allocs garbage**.
//     (This is the "read can't panic" boundary for (1a) and the "only
//     well-formed files enter the DB" boundary for (1b).)
//
// Key invariants the *spec* side will pin (see §13g — pending):
//
//   * **I1** (framing): `find_fm(data)` returns `Some(fm)` iff `data` starts
//     with `\uFEFF?` + `---\n` and has a later `---\n` line; the two are
//     distinct and the region between them is the frontmatter.
//   * **I2** (field locality): `replace_field` / `append_list_item` /
//     `remove_field` / `remove_list_item` only touch lines **within**
//     `[fm.open_line, fm.close_line)`; outside that range the byte stream
//     is byte-identical (`splice`-equivalence, proven in §13g).
//   * **I3** (idempotence): `remove_field(fm_field(fm_field(x, v), v), v)
//     = NoFrontmatter/FieldAbsent` — the second call finds nothing.
//   * **I4** (total): none of the functions call `.unwrap()` / `.expect()`
//     / indexed read of a possibly-empty collection in the non-test zone;
//     every `?` is a total-function guard, every `Err` is a named outcome.
//
// Exec↔spec pinning: the unit tests below (`mod fm_tests`) exercise the
// concrete corpus (I1–I4 by example); the Verus spec (next) proves the
// *shape* of the invariants for all inputs.
// =============================================================================

/// A **well-formed** frontmatter frame inside `data`. Offsets are 0-based
/// line indices (consistent with `Doc::Section::*_line`). `Some(…)` means
/// the frame *is present* on `data`; `None` means the file has no frontmatter.
///
/// **Well-formedness (I1):** `present == true` iff both an *opening* `---`
/// line at `open_line` and a *closing* `---` line at `close_line` exist
/// with `open_line < close_line`. The file may or may not carry a BOM; a
/// BOM is legal only at byte 0 and is stripped from the first line for
/// matching but preserved in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fm {
    /// The opening `---` line index (0-based, after any BOM).
    pub open_line: usize,
    /// The closing `---` line index. Exclusive bound: the body starts at
    /// `close_line + 1`.
    pub close_line: usize,
}

impl Fm {
    /// The inclusive range of lines that *are* frontmatter.
    pub fn lines(&self) -> std::ops::RangeInclusive<usize> {
        self.open_line..=self.close_line
    }
}

/// Line iteration over `data`. Char-aware (via `from_utf8_lossy`),
/// **total** (never panics on malformed UTF-8 — lossy yields U+FFFD and we
/// still split on `\n`), and preserves the trailing-newline count so a
/// round-trip join exactly reproduces the input byte-for-byte on valid
/// UTF-8.
///
/// Returns a `Vec` of borrowed string slices (not owned) to keep this the
/// *cheapest* total helper the caller can build on.
fn lines_of(data: &[u8]) -> Vec<String> {
    // `split_inclusive('\n')` on a `&str` already preserves newlines. We
    // `to_string()` per line so callers can push/replace freely; the join
    // identity holds on valid UTF-8 (asserted by `fm_tests::roundtrip`).
    // SECURITY_REPORT 3.7: `from_utf8` (strict) + `unwrap_or("")` would drop
    // the *entire* file's lines on any invalid UTF-8 byte — a CP1252 char
    // pasted into the body made an otherwise-valid frontmatter frame appear
    // absent. `from_utf8_lossy` (the documented behaviour above) maps the bad
    // bytes to U+FFFD and leaves the frame intact, so `find_fm` + the rewrite
    // helpers still see the lines they operate on.
    let s = String::from_utf8_lossy(data);
    if s.is_empty() {
        return Vec::new();
    }
    s.split_inclusive('\n').map(|l| l.to_string()).collect()
}

/// `find_fm`: locate the frontmatter frame in `data`, per invariant I1.
/// Total: `Some(Fm)` iff a well-formed frame is present, else `None`.
pub fn find_fm(data: &[u8]) -> Option<Fm> {
    let lines = lines_of(data);
    if lines.is_empty() {
        return None;
    }
    // First line (after any BOM) must be exactly `---`.
    if lines[0].trim_start_matches('\u{feff}').trim() != "---" {
        return None;
    }
    // A closing `---` strictly after the opener (open < close).
    let open = 0usize;
    let close = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(i, l)| *i > open && l.trim().trim_start_matches('\u{feff}') == "---")
        .map(|(i, _)| i)
        .filter(|i| *i > open)?;
    Some(Fm { open_line: open, close_line: close })
}

/// The list values of a top-level list field, in source order. `None` if no
/// frame, the key is absent, or the key holds a scalar (we do not coerce a
/// scalar into a 1-element list). Total.
pub fn list_values(data: &[u8], key: &str) -> Option<Vec<String>> {
    let fm = find_fm(data)?;
    let lines = lines_of(data);
    let key_b = key.to_string();
    let mut i = fm.open_line + 1;
    while i < fm.close_line {
        let line = lines[i].trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with('#') {
            i += 1;
            continue;
        }
        let Some(colon) = line.find(':') else {
            i += 1;
            continue;
        };
        if line[..colon].trim() != key_b {
            i += 1;
            continue;
        }
        // `key:` line found. If `rest` is a scalar (non-empty, non-`[`), this
        // is *not* a list → `None`.
        let rest = line[colon + 1..].trim();
        if !rest.is_empty() && !rest.starts_with('[') {
            return None;
        }
        // List: collect inline items, then contiguous block items.
        let mut vals: Vec<String> = Vec::new();
        if rest.starts_with('[') {
            let inner = rest.trim_start_matches('[').trim_end_matches(']');
            for t in split_top_commas(inner) {
                let c = clean_scalar(&t);
                if !c.is_empty() {
                    vals.push(c);
                }
            }
        }
        let mut j = i + 1;
        while j < fm.close_line {
            let nxt = lines[j].trim_start_matches('\u{feff}').trim();
            if nxt.starts_with('-') {
                vals.push(clean_scalar(nxt.trim_start_matches('-').trim()));
                j += 1;
            } else if nxt.is_empty() || nxt.starts_with('#') {
                j += 1;
            } else {
                break;
            }
        }
        return Some(vals);
    }
    None
}

/// The scalar value of `key`, or `None` if absent, or if it's a list. Total.
pub fn scalar_value(data: &[u8], key: &str) -> Option<String> {
    let fm = find_fm(data)?;
    let lines = lines_of(data);
    let key_b = key.to_string();
    for line in &lines[fm.open_line + 1..fm.close_line] {
        let line = line.trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        if line[..colon].trim() != key_b {
            continue;
        }
        let rest = line[colon + 1..].trim().to_string();
        if rest.is_empty() || rest.starts_with('[') {
            return None; // list (or empty)
        }
        return Some(clean_scalar(&rest));
    }
    None
}

/// Trim *and* strip a single matching pair of surrounding quotes from a
/// scalar token. `clean_scalar` is the one place the writer normalises a
/// value: `trim → strip quotes → trim`. Idempotent and **total**.
fn clean_scalar(t: &str) -> String {
    let t = t.trim();
    let b = t.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"')
            || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        t[1..t.len() - 1].trim().to_string()
    } else {
        t.to_string()
    }
}

// =============================================================================
// Writer ops. All four are **byte-level splices**: they take `data: &[u8]`,
// locate the target line(s) inside the frontmatter frame, and return a new
// `Vec<u8>` where *exactly* the target region has changed and every other
// byte is preserved (invariant I2). `insert` (add field / add list item)
// adds lines. All are total — no `.unwrap()`, no bare index on a maybe-empty
// collection, every failure is a named `FmError`.
// =============================================================================

/// Named failure outcomes for frontmatter writer ops. (A `Result<Vec<u8>,
/// FmError>` keeps the "total, never panics" contract machine-checkable.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FmError {
    /// The file has no well-formed frontmatter frame (I1 `None`).
    NoFrontmatter,
    /// The key is a top-level field but not present as a scalar.
    NotScalar,
    /// The key is a top-level field but present as a list (scalar op).
    IsList,
    /// The key is present as neither (scalar op).
    Missing,
    /// The key is a scalar but a list op was requested.
    IsScalar,
    /// `remove_list_item`: the (key,value) pair is not in the list.
    ValueAbsent,
}

impl std::fmt::Display for FmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            FmError::NoFrontmatter => "file has no frontmatter block (add one first)",
            FmError::NotScalar => "field is not a scalar",
            FmError::IsList => "field is a list",
            FmError::Missing => "field is missing",
            FmError::IsScalar => "field is a scalar, not a list",
            FmError::ValueAbsent => "value not present in the list",
        };
        f.write_str(s)
    }
}

/// Join a line-slice back into one `String`. (Lines were produced by
/// `lines_of`, i.e. `\n`-terminated `split_inclusive`, so a plain join is the
/// exact inverse on valid UTF-8 — pinned by `fm_tests::roundtrip`.)
fn join_lines(lines: &[String]) -> String {
    lines.join("")
}

/// Locate the **0-based line index** (global, over `data`) of a top-level
/// `key:` line inside the frame, if any. Returns `None` if no frame or the
/// key is only a comment / absent.
fn locate_key_line(data: &[u8], key: &str) -> Option<usize> {
    let fm = find_fm(data)?;
    let lines = lines_of(data);
    let key_b = key.to_string();
    let start = fm.open_line + 1;
    let end = fm.close_line; // exclusive: the closing `---`
    for (offset, raw) in lines[start..end].iter().enumerate() {
        let line = raw.trim_start_matches('\u{feff}').trim().to_string();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(colon) = line.find(':') {
            let k = line[..colon].trim().to_string();
            if k == key_b {
                return Some(start + offset);
            }
        }
    }
    None
}

/// `fm set <key> <value>`: replace the scalar value of an existing `key:`
/// OR insert a fresh `key: value` line just before the closing `---`.
/// Never mutates the body, headings, or anything outside the frame.
pub fn set_scalar(data: &[u8], key: &str, value: &str) -> Result<Vec<u8>, FmError> {
    if find_fm(data).is_none() {
        return Err(FmError::NoFrontmatter);
    }
    let value = value.to_string();
    scalar_rewrite(data, key, &value)
}

/// `fm append <key> <value>`: add `value` to the list under `key`. If the
/// field is a scalar, upgrade to a 2-element inline list (scalar, value).
/// If the field is absent, create a 1-element inline list.
pub fn append_list_item(data: &[u8], key: &str, value: &str) -> Result<Vec<u8>, FmError> {
    let fm = find_fm(data);
    let fm = match fm {
        Some(f) => f,
        None => return Err(FmError::NoFrontmatter),
    };
    let _ = fm;
    let value = clean_scalar(value);
    if value.is_empty() {
        return Err(FmError::Missing);
    }
    // If already a list, append at the tail of the block/inline list.
    match list_values(data, key) {
        Some(_) => Ok(list_rewrite_add(data, key, &value)),
        None => {
            // Not a list. If scalar, upgrade; else create a fresh field.
            match scalar_value(data, key) {
                Some(s) => Ok(list_rewrite_upgrade(data, key, &s, &value)),
                None => Ok(list_rewrite_new(data, key, &value)),
            }
        }
    }
}

/// `fm rm <key>`: delete a top-level scalar or list field entirely (lines).
pub fn remove_field(data: &[u8], key: &str) -> Result<Vec<u8>, FmError> {
    if find_fm(data).is_none() {
        return Err(FmError::NoFrontmatter);
    }
    let idx = match locate_key_line(data, key) {
        Some(i) => i,
        None => return Err(FmError::Missing),
    };
    // Determine extent: a scalar is 1 line; a list is the key line plus
    // any contiguous block-item lines.
    let lines = lines_of(data);
    let mut end = idx + 1;
    if let Some(fm) = find_fm(data) {
        // If it's a list, extend past contiguous `- …` / blank / comment lines.
        if scalar_value(data, key).is_none() && list_values(data, key).is_some() {
            let mut j = idx + 1;
            while j < lines.len() {
                let l = lines[j].trim_start_matches('\u{feff}').trim().to_string();
                if j < fm.close_line && (l.starts_with('-') || l.is_empty() || l.starts_with('#')) {
                    j += 1;
                    continue;
                }
                break;
            }
            end = j;
        }
    }
    remove_lines(data, idx, end)
}

/// `fm rm <key> <value>`: remove a specific element from the list under
/// `key`. Errors `ValueAbsent` if it isn't there.
pub fn remove_list_item(data: &[u8], key: &str, value: &str) -> Result<Vec<u8>, FmError> {
    if find_fm(data).is_none() {
        return Err(FmError::NoFrontmatter);
    }
    let value = clean_scalar(value);
    let vals = match list_values(data, key) {
        Some(v) => v,
        None => return Err(FmError::IsScalar),
    };
    if !vals.contains(&value) {
        return Err(FmError::ValueAbsent);
    }
    let new_vals: Vec<String> = vals.into_iter().filter(|v| *v != value).collect();
    Ok(list_rewrite_vals(data, key, &new_vals))
}

/// Common splice primitive: drop `lines[start..end)` (global line indices,
/// `end` exclusive) and re-join. Everything outside is preserved exactly.
fn remove_lines(data: &[u8], start: usize, end: usize) -> Result<Vec<u8>, FmError> {
    debug_assert!(start <= end, "remove_lines range");
    // SECURITY_REPORT 3.7: `from_utf8_lossy` so an invalid byte in the body
    // does not swallow the whole buffer (which `map_err(NoFrontmatter)` would
    // do today).
    let s = String::from_utf8_lossy(data);
    let lines: Vec<&str> = s.split_inclusive('\n').collect();
    if start > end || end > lines.len() {
        return Err(FmError::NoFrontmatter);
    }
    let mut out = String::with_capacity(s.len());
    for l in &lines[..start] {
        out.push_str(l);
    }
    for l in &lines[end..] {
        out.push_str(l);
    }
    Ok(out.into_bytes())
}

/// Build a fresh `key: a, b, c` (or `key: v`) line for a *new* field and
/// splice it in just above the closing `---`.
fn list_rewrite_new(data: &[u8], key: &str, value: &str) -> Vec<u8> {
    // SECURITY_REPORT 3.7 — same `from_utf8_lossy` fix.
    let s = String::from_utf8_lossy(data);
    let lines: Vec<String> = s.split_inclusive('\n').map(|l| l.to_string()).collect();
    match find_fm(data) {
        Some(fm) => {
            let mut out: Vec<String> = Vec::with_capacity(lines.len() + 1);
            out.extend_from_slice(&lines[..fm.close_line]); // up to (excl.) the `---`
            out.push(format!("{}: {}\n", key, value));
            out.extend_from_slice(&lines[fm.close_line..]);
            join_lines(&out).into_bytes()
        }
        _ => data.to_vec(), // (unreachable: callers gate on NoFrontmatter)
    }
}

/// Upgrade a scalar field to an inline 2-element list: `key: scal, val`.
fn list_rewrite_upgrade(data: &[u8], key: &str, scalar: &str, added: &str) -> Vec<u8> {
    // SECURITY_REPORT 3.7 — same `from_utf8_lossy` fix.
    let s = String::from_utf8_lossy(data);
    let mut lines: Vec<String> = s.split_inclusive('\n').map(|l| l.to_string()).collect();
    let idx = locate_key_line(data, key)
        .expect("scalar_value found a scalar with this key, so the line must exist");
    let line = lines[idx].clone();
    // Preserve leading indentation (unlikely but keep it faithful).
    let leading = &line[..line.len() - line.trim_start().len()];
    let nl = if line.ends_with("\r\n") { "\r\n" } else if line.ends_with('\n') { "\n" } else { "" };
    lines[idx] = format!("{leading}{}: {scalar}, {added}{nl}", key);
    join_lines(&lines).into_bytes()
}

/// Extend an existing list (block or inline) by appending `added`.
fn list_rewrite_add(data: &[u8], key: &str, added: &str) -> Vec<u8> {
    // SECURITY_REPORT 3.7 — same `from_utf8_lossy` fix.
    let s = String::from_utf8_lossy(data);
    let mut lines: Vec<String> = s.split_inclusive('\n').map(|l| l.to_string()).collect();
    let idx = locate_key_line(data, key)
        .unwrap_or(0); // guarded by caller
    let line = lines[idx].clone();
    let trimmed = line.trim_start_matches('\u{feff}').trim().to_string();
    if trimmed.contains(':') {
        let after_colon = &trimmed[trimmed.find(':').unwrap() + 1..];
        if after_colon.trim().starts_with('[') {
            // Inline list: append the value before the `]`.
            let leading = &line[..line.len() - line.trim_start().len()];
            let nl = if line.ends_with("\r\n") { "\r\n" } else if line.ends_with('\n') { "\n" } else { "" };
            let (key_part, rest) = trimmed.split_once(':').unwrap_or((trimmed.as_str(), ""));
            let inner = rest
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']');
            let mut parts: Vec<String> = inner
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            parts.push(added.to_string());
            let joined = parts.join(", ");
            lines[idx] = format!("{leading}{}: [{joined}]{nl}", key_part.trim());
            return join_lines(&lines).into_bytes();
        }
    }
    // Block list: append a `- added` line right after the last block item.
    let close = find_fm(data).map(|f| f.close_line).unwrap_or(lines.len());
    let (last, _) = last_block_item_line(lines.as_slice(), idx, close).unwrap_or((idx, false));
    let nl = if lines[last].ends_with("\r\n") { "\r\n" } else if lines[last].ends_with('\n') { "\n" } else { "" };
    let mut new_lines: Vec<String> = lines.clone();
    let line_at_last = new_lines[last].clone();
    let leading = &line_at_last[..line_at_last.len() - line_at_last.trim_start().len()];
    let item = format!("{leading}- {}{nl}", added);
    new_lines.insert(last + 1, item);
    join_lines(&new_lines).into_bytes()
}

/// Rewrite a list field in place so its *values* are exactly `vals`. (Used by
/// `remove_list_item`.) Preserves the leading key line style.
fn list_rewrite_vals(data: &[u8], key: &str, vals: &[String]) -> Vec<u8> {
    if vals.is_empty() {
        return remove_field(data, key).unwrap_or_else(|_| data.to_vec());
    }
    let joined = vals.join(", ");
    let mut lines = lines_of(data);
    // Locate the existing key line and the extent of its block items, then
    // collapse the whole field to a single inline `key: <joined>` line.
    match locate_key_line(data, key) {
        Some(idx) => {
            let fm = find_fm(data).map(|f| f.close_line).unwrap_or(lines.len());
            let mut j = idx + 1;
            while j < lines.len() && j < fm {
                let l = lines[j].trim_start_matches('\u{feff}').trim();
                if l.starts_with('-') || l.is_empty() || l.starts_with('#') {
                    j += 1;
                } else {
                    break;
                }
            }
            let line = lines[idx].clone();
            let leading = &line[..line.len() - line.trim_start().len()];
            let nl = if line.ends_with("\r\n") { "\r\n" } else if line.ends_with('\n') { "\n" } else { "" };
            lines.splice(idx..j, [format!("{leading}{key}: [{joined}]{nl}")]);
            join_lines(&lines).into_bytes()
        }
        None => list_rewrite_new(data, key, &joined), // fresh field
    }
}

/// Find the last line in `[idx, close_line)` that is a `- …` block item for
/// the field at `idx`, or `(idx, true)` if the field is an inline-list line.
fn last_block_item_line(lines: &[String], idx: usize, close: usize) -> Option<(usize, bool)> {
    let first = lines.get(idx)?.trim_start();
    if first.starts_with('[') || (first.contains(':') && first.split_once(':').map(|x| x.1).map(|v| v.trim().starts_with('[')).unwrap_or(false)) {
        return Some((idx, true));
    }
    let mut j = idx + 1;
    let mut last_item: Option<usize> = None;
    while j < close && j < lines.len() {
        let l = lines[j].trim_start_matches('\u{feff}').trim().to_string();
        if l.starts_with('-') {
            last_item = Some(j);
        } else if !l.is_empty() && !l.starts_with('#') {
            break;
        }
        j += 1;
    }
    last_item.map(|i| (i, false)).or(Some((idx, false)))
}

/// The real scalar rewrite: replace the value of an existing `key:` line, or
/// insert a fresh one just before the closing `---`.
fn scalar_rewrite(data: &[u8], key: &str, value: &str) -> Result<Vec<u8>, FmError> {
    // SECURITY_REPORT 3.7: `from_utf8_lossy` so a single invalid byte in the
    // body cannot collapse the whole buffer and hide the frontmatter frame
    // (which `map_err(NoFrontmatter)` did for the entire `set_scalar` op).
    let s = String::from_utf8_lossy(data);
    let mut lines: Vec<String> = s.split_inclusive('\n').map(|l| l.to_string()).collect();
    let fm = match find_fm(data) {
        Some(f) => f,
        None => return Err(FmError::NoFrontmatter),
    };
    let idx = match locate_key_line(data, key) {
        Some(i) => i,
        None => {
            // Insert a new scalar field just above the closing `---`.
            let mut out: Vec<String> = Vec::with_capacity(lines.len() + 1);
            out.extend_from_slice(&lines[..fm.close_line]);
            out.push(format!("{key}: {value}\n"));
            out.extend_from_slice(&lines[fm.close_line..]);
            return Ok(join_lines(&out).into_bytes());
        }
    };
    // Replace the value on the existing line, preserving leading indent.
    let line = lines[idx].clone();
    let leading = &line[..line.len() - line.trim_start().len()];
    let trim = line.trim_start_matches('\u{feff}');
    let nl = if line.ends_with("\r\n") { "\r\n" } else if line.ends_with('\n') { "\n" } else { "" };
    // Preserve the `key:` prefix exactly as written.
    let key_part = &trim[..trim.find(':').map(|p| p + 1).unwrap_or(trim.len())];
    lines[idx] = format!("{leading}{key_part} {value}{nl}");
    Ok(join_lines(&lines).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(fm: &str, body: &str) -> Frontmatter {
        parse(&format!("---\n{fm}\n---\n{body}"))
    }

    // ---- framing: no-frontmatter page ----------------------------------
    #[test]
    fn no_frontmatter_returns_none() {
        let p = parse("# A plain file\n\nno frontmatter here");
        assert!(!p.is_indexable());
        assert!(p.page_type.is_none());
        assert_eq!(p.body, "# A plain file\n\nno frontmatter here");
    }

    // ---- type: case-insensitive + unknown → not indexable ---------------
    #[test]
    fn type_case_insensitive_wiki() {
        let p = page("type: WIKI\ntitle: x", "body");
        assert_eq!(p.page_type, Some(NoteType::Wiki));
        assert!(p.is_indexable());
        assert!(p.type_raw.as_deref() == Some("WIKI"));
    }

    #[test]
    fn type_unknown_is_present_but_not_indexable() {
        let p = page("type: blog\ntitle: x", "body");
        assert!(p.page_type.is_none());
        assert!(p.unknown_type_present);
        assert!(!p.is_indexable());
        assert_eq!(p.type_raw.as_deref(), Some("blog"));
    }

    #[test]
    fn type_missing_is_not_indexable_and_not_unknown() {
        let p = page("title: x", "body");
        assert!(p.page_type.is_none());
        assert!(!p.unknown_type_present);
        assert!(!p.is_indexable());
        assert_eq!(p.type_raw, None);
    }

    // ---- tags: trim, unquote, dedupe, drop-empty ------------------------
    #[test]
    fn tags_trim_unquote_dedupe_drop_empty() {
        let p = page(
            "type: note\n\ntags:\n  -  rust  \n  - \"rust\"\n  - ''\n  - http\n  - http\n  -  \n",
            "body",
        );
        assert_eq!(p.tags, vec!["rust".to_string(), "http".to_string()]);
    }

    #[test]
    fn tags_inline_list() {
        let p = page("type: wiki\ntags: [a, b, a, \"c\", 'd', , ]", "body");
        assert_eq!(p.tags, vec!["a", "b", "c", "d"]);
    }

    // ---- search_terms: verbatim, quotes retained -------------------------
    #[test]
    fn search_terms_keep_quotes() {
        let p = page(
            "type: note\nsearch_terms:\n  - mTLS\n  - \"token bucket\"\n",
            "body",
        );
        assert_eq!(
            p.search_terms,
            vec!["mTLS".to_string(), "\"token bucket\"".to_string()]
        );
    }

    /// Gate contract (A.2a): `"token bucket"` matches on the *whole,
    /// unquoted* phrase case-insensitively; `token` alone does not.
    #[test]
    fn gate_token_bucket_phrase() {
        let p = page("type: note\nsearch_terms:\n  - \"token bucket\"\n", "x");
        let entry = p.search_terms[0].trim_matches('"').to_ascii_lowercase();
        assert_eq!(entry, "token bucket");
        assert!(query_unlocks("gateway token bucket", &entry));
        assert!(query_unlocks("gateway TOKEN BUCKET", &entry));
        assert!(!query_unlocks("gateway token", &entry));
    }

    fn query_unlocks(query: &str, entry_unquoted_lower: &str) -> bool {
        query.to_ascii_lowercase().contains(entry_unquoted_lower)
    }

    // ---- description bound: value retained, flag set --------------------
    #[test]
    fn description_over_limit_flagged_not_dropped() {
        let long: String = std::iter::repeat_n('a', 256).collect();
        let p = page(&format!("type: wiki\ndescription: {long}"), "body");
        assert!(p.description.is_some());
        assert_eq!(p.description.as_deref().unwrap().len(), 256);
        assert!(p.description_over_limit());
        // Still indexable — the *drop* is the caller's choice.
        assert!(p.is_indexable());
    }

    #[test]
    fn description_at_limit_ok() {
        let fit: String = std::iter::repeat_n('a', 255).collect();
        let p = page(&format!("type: wiki\ndescription: {fit}"), "body");
        assert!(!p.description_over_limit());
    }

    // ---- frontmatter_text is preserved raw (for FTS) ---------------------
    #[test]
    fn frontmatter_text_preserves_raw() {
        let p = page("type: wiki\ntitle: x\n# a comment", "body");
        assert!(p.frontmatter_text.contains("title: x"));
        assert!(p.frontmatter_text.contains("# a comment"));
    }

    // ---- body is the text after the closing --- -------------------------
    #[test]
    fn body_is_text_after_closing_delim() {
        let p = page("type: wiki", "## Section\n\ncontent here");
        assert_eq!(p.body, "## Section\n\ncontent here");
    }

    // ---- A.3 (label) full frontmatter round-trip --------------------------
    #[test]
    fn a3_label_round_trip() {
        let p = page(
            "type: label\ntitle: LLM\nlanguage: en\ndescription: Private collection of 4 LLM papers/books.\ntags:\n  - llm\n  - attention",
            "body",
        );
        assert_eq!(p.page_type, Some(NoteType::Label));
        assert_eq!(p.title.as_deref(), Some("LLM"));
        assert!(p.description.is_some());
        assert!(p.is_indexable());
        assert_eq!(p.tags, vec!["llm", "attention"]);
    }

    // ---- trailing comment: scalar values survive inline `#` -------------
    #[test]
    fn inline_comment_stripped() {
        let p = page("type: wiki\ntitle: My Title  # the real title", "body");
        assert_eq!(p.title.as_deref(), Some("My Title"));
    }
}

/// Writer-op tests (`fm_tests`). Exercises the byte-level kernel directly,
/// pinning round-trip, `FmError` variants, non-UTF-8 boundaries, BOM
/// tolerance, and body preservation.
#[cfg(test)]
mod fm_tests {
    use super::*;

    fn s(t: &str) -> Vec<u8> { t.as_bytes().to_vec() }
    fn r(v: Vec<u8>) -> String { String::from_utf8(v).unwrap() }

    // ---- find_fm ---------------------------------------------------------
    #[test]
    fn find_fm_no_frontmatter_returns_none() {
        assert!(find_fm(s("# heading\nbody").as_slice()).is_none());
        assert!(find_fm(s("").as_slice()).is_none());
        assert!(find_fm(s("---\n---").as_slice()).is_some());
        assert!(find_fm(s("---\ntitle: x\n---").as_slice()).is_some());
    }

    #[test]
    fn find_fm_bom() {
        let mut b: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
        b.extend_from_slice(b"---\ntitle: x\n---\nbody");
        assert!(find_fm(&b).is_some());
    }

    #[test]
    fn find_fm_partial_dashes_not_frame() {
        assert!(find_fm(s("-\ntitle: x\n-").as_slice()).is_none());
        assert!(find_fm(s("---\ntitle: x\n--").as_slice()).is_none());
    }

    // ---- list_values -----------------------------------------------------
    #[test]
    fn list_values_inline() {
        let d = s("---\ntype: wiki\ntags: [a, b, c]\n---\nbody");
        assert_eq!(list_values(d.as_slice(), "tags"),
            Some(vec!["a".into(), "b".into(), "c".into()]));
    }

    #[test]
    fn list_values_block() {
        let d = s("---\ntype: wiki\ntags:\n  - rust\n  - http\n---\nbody");
        assert_eq!(list_values(d.as_slice(), "tags"),
            Some(vec!["rust".into(), "http".into()]));
    }

    #[test]
    fn list_values_scalar_is_none() {
        let d = s("---\ntype: wiki\ntitle: Hello\n---\nbody");
        assert!(list_values(d.as_slice(), "title").is_none());
    }

    #[test]
    fn list_values_absent_key_none() {
        let d = s("---\ntype: wiki\n---\nbody");
        assert!(list_values(d.as_slice(), "tags").is_none());
    }

    // ---- scalar_value ----------------------------------------------------
    #[test]
    fn scalar_value_present() {
        let d = s("---\ntype: wiki\ntitle: Hello\n---\nbody");
        assert_eq!(scalar_value(d.as_slice(), "title"), Some("Hello".into()));
    }

    #[test]
    fn scalar_value_list_none() {
        let d = s("---\ntype: wiki\ntags: [a, b]\n---\nbody");
        assert!(scalar_value(d.as_slice(), "tags").is_none());
    }

    #[test]
    fn scalar_value_absent_none() {
        let d = s("---\ntype: wiki\n---\nbody");
        assert!(scalar_value(d.as_slice(), "description").is_none());
    }

    // ---- set_scalar ------------------------------------------------------
    #[test]
    fn set_scalar_replaces() {
        let d = s("---\ntype: wiki\ntitle: Old\n---\nbody");
        assert_eq!(r(set_scalar(d.as_slice(), "title", "New").unwrap()),
            "---\ntype: wiki\ntitle: New\n---\nbody");
    }

    #[test]
    fn set_scalar_inserts_missing() {
        let d = s("---\ntype: wiki\n---\nbody");
        assert_eq!(r(set_scalar(d.as_slice(), "description", "desc").unwrap()),
            "---\ntype: wiki\ndescription: desc\n---\nbody");
    }

    #[test]
    fn set_scalar_no_frontmatter_err() {
        assert_eq!(set_scalar(s("# heading").as_slice(), "t", "v"), Err(FmError::NoFrontmatter));
    }

    #[test]
    fn set_scalar_tolerates_non_utf8_body() {
        // SECURITY_REPORT 3.7: a single invalid byte (0x92, CP1252 ') in the
        // *body* used to make `set_scalar` fail with `NoFrontmatter` because
        // the rewrite helpers called strict `from_utf8(...).unwrap_or("")`,
        // which collapsed the whole buffer to "" and hid the frontmatter frame.
        // With `from_utf8_lossy` the frame is found and the scalar rewrite
        // succeeds (the bad byte maps to U+FFFD, body otherwise preserved).
        let mut data = b"---\ntype: wiki\ntitle: Old\n---\nbody with a ".to_vec();
        data.push(0x92);
        data.extend_from_slice(b" here\n");
        let out = set_scalar(data.as_slice(), "title", "New");
        assert!(out.is_ok(), "non-UTF-8 body must not erase the frontmatter frame: {out:?}");
        let out = out.unwrap();
        let title = scalar_value(out.as_slice(), "title").expect("title rewritten");
        assert_eq!(title, "New");
    }

    #[test]
    fn set_scalar_preserves_crlf_terminator() {
        // SECURITY_REPORT 3.11: on a CRLF file, the edited line used to be
        // rebuilt with a bare `\n` (the `ends_with('\n') == "\n"` family),
        // silently dropping the `\r` and leaving one LF-terminated line among
        // CRLF neighbours. The rewritten `title` line must keep its `\r\n`.
        let data = b"---\r\ntype: wiki\r\ntitle: Old\r\n---\r\nbody\r\n";
        let out = set_scalar(data, "title", "New").expect("set_scalar on CRLF file");
        let title_line = out.windows(b"title: New".len()).position(|w| w == b"title: New");
        assert!(title_line.is_some(), "rewritten title line present: {:?}", String::from_utf8_lossy(&out));
        let start = title_line.unwrap();
        let after = &out[start + b"title: New".len()..];
        assert!(
            after.starts_with(b"\r\n"),
            "rewritten line must keep its CRLF terminator: {:?}",
            String::from_utf8_lossy(&out)
        );
        // CRLF still present in the rest of the file (no mixed endings introduced).
        assert!(out.windows(2).any(|w| w == b"\r\n"), "CRLF still present");
    }

    // ---- append_list_item ------------------------------------------------
    #[test]
    fn append_inline() {
        let d = s("---\ntype: wiki\ntags: [a, b]\n---\nbody");
        assert_eq!(r(append_list_item(d.as_slice(), "tags", "c").unwrap()),
            "---\ntype: wiki\ntags: [a, b, c]\n---\nbody");
    }

    #[test]
    fn append_block() {
        let d = s("---\ntype: wiki\ntags:\n  - a\n  - b\n---\nbody");
        let out = r(append_list_item(d.as_slice(), "tags", "c").unwrap());
        assert!(out.contains("- a"));
        assert!(out.contains("- b"));
        assert!(out.contains("- c\n---\nbody"));
    }

    #[test]
    fn append_upgrades_scalar() {
        let d = s("---\ntype: wiki\ntags: a\n---\nbody");
        let out = r(append_list_item(d.as_slice(), "tags", "b").unwrap());
        assert!(out.contains("tags: a, b"));
    }

    #[test]
    fn append_creates_absent() {
        let d = s("---\ntype: wiki\n---\nbody");
        assert_eq!(r(append_list_item(d.as_slice(), "tags", "a").unwrap()),
            "---\ntype: wiki\ntags: a\n---\nbody");
    }

    #[test]
    fn append_no_frontmatter_err() {
        assert_eq!(append_list_item(s("# h\ntags: [a]").as_slice(), "tags", "b"),
            Err(FmError::NoFrontmatter));
    }

    // ---- remove_field ----------------------------------------------------
    #[test]
    fn remove_field_scalar() {
        let d = s("---\ntype: wiki\ntitle: Hello\ntags: [a]\n---\nbody");
        let out = r(remove_field(d.as_slice(), "title").unwrap());
        assert!(!out.contains("title"));
        assert!(out.contains("tags: [a]"));
    }

    #[test]
    fn remove_field_block_list() {
        let d = s("---\ntype: wiki\ntags:\n  - a\n  - b\n---\nbody");
        let out = r(remove_field(d.as_slice(), "tags").unwrap());
        assert!(!out.contains("tags:"));
        assert!(!out.contains("- a"));
        assert!(out.contains("---\nbody"));
    }

    #[test]
    fn remove_field_missing_err() {
        let d = s("---\ntype: wiki\n---\nbody");
        assert_eq!(remove_field(d.as_slice(), "title"), Err(FmError::Missing));
    }

    #[test]
    fn remove_field_no_frontmatter_err() {
        assert_eq!(remove_field(s("no fm").as_slice(), "t"), Err(FmError::NoFrontmatter));
    }

    // ---- remove_list_item ------------------------------------------------
    #[test]
    fn remove_list_item_ok() {
        let d = s("---\ntype: wiki\ntags: [a, b, c]\n---\nbody");
        let out = r(remove_list_item(d.as_slice(), "tags", "b").unwrap());
        assert!(out.contains("a, c"));
        assert!(!out.contains(", b,"));
    }

    #[test]
    fn remove_list_item_block_preserves_newline() {
        // Block list: collapsing must keep the trailing newline (next line
        // must not fuse onto the rewritten key line).
        let d = s("---\ntype: wiki\ntags:\n  - a\n  - b\n---\nbody");
        let out = r(remove_list_item(d.as_slice(), "tags", "a").unwrap());
        assert!(out.contains("tags: [b]"));
        assert!(!out.contains("tags: [b]---"));
    }

    #[test]
    fn remove_list_item_block_followed_by_field() {
        // Block list in the middle of the frontmatter: collapsing must not
        // merge the rewritten line with the *next* field.
        let d = s("---\ntags:\n  - a\n  - b\ndescription: x\n---\nbody");
        let out = r(remove_list_item(d.as_slice(), "tags", "a").unwrap());
        assert!(out.contains("tags: [b]\n"));
        assert!(out.contains("description: x"));
        assert!(!out.contains("[b]description"));
    }

    #[test]
    fn remove_list_item_to_empty_removes_field() {
        let d = s("---\ntype: wiki\ntags: [a]\n---\nbody");
        let out = r(remove_list_item(d.as_slice(), "tags", "a").unwrap());
        assert!(!out.contains("tags:"));
    }

    #[test]
    fn remove_list_item_absent_err() {
        let d = s("---\ntype: wiki\ntags: [a, b]\n---\nbody");
        assert_eq!(remove_list_item(d.as_slice(), "tags", "zzz"), Err(FmError::ValueAbsent));
    }

    #[test]
    fn remove_list_item_on_scalar_err() {
        let d = s("---\ntype: wiki\ntitle: X\n---\nbody");
        assert_eq!(remove_list_item(d.as_slice(), "title", "x"), Err(FmError::IsScalar));
    }

    // ---- non-UTF-8 does not panic ----------------------------------------
    #[test]
    fn non_utf8_find_fm_no_panic() {
        let d: Vec<u8> = vec![b'-', b'-', b'-', b'\n', 0xFF, b'\n', b'-', b'-', b'-', b'\n'];
        let _ = find_fm(d.as_slice());
    }

    #[test]
    fn non_utf8_set_no_panic() {
        let d: Vec<u8> = vec![b'-', b'-', b'-', b'\n', 0xFF, b'\n', b'-', b'-', b'-', b'\n'];
        let _ = set_scalar(d.as_slice(), "title", "x");
        let _ = append_list_item(d.as_slice(), "tags", "a");
        let _ = remove_field(d.as_slice(), "title");
        let _ = remove_list_item(d.as_slice(), "tags", "a");
    }

    // ---- body preservation -----------------------------------------------
    #[test]
    fn body_preserved_through_set() {
        let d = s("---\ntype: wiki\ntags: [a]\n---\n# Body\nline2");
        let out = r(set_scalar(d.as_slice(), "title", "T").unwrap());
        assert!(out.contains("# Body\nline2"));
    }

    #[test]
    fn body_preserved_through_remove() {
        let d = s("---\ntype: wiki\ntags: [a]\n---\n# Body\nline2");
        let out = r(remove_field(d.as_slice(), "tags").unwrap());
        assert!(out.contains("# Body\nline2"));
    }

    // ---- roundtrip join/lines identity -----------------------------------
    #[test]
    fn join_lines_is_inverse_of_lines_of() {
        let d = s("---\ntype: wiki\ntags:\n  - a\n---\n# B\nx");
        let lines = lines_of(d.as_slice());
        assert_eq!(join_lines(lines.as_slice()), String::from_utf8(d).unwrap());
    }
}
