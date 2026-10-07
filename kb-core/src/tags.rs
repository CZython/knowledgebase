//! tags — tag-string normalization + query-time alias/case canonicalization (M2).
//!
//! §6 (Tags and Labels) + §9 (Verification):
//!
//! > tag-string collection from frontmatter: **duplicate-free**; trim rule:
//! > leading/trailing whitespace per item removed, **non-empty items only**;
//! > empty `tags:` list → no rows.
//!
//! > Tag matching is **case-insensitive** at query time … stored tag strings
//! > keep their raw casing. **Alias lookup is case-insensitive too.**
//!
//! ## Two distinct operations (do not conflate)
//!
//! 1. **Store-time normalization** (`normalize`) — applied once at index
//!    time to the raw `tags:` list before it hits `page_tags`. Trim, drop
//!    empties, dedupe (first occurrence). The *stored* strings keep their
//!    written casing (a tag `Czechia` is stored as `Czechia`, not
//!    lower-cased) — the DB has no `lower()` index, so matching is handled
//!    at query time.
//!
//! 2. **Query-time canonicalization** (`canonicalize`) — applied to *query*
//!    tags (and to stored tags when building the match set) so that
//!    "Czech"/"czech"/"czechia" all land on the canonical `czech`. Case-
//!    insensitive, alias-resolved. The exec's `normalize` deliberately does
//!    NOT lower-case or alias — those are *query* concerns, and folding them
//!    into storage would lose the raw spelling the user wrote.
//!
//! Verified: `spec/tags_spec.rs` proves the shape (normalize is idempotent,
//! duplicate-free, empty-drop is a total/disjoint partition of the input;
//! canonicalize is idempotent and conflates case + alias).
//!
//! The concrete boundary cases (whitespace-only tags, mixed-case alias hits,
//! empty list → `Vec::new()`) are pinned by the unit tests below.

use std::collections::{BTreeMap, BTreeSet};

/// Store-time normalization (§9 `tags`):
/// - trim leading/trailing whitespace on each item
/// - drop empty / whitespace-only items
/// - dedupe, **first-occurrence** order preserved
/// - keep the item's original internal case (no lowercasing — §6 "stored
///   tag strings keep their raw casing")
///
/// An empty (or all-empty) input yields an empty `Vec` — the "empty `tags:`
/// list → no rows" contract (§9).
///
/// Idempotent: `normalize(normalize(x)) == normalize(x)`.
pub fn normalize(tags: &[String]) -> Vec<String> {
    // SECURITY_REPORT 3.16: use a `BTreeSet<String>` for "seen" instead of a
    // `Vec<String>` with `seen.iter().any(…)` — the old per-element linear
    // scan is O(n²) for large `tags:` lists, and the `BTreeSet` gives O(n log n).
    // The emitted `Vec<String>` still preserves first-occurrence order: we
    // test the membership in the set, and only push to `out` when the
    // element is new.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let trimmed = t.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Dedupe on the *trimmed* value (so `" a"` and `" a "` collapse),
        // keeping first-occurrence order.
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// Build the *match set* for a single query tag: the query tag itself plus
/// all tags that alias-resolve to the same canonical, lower-cased on each
/// side so that "Czech" matches a stored "czech" and via any alias row.
///
/// # Args
///
/// - `tag` — the query-side tag (as the user typed it, any case).
/// - `stored` — the corpus's stored tag strings (raw casing, from
///   `page_tags`). We return the subset that *match* `tag`.
/// - `aliases` — user-managed `alias → canonical` rows (raw casing as
///   written). Matching is case-insensitive on all three.
///
/// # Returns
///
/// The (deduped, first-occurrence) subset of `stored` that matches `tag`
/// either directly (case-insensitive) or through a shared canonical.
///
/// Example: `stored=["Czech","czechia","llm"]`, `tag="czech"`,
/// `aliases=[("Czechia","czech")]` → `["Czech","czechia"]`.
pub fn canonicalize<'a>(
    tag: &str,
    stored: &'a [String],
    aliases: &[(String, String)],
) -> Vec<&'a str> {
    let want = tag.trim().to_lowercase();
    if want.is_empty() {
        return Vec::new();
    }
    // SECURITY_REPORT 3.16: `canonical_of` used to re-scan the alias table
    // (O(A)) for *every* stored tag (O(T·A) total). Pre-build a
    // `BTreeMap<key, canonical>` once (O(A log A)) so each lookup is
    // O(log A). The key/value are both the trimmed, lower-cased forms that
    // the old linear scan compared against.
    let canon: BTreeMap<String, String> = aliases
        .iter()
        .map(|(a, c)| (a.trim().to_lowercase(), c.trim().to_lowercase()))
        .collect();
    let canonical_of = |s: &str| -> String {
        let sk = s.trim().to_lowercase();
        canon.get(&sk).cloned().unwrap_or(sk)
    };
    let target = canonical_of(&want);

    let mut out: Vec<&'a str> = Vec::new();
    // SECURITY_REPORT 3.16: membership set for the output dedupe (was a
    // `Vec<String>` + per-element `any(…)`).
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for s in stored {
        let sk = s.trim().to_lowercase();
        if sk.is_empty() {
            continue;
        }
        let sc = canonical_of(&sk);
        // match iff the canonical of the stored tag EQUALS the target
        // canonical (this conflates direct, case-fold, and alias paths).
        if sc == target {
            let key = s.as_str();
            if seen.insert(key.to_string()) {
                out.push(key);
            }
        }
    }
    out
}

/// True if `alias` (case-insensitive) maps to `canonical` (case-insensitive)
/// in the user-managed alias map. A pure predicate mirroring the DB
/// `tag_aliases` table; no IO.
#[inline]
pub fn has_alias(alias: &str, canonical: &str, aliases: &[(String, String)]) -> bool {
    let a = alias.trim().to_lowercase();
    let c = canonical.trim().to_lowercase();
    if a.is_empty() || c.is_empty() {
        return false;
    }
    aliases.iter().any(|(al, ca)| {
        al.trim().to_lowercase() == a && ca.trim().to_lowercase() == c
    })
}

// =============================================================================
// Unit tests — pin exec `tags` to the spec shape.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> String { x.to_string() }
    fn t<T: ToString>(x: T) -> String { x.to_string() }

    // ---- normalize (§9): trim / drop-empty / dedupe / first-occurrence ---

    #[test]
    fn normalize_trims_and_drops_empty() {
        let input = vec![s(" a"), s(""), s("   "), s("b"), s(" ")];
        assert_eq!(normalize(&input), vec!["a", "b"]);
    }

    #[test]
    fn normalize_dedupe_first_occurrence() {
        let input = vec![s("llm"), s("llm"), s("LLM")]; // "LLM" case differs → kept
        assert_eq!(normalize(&input), vec!["llm", "LLM"]);
    }

    #[test]
    fn normalize_dedupe_on_trimmed_value() {
        // " a" and " a " both normalize to "a" → one row.
        let input = vec![s(" a"), s(" a "), s("a")];
        assert_eq!(normalize(&input), vec!["a"]);
    }

    #[test]
    fn normalize_preserves_internal_case() {
        let input = vec![s("CamelCase"), s("snake_case")];
        assert_eq!(normalize(&input), vec!["CamelCase", "snake_case"]);
    }

    #[test]
    fn normalize_empty_and_all_empty_yield_empty() {
        assert_eq!(normalize(&[]), Vec::<String>::new());
        assert_eq!(normalize(&[s(""), s("   ")]), Vec::<String>::new());
    }

    #[test]
    fn normalize_idempotent() {
        let input = vec![s(" a"), s("b"), s(" a"), s("")];
        let once = normalize(&input);
        let twice = normalize(&once);
        assert_eq!(once, twice);
    }

    // ---- canonicalize (§6): case-fold + alias conflate -------------------

    #[test]
    fn canonicalize_case_fold_direct_match() {
        let stored = vec![s("czech"), s("Czech"), s("llm")];
        let hit = canonicalize("CZECH", &stored, &[]);
        assert_eq!(hit, vec!["czech", "Czech"]);
    }

    #[test]
    fn canonicalize_alias_conflates() {
        // "czechia" aliases to "czech" → querying czech finds both.
        let stored = vec![s("czech"), s("czechia"), s("llm")];
        let aliases = vec![(t("Czechia"), t("czech"))];
        let hit = canonicalize("czech", &stored, &aliases);
        assert_eq!(hit, vec!["czech", "czechia"]);
    }

    #[test]
    fn canonicalize_case_insensitive_alias_lookup() {
        // alias row written "Czechia"; query tag lower, alias stored upper;
        // all case-insensitive so it still conflates.
        let stored = vec![s("Czechia"), s("llm")];
        let aliases = vec![(t("czechia"), t("CZECH"))];
        let hit = canonicalize("czech", &stored, &aliases);
        assert_eq!(hit, vec!["Czechia"]);
    }

    #[test]
    fn canonicalize_no_match_is_empty() {
        let stored = vec![s("czech"), s("llm")];
        assert_eq!(canonicalize("rust", &stored, &[]), Vec::<&str>::new());
    }

    #[test]
    fn canonicalize_empty_query_is_empty() {
        let stored = vec![s("czech")];
        assert_eq!(canonicalize("", &stored, &[]), Vec::<&str>::new());
        assert_eq!(canonicalize("   ", &stored, &[]), Vec::<&str>::new());
    }

    // ---- has_alias --------------------------------------------------------

    #[test]
    fn has_alias_case_insensitive_both_ways() {
        let aliases = vec![(t("Czechia"), t("czech"))];
        assert!(has_alias("czechia", "CZECH", &aliases));
        assert!(has_alias("CZECHIA", "czech", &aliases));
        assert!(!has_alias("czech", "czechia", &aliases));
    }

    #[test]
    fn has_alias_empty_sides_are_false() {
        let aliases = vec![(t("Czechia"), t("czech"))];
        assert!(!has_alias("", "czech", &aliases));
        assert!(!has_alias("czechia", "", &aliases));
    }
}
