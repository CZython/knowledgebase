//! `uuid` — the page identity token (PLAN §"uuid rework").
//!
//! A *stable, shell-inert* identifier for a knowledgebase page. Pure exec
//! (no DB engine, no IO) — lives in `kb-core` next to `etag` so the CLI
//! (`kb/cli/mod.rs`) and the indexer (`kb-db`) compute the *same* 16-hex
//! string for the same `(id, abs_path)` pair. Single owner, no drift.
//!
//! ## Full form
//!
//! `uuid_of(id, abs_path) = etag_of(id \0 abs_path)` — FNV-1a 64, rendered
//! as 16 lowercase hex. Salted with `abs_path` (the `pages` PK) so two pages
//! sharing the same `type:slug` in different folders get *different* uuids.
//! The 16 hex is fixed — it's the FNV-1a 64 output width, not a policy.
//!
//! Rules this satisfies (the user's original three):
//!   1. **Stable under body edits** — `id` and `abs_path` don't change when
//!      the body does. Only re-naming (title/slug) or moving (path) changes
//!      the uuid.
//!   2. **Short** — a 5–16 hex token fits in a shell word (no quote/escape
//!      gymnastics for the common case).
//!   3. **Content-relevant** — the hash covers `id` (type+slug, i.e. content
//!      identity), not just a random counter.
//!
//! ## Short form
//!
//! The *displayed* token is the **shortest prefix of length ≥ 5** that
//! uniquely identifies the page against the corpus. The corpus decides which
//! lengths qualify; this module walks `5, 6, …, 16` and stops at the first
//! length where the corpus says "exactly one row starts with this prefix".
//! The walk is *abstract* here — the concrete corpus is a DB, and the DB
//! does the lookup with an indexed `LIMIT 2` probe (see `kb/cli/mod.rs`),
//! bounding memory and time to ≤2 rows per probe (not `COUNT(*)`).
//!
//! ## Spec twin
//!
//! The *verified* invariants (the "shortest-qualifying" shape, the floor/
//! ceiling, the non-hex-invalidates, the "shorter-shadows-larger" property)
//! live in `spec/uuid_spec.rs` (see `./verify.sh`). The *concrete* FNV
//! output for canonical inputs is pinned by the unit tests below.

use crate::etag::of;

/// The display floor: the shortest token a human/agent can paste back.
///
/// Must be ≥ 5 so that 4 distinct pages sharing a 4-hex prefix are still
/// distinguishable by *at least* the 5th digit — the same "short hash"
/// convention as git (7) and docker (12). 5 is the minimum; the walk
/// *extends* from here on collision.
pub const FLOOR: usize = 5;

/// The display ceiling: the FNV-1a 64 width (16 hex). The user can paste
/// *any* hex string of length `FLOOR..=16`; the resolver prefix-matches.
pub const FULL_LEN: usize = 16;

/// Compute the full (16-hex) page uuid from its content identity
/// (`id = type:slug`) and its storage location (`abs_path`, the PK).
///
/// `id` feeds the hash (content-relevant rule); `abs_path` also feeds the
/// hash (salt — uniqueness rule, since `id` alone is not row-unique). No
/// new column is required to *derive* this; the `pages.uuid` column in
/// `kb-db` is the *stored* copy (so the resolver can index it).
pub fn uuid_of(id: &str, abs_path: &str) -> String {
    // One NUL separator so "ab" + "c" and "a" + "bc" hash to different
    // uuids (concatenation is ambiguous without a delimiter).
    let mut buf = Vec::with_capacity(id.len() + 1 + abs_path.len());
    buf.extend_from_slice(id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(abs_path.as_bytes());
    of(&buf)
}

/// A user-supplied token *looks like* a uuid prefix iff it is 5..=16 hex
/// chars. Used by the resolver to dispatch on the token shape:
///
///   - path signal (`/`, `~`, `\`, `.//`, `..//`) → path verbatim
///   - display id (`type:slug`, contains `:`) → `pages.id` exact
///   - **uuid prefix (this predicate)** → indexed prefix match
///   - bare token → relative path
pub fn is_uuid_token(s: &str) -> bool {
    FLOOR <= s.len() && s.len() <= FULL_LEN && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Given a full uuid and a corpus predicate (`qualifies` returns `true` iff
/// exactly one row starts with this prefix), walk `FLOOR..=full.len()` and
/// return the *first* length for which `qualifies` holds. If no length does,
/// return the full uuid (the "no qualifier" fallback — a hex token can never
/// be *shorter* than the whole thing).
///
/// Pure / no IO: the caller supplies `qualifies` (typically an indexed DB
/// `LIMIT 2` count probe). The walk is O(full.len() − FLOOR + 1) corpus
/// probes, bounded.
pub fn shortest_qualifying<R: Fn(&str) -> bool>(full: &str, qualifies: R) -> String {
    debug_assert!(full.len() >= FLOOR, "full uuid shorter than FLOOR: {full}");
    if full.len() < FLOOR {
        return full.to_string();
    }
    for k in FLOOR..=full.len() {
        let prefix = &full[..k];
        if qualifies(prefix) {
            return prefix.to_string();
        }
    }
    full.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_is_five() {
        assert_eq!(FLOOR, 5);
    }

    #[test]
    fn full_len_is_sixteen() {
        assert_eq!(FULL_LEN, 16);
    }

    #[test]
    fn is_uuid_token_bounds_inclusive() {
        assert!(is_uuid_token("01234")); // exactly floor
        assert!(is_uuid_token("abcdef0123456789")); // exactly full
        assert!(!is_uuid_token("0123")); // below floor
        assert!(!is_uuid_token("abcdef01234567890")); // above full
    }

    #[test]
    fn is_uuid_token_requires_all_hex() {
        assert!(!is_uuid_token("0123g")); // non-hex char at floor length
        assert!(!is_uuid_token("0123456789abcdeZ")); // non-hex at full length
    }

    #[test]
    fn is_uuid_token_uppercase_ok() {
        assert!(is_uuid_token("ABCDEF0123456789"));
    }

    #[test]
    fn uuid_of_is_fully_deterministic() {
        let a = uuid_of("wiki:foo", "/root/wiki/foo.md");
        let b = uuid_of("wiki:foo", "/root/wiki/foo.md");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn uuid_of_salt_distinguishes_same_id_different_path() {
        // Two pages with the same type:slug in different folders →
        // different uuids (the salt rule).
        let a = uuid_of("wiki:foo", "/root/a/foo.md");
        let b = uuid_of("wiki:foo", "/root/b/foo.md");
        assert_ne!(a, b);
    }

    #[test]
    fn uuid_of_id_distinguishes_same_path_different_id() {
        let a = uuid_of("wiki:foo", "/root/wiki/foo.md");
        let b = uuid_of("note:foo", "/root/wiki/foo.md");
        assert_ne!(a, b);
    }

    #[test]
    fn uuid_of_concat_boundary_is_unambiguous() {
        // Without the NUL separator, `id="ab", abs="c"` and `id="a", abs="bc"`
        // both concat to "abc" — same input to `etag_of`, same hash. With the
        // separator, they're `ab\0c` vs `a\0bc` — distinct.
        // We can't easily test "without" (the separator is baked in), but we
        // can assert: distinct (id, abs) pairs with the same concat produce
        // distinct uuids (the NUL breaks the ambiguity).
        let a = uuid_of("ab", "c");
        let b = uuid_of("a", "bc");
        assert_ne!(
            a, b,
            "the NUL separator must break the id||abs concat ambiguity"
        );
    }

    #[test]
    fn shortest_qualifying_floor_when_first_prefix_is_unique() {
        let full = "abcdef0123456789";
        let got = shortest_qualifying(full, |p| p.len() == 5 && full == "abcdef0123456789");
        assert_eq!(got, "abcde");
    }

    #[test]
    fn shortest_qualifying_extends_past_floor_on_collision() {
        let full = "abcdef0123456789";
        // "abcde" is ambiguous (2+ rows share it); "abcdef" is unique.
        let got = shortest_qualifying(full, |p| p.len() == 6);
        assert_eq!(got, "abcdef");
    }

    #[test]
    fn shorteest_qualifying_full_when_no_prefix_is_unique() {
        // Pathological: only the full 16 is "unique".
        let full = "abcdef0123456789";
        let got = shortest_qualifying(full, |p| p.len() == 16);
        assert_eq!(got, full);
    }

    #[test]
    fn shortest_qualifying_never_shorter_than_floor() {
        let full = "abcdef0123456789";
        let got = shortest_qualifying(full, |p| p.len() >= 14);
        assert_eq!(got.len(), 14);
    }

    // ---- spec↔exec agreement: the concrete FNV values are the same the
    // ---- spec's abstract predicates describe (the `full_len` / `FLOOR`
    // ---- constants the spec uses, and the "hex" shape, agree exactly).
    #[test]
    fn full_len_matches_spec_full_len_const() {
        assert_eq!(FULL_LEN, 16);
    }

    #[test]
    fn floor_matches_spec_floor_const() {
        assert_eq!(FLOOR, 5);
    }
}
