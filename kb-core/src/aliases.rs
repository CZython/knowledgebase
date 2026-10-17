//! aliases — the **export/import serialization shape** of the `tag_aliases`
//! recovery table (M8, the "recover alias knowledge after a DB wipe" pathway).
//!
//! Pure core, no IO. The shell (`kb/src/main.rs`, `--export`/`--import`)
//! reads/writes the file and the DB; *this* owns the byte↔row mapping so the
//! codec is testable, deterministic, and provable in isolation — the same
//! "provable core, assumed shell" split as every other module.
//!
//! ## Why a fixed, boring codec
//!
//! `tag_aliases` is the one piece of *non-derivable* DB state (§2: "the only
//! DB-resident, non-derivable state is `roots` … + `tag_aliases`"). Everything
//! else in the DB rebuilds from the tree; the alias map does not. So its
//! export is a recovery backup, and the format must be:
//!
//! - **deterministic** — the same alias set always renders to the same bytes,
//!   so a diff on the file is meaningful and a round-trip is idempotent;
//! - **self-describing enough to parse** — a human can read it, and a re-import
//!   must reproduce the exact rows;
//! - **total + lossless over the DB's domain** — `alias` is the table primary
//!   key, so aliases are unique and (being tag strings) carry no tabs or
//!   newlines. Within that domain, export→import is an exact inverse.
//!
//! The codec: one row per line, `alias<TAB>canonical`. Lines that are blank or
//! a `#`-comment are *ignored* on import, so a user can annotate the file.
//! A non-blank, non-comment line that has no tab, or an empty field, is
//! *skipped* (reported by the shell) rather than fatal — a partially-typed
//! backup should never lose the rest.
//!
//! ## Two directions, no shared mutable state
//!
//! - `to_lines` / `to_text` — **export**: `Vec<(alias,canonical)>` → text.
//!   Sorts by `(alias, canonical)` so the output is canonical (order-
//!   independent of how the DB returned the rows) and diff-stable.
//! - `parse_line` / `from_text` — **import**: text → `Vec<(alias,canonical)>`.
//!   Last-wins on a repeated alias (matching the `--add` upsert,
//!   `kb/src/main.rs` `ON CONFLICT(alias) DO UPDATE`), first-occurrence order
//!   preserved for the surviving alias set.
//!
//! Verified: `spec/alias_spec.rs` proves the *shape* — a data line
//! decomposes as `alias ++ TAB ++ canonical ++ NL`; the alias is a recoverable
//! prefix and the canonical a recoverable suffix (the parse-recovers-the-fields
//! contract); the per-line import decision is a clean total+disjoint partition
//! (data vs ignorable); and a well-formed alias pair is one whose two fields
//! are both non-empty. The concrete round-trip (export then import == identity
//! over the DB's unique-alias, tab-free domain) is pinned by the unit tests
//! below, the same exec↔spec split as `tags.rs` / `links.rs`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The line-level codec constant: a single ASCII tab separates the two fields.
/// It is a *data* separator, chosen because tag strings never contain one
/// (they are frontmatter tag tokens) and because it renders unambiguously in
/// both the export text and the human-readable `--list` view.
pub const FIELD_SEP: &str = "\t";

/// True if `line` is **ignorable on import** — blank (empty or whitespace-only)
/// or a `#`-comment (first non-whitespace char is `#`). These are legal in the
/// file for annotation and are skipped, never loaded, never reported as errors.
///
/// Total and total-disjoint with `parse_line`'s domain: a line is either
/// ignorable or a candidate data line (which may itself parse to `None`).
#[inline]
pub fn line_is_ignorable(line: &str) -> bool {
    let t = line.trim_start();
    t.is_empty() || t.starts_with('#')
}

/// Parse one **non-ignorable** line into an `(alias, canonical)` pair.
///
/// - splits on the **first** tab (`FIELD_SEP`);
/// - requires exactly the two fields both non-empty after trim;
/// - a line with no tab, or an empty alias, or an empty canonical → `None`
///   (the shell reports it as *skipped*, not fatal — a partial backup must
///   never lose the rest).
///
/// Internal (or trailing) whitespace inside a field is preserved verbatim —
/// the codec does not re-canonicalize tag strings (that is the *query-time*
/// concern, `tags::canonicalize`); it only transmits them.
///
/// Returns `None` (not `Err`): parse failure is a *per-line* skip, and the
/// caller (`from_text`) decides how to tally them.
pub fn parse_line(line: &str) -> Option<(&str, &str)> {
    if line_is_ignorable(line) {
        return None;
    }
    let i = line.find(FIELD_SEP)?;
    let alias = line[..i].trim();
    let canonical = line[i + FIELD_SEP.len()..].trim();
    if alias.is_empty() || canonical.is_empty() {
        return None;
    }
    Some((alias, canonical))
}

/// Export a set of alias rows to a **canonical text** form.
///
/// - sorts by `(alias, canonical)` (stable, total) so the output is
///   independent of the order the DB returned the rows, and thus diff-stable;
/// - renders one `alias<TAB>canonical` per line;
/// - a non-empty set ends with a trailing newline (POSIX text-file
///   convention; an empty set renders to the empty string, not a lone
///   newline).
///
/// Deterministic: `to_text(x) == to_text(y)` whenever `x` and `y` render to
/// the same sorted set — the backup is a function of the *set*, not of its
/// incidental row order.
pub fn to_text(aliases: &[(String, String)]) -> String {
    let mut owned: Vec<(String, String)> = aliases.to_vec();
    owned.sort();
    owned.dedup();
    let mut out = String::new();
    if owned.is_empty() {
        return out; // empty set → empty text (no lone newline)
    }
    for (alias, canonical) in &owned {
        // both fields are user-controlled tag strings; the codec assumes the
        // DB domain (alias is the PK ⇒ unique; tag strings ⇒ no tab/newline),
        // so a naive write is total over the domain we claim to back up.
        let _ = writeln!(out, "{alias}{FIELD_SEP}{canonical}");
    }
    out
}

/// Import canonical text back to an alias set — the **inverse of
/// `to_text`** over the DB's domain (unique aliases, tab-free tag strings).
///
/// Filtering (per line, in order):
/// - ignorable (blank / `#`) → dropped silently;
/// - unparseable (no tab / empty field) → dropped silently here; the shell
///   re-runs `parse_line` itself if it wants to *report* the skips;
/// - data → kept.
///
/// **Last-wins on a repeated alias** (matches the `--add` upsert in
/// `kb/src/main.rs`): if the file lists `a → x` then later `a → y`, the
/// surviving row is `a → y`. First-occurrence *order* of the surviving alias
/// set is preserved (the user's reading order, stable).
///
/// Deterministic in the input text: the same text always yields the same set.
pub fn from_text(text: &str) -> Vec<(String, String)> {
    // SECURITY_REPORT 3.16: `rows.iter_mut().find(…)` per input line was
    // O(R²) for large alias files. A `BTreeMap<alias, row-index>` gives
    // O(log R) last-wins lookups while still preserving first-occurrence
    // order of the surviving rows.
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut idx: BTreeMap<String, usize> = BTreeMap::new();
    for line in text.lines() {
        let Some((alias, canonical)) = parse_line(line) else {
            continue;
        };
        // last-wins: replace an existing alias, else push preserving order.
        match idx.get(alias).copied() {
            Some(i) => rows[i].1 = canonical.to_string(),
            None => {
                idx.insert(alias.to_string(), rows.len());
                rows.push((alias.to_string(), canonical.to_string()));
            }
        }
    }
    rows
}

// =============================================================================
// Unit tests — pin exec `aliases` to the spec shape + the concrete
// round-trip the spec's shape lemmas imply over the DB's domain.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- to_text (export) --------------------------------------------------

    #[test]
    fn export_sorts_and_is_deterministic() {
        let a = vec![
            ("czechia".to_string(), "czech".to_string()),
            ("llm2".to_string(), "llm".to_string()),
        ];
        let b = vec![
            ("llm2".to_string(), "llm".to_string()),
            ("czechia".to_string(), "czech".to_string()),
        ];
        assert_eq!(to_text(&a), to_text(&b), "row order must not matter");
        assert_eq!(to_text(&a), "czechia\tczech\nllm2\tllm\n");
    }

    #[test]
    fn export_dedups_identical_rows() {
        let a = vec![
            ("x".into(), "y".into()),
            ("x".into(), "y".into()),
            ("x".into(), "y".into()),
        ];
        assert_eq!(to_text(&a), "x\ty\n");
    }

    #[test]
    fn export_empty_set_is_empty_string() {
        assert_eq!(to_text(&[]), ""); // no lone trailing newline
    }

    #[test]
    fn export_preserves_internal_case_and_whitespace() {
        // codec transmits tag strings verbatim (trim is parse-side only).
        let a = vec![("Czechia".to_string(), "CZeCh".to_string())];
        assert_eq!(to_text(&a), "Czechia\tCZeCh\n");
    }

    // ---- line_is_ignorable -------------------------------------------------

    #[test]
    fn ignorable_blank_and_comment() {
        assert!(line_is_ignorable(""));
        assert!(line_is_ignorable("   "));
        assert!(line_is_ignorable("# a note"));
        assert!(line_is_ignorable("   # indented comment"));
        assert!(!line_is_ignorable("czechia\tczech"));
    }

    // ---- parse_line --------------------------------------------------------

    #[test]
    fn parse_line_split_on_first_tab() {
        // canonical contains a tab → split on the FIRST tab, keep the rest.
        assert_eq!(parse_line("a\tb\tc"), Some(("a", "b\tc")));
    }

    #[test]
    fn parse_line_trims_fields() {
        assert_eq!(parse_line("  a  \t  b  "), Some(("a", "b")));
    }

    #[test]
    fn parse_line_rejects_empty_field() {
        assert_eq!(parse_line("\tb"), None); // empty alias
        assert_eq!(parse_line("a\t"), None); // empty canonical
        assert_eq!(parse_line("  \t  "), None);
    }

    #[test]
    fn parse_line_rejects_no_tab() {
        assert_eq!(parse_line("justonefield"), None);
    }

    #[test]
    fn parse_line_ignores_ignorable_input() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("# comment"), None);
    }

    // ---- from_text (import) ------------------------------------------------

    #[test]
    fn import_round_trips_the_db_domain() {
        // unique aliases, tab-free tag strings — the `tag_aliases` PK domain.
        let set = vec![
            ("czechia".to_string(), "czech".to_string()),
            ("uk".to_string(), "united-kingdom".to_string()),
            ("llm2".to_string(), "llm".to_string()),
        ];
        let text = to_text(&set);
        // to_text sorts; from_text preserves that order → compare to the set's sorted form.
        assert_eq!(from_text(&text), sorted_copy(&set));
    }

    #[test]
    fn import_skips_ignorable_and_malformed_lines() {
        let text = "# backup\nczechia\tczech\n\nbadline\n\tczech\nllm2\tllm\na\t\n";
        // "badline" (no tab), "\tczech" (empty alias), "a\t" (empty canon) skip.
        assert_eq!(
            from_text(text),
            vec![
                ("czechia".into(), "czech".into()),
                ("llm2".into(), "llm".into())
            ]
        );
    }

    #[test]
    fn import_last_wins_on_repeated_alias() {
        // matches `--add` upsert: earlier definition shadowed by later.
        let text = "a\tx\nb\ty\na\tX2\n";
        assert_eq!(
            from_text(text),
            vec![("a".into(), "X2".into()), ("b".into(), "y".into())]
        );
    }

    #[test]
    fn import_preserves_first_occurrence_order_of_survivors() {
        let text = "z\t1\na\t2\nm\t3\nz\t1b\n"; // z appears first & last
                                                // order: z, a, m  (z stays at its first index; value updated to 1b)
        assert_eq!(
            from_text(text),
            vec![
                ("z".into(), "1b".into()),
                ("a".into(), "2".into()),
                ("m".into(), "3".into())
            ]
        );
    }

    #[test]
    fn import_idempotent_over_export() {
        let set = vec![
            ("czechia".into(), "czech".into()),
            ("uk".into(), "united-kingdom".into()),
        ];
        let text = to_text(&set);
        let once = from_text(&text);
        let twice = from_text(&to_text(&once));
        assert_eq!(once, twice);
    }

    #[test]
    fn import_empty_and_comment_only_is_empty() {
        assert_eq!(from_text(""), Vec::<(String, String)>::new());
        assert_eq!(
            from_text("# only a comment\n\n  #another\n"),
            Vec::<(String, String)>::new()
        );
    }

    fn sorted_copy(v: &[(String, String)]) -> Vec<(String, String)> {
        let mut c: Vec<(String, String)> = v.to_vec();
        c.sort();
        c
    }
}
