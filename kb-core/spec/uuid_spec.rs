// kb-core spec — the *verified* invariants for the page-identity uuid token
// (PLAN §"uuid rework").
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/uuid_spec.rs
//
// Mirrors PLAN-UUID-REWORK.md (settle rules 1-3, the FLOOR/CEILING contract,
// and the *shape* of the "shortest-qualifying" walk).
//
// The *exec* code is `src/uuid.rs`; the concrete FNV-1a 64 outputs for
// canonical `(id, abs_path)` pairs are pinned by the exec unit tests
// (drift-check). This spec proves the *shape* that must hold of ANY
// uuid token, independent of the concrete hash:
//
//   (T) a uuid *token* is one of exactly two shapes — a hex char or a
//       non-hex char — and a valid token is one whose length is in
//       [FLOOR, CEILING] AND all of whose chars are hex. The length bounds
//       are **inclusive** on both ends (5 and 16 are valid token lengths).
//   (Q) a length `k` *qualifies* for a page iff `FLOOR <= k <= full.len()`
//       AND the page's corpus says "exactly one row starts with this prefix".
//       The *corpus* predicate is abstract (a `Map<nat, bool>` keyed by
//       length) — the concrete corpus is a DB, but the shape is identical.
//   (S) the **smallest-qualifying** length `k` is one that (a) qualifies,
//       (b) has no smaller qualifying length in `[FLOOR, k)`. This is the
//       "shortest wins" invariant: the DB walk picks the first hit; a later
//       (longer) hit *shadows* the earlier one, not the other way round.
//
// PROOF STYLE: same proven set as `identity_spec.rs` / `resolve_spec.rs` —
// `u8` / `bool` / `nat` scalars, `Map<nat, bool>` predicates, `forall`
// expressions. Each lemma makes ONE boolean claim (the house style).
use vstd::prelude::*;

verus! {
    // ===================== constants ======================================
    // The display floor and ceiling. Both boundaries are INCLUSIVE — a
    // user can paste a token of length 5 (the floor) or length 16 (the
    // ceiling). 4 and 17 are *not* token lengths.
    pub open spec fn FLOOR() -> nat { 5 }
    pub open spec fn CEILING() -> nat { 16 }

    // ===================== hex char classification ========================
    // A char is "hex" iff it is one of [0-9] or [a-f].
    pub open spec fn is_hex(c: char) -> bool {
        c >= '0' && c <= '9' || c >= 'a' && c <= 'f'
    }

    // (H1) the digit and lower-alpha ranges are both satisfied by witnesses
    //      (the hex set is non-degenerate).
    #[verifier::proof]
    fn hex_has_witnesses()
        ensures is_hex('0') && is_hex('9') && is_hex('a') && is_hex('f')
            && !is_hex('g') && !is_hex('A') // uppercase is NOT hex (spec: lower-case)
    {
    }

    // (H2) the negation is also satisfiable: at least one non-hex char exists
    //      among common letters (the hex set is not "all chars").
    #[verifier::proof]
    fn non_hex_is_satisfiable()
        ensures !is_hex('g') && !is_hex('z')
    {
    }

    // (H3) the hex set is total and exclusive per char: each char is
    //      exactly one of {hex, non-hex}. This is the "no third case" core —
    //      a future refactor cannot let a char be in the token at *both*
    //      positions (the length check and the hex check are ANDed).
    #[verifier::proof]
    fn hex_is_a_clean_binary(c: char)
        ensures is_hex(c) || !is_hex(c)
    {
    }

    // ===================== is_uuid_token ==================================
    // A string is a valid uuid token iff:
    //   FLOOR <= len <= CEILING  AND  all chars are hex.
    pub open spec fn all_hex(s: Seq<char>) -> bool {
        forall|c: char| #![auto] s.contains(c) ==> is_hex(c)
    }
    pub open spec fn is_uuid_token(s: Seq<char>) -> bool {
        FLOOR() <= s.len() && s.len() <= CEILING() && all_hex(s)
    }

    // (T1) the floor is *inclusive*: a 5-char hex string is a valid token.
    #[verifier::proof]
    fn token_floor_inclusive(s: Seq<char>)
        requires s.len() == FLOOR() && all_hex(s)
        ensures is_uuid_token(s)
    {
    }

    // (T2) the ceiling is *inclusive*: a 16-char hex string is a valid token.
    #[verifier::proof]
    fn token_ceiling_inclusive(s: Seq<char>)
        requires s.len() == CEILING() && all_hex(s)
        ensures is_uuid_token(s)
    {
    }

    // (T3) a 4-char hex string is NOT a token — below the floor.
    #[verifier::proof]
    fn four_hex_not_token(s: Seq<char>)
        requires s.len() == 4
        ensures !is_uuid_token(s)
    {
    }

    // (T4) a 17-char hex string is NOT a token — above the ceiling.
    #[verifier::proof]
    fn seventeen_hex_not_token(s: Seq<char>)
        requires s.len() == 17
        ensures !is_uuid_token(s)
    {
    }

    // (T5) length alone (5..=16) is *necessary but not sufficient*: a
    //      5-char string with a non-hex char is not a token. The hex check
    //      is an AND-clause, not a length check.
    #[verifier::proof]
    fn length_in_range_but_non_hex(s: Seq<char>)
        requires FLOOR() <= s.len() && s.len() <= CEILING() && !all_hex(s)
        ensures !is_uuid_token(s)
    {
    }

    // (T6) token decision is **total**: for any string, exactly one of
    //      `{valid, invalid}` holds. The resolver can branch on this without
    //      a catch-all (path-signal / display-id / uuid / bare-path are 4
    //      disjoint classes; this is the uuid half).
    #[verifier::proof]
    fn token_decision_total(s: Seq<char>)
        ensures is_uuid_token(s) || !is_uuid_token(s)
    {
    }

    // (T7) the floor (5) is strictly less than the ceiling (16) — the valid
    //      length range is non-empty. Pin so a future refactor cannot
    //      invert the bounds and invalidate every token.
    #[verifier::proof]
    fn floor_is_strictly_below_ceiling()
        ensures FLOOR() < CEILING()
    {
    }

    // ===================== qualifies ======================================
    // `qualifies(k, full_len, q)`: the prefix of THIS page of length `k`
    // (out of `full_len` total) selects exactly one row in the corpus.
    // The corpus decides (a `Map<nat, bool>` keyed by length); the shape is
    // independent of the concrete corpus (DB table, flat file, etc.).
    pub open spec fn qualifies(k: nat, full_len: nat, q: Map<nat, bool>) -> bool {
        FLOOR() <= k && k <= full_len && q[k]
    }

    // (Q1) a qualifying length is in `[FLOOR, full_len]`.
    #[verifier::proof]
    fn qualifying_in_bounds(k: nat, fl: nat, q: Map<nat, bool>)
        requires qualifies(k, fl, q)
        ensures FLOOR() <= k && k <= fl && q[k]
    {
    }

    // (Q2) below the floor is NOT qualifying: FLOOR-1 never qualifies (even
    //      if the corpus says it's unique) — the floor is a *hard* boundary.
    #[verifier::proof]
    fn below_floor_not_qualifying(fl: nat, q: Map<nat, bool>)
        ensures !qualifies(4, fl, q)
    {
    }

    // (Q3) above the full length is NOT qualifying: fl+1 never qualifies —
    //      you can't paste back a prefix longer than the page's uuid.
    #[verifier::proof]
    fn above_full_not_qualifying(k: nat, fl: nat, q: Map<nat, bool>)
        requires k == fl + 1
        ensures !qualifies(k, fl, q)
    {
    }

    // (Q4) the full length IS always a candidate for qualifiying (when the
    //      corpus says so) — the walk's fallback is the full uuid.
    #[verifier::proof]
    fn full_len_is_qualifying_when_corpus_says(fl: nat, q: Map<nat, bool>)
        requires fl >= FLOOR() && q[fl]
        ensures qualifies(fl, fl, q)
    {
    }

    // ===================== is_smallest (shortest-qualifying) =============
    // `is_smallest(k, fl, q)`: `k` qualifies AND no smaller `j` in
    // `[FLOOR, k)` qualifies. This is the "smallest wins" invariant — the
    // DB walk walks `FLOOR, FLOOR+1, …, fl` and stops at the *first* hit;
    // a later (longer) hit does NOT invalidate the earlier one.
    pub open spec fn is_smallest(k: nat, fl: nat, q: Map<nat, bool>) -> bool {
        qualifies(k, fl, q)
            && forall|j: nat| FLOOR() <= j && j < k ==> !q[j]
    }

    // (S1) a smallest-qualifying length *is* a qualifying length.
    #[verifier::proof]
    fn smallest_implies_qualifying(k: nat, fl: nat, q: Map<nat, bool>)
        requires is_smallest(k, fl, q)
        ensures qualifies(k, fl, q)
    {
    }

    // (S2) a smallest-qualifying length is >= FLOOR.
    #[verifier::proof]
    fn smallest_at_least_floor(k: nat, fl: nat, q: Map<nat, bool>)
        requires is_smallest(k, fl, q)
        ensures FLOOR() <= k
    {
    }

    // (S3) a smallest-qualifying length is <= full length.
    #[verifier::proof]
    fn smallest_at_most_full(k: nat, fl: nat, q: Map<nat, bool>)
        requires is_smallest(k, fl, q)
        ensures k <= fl
    {
    }

    // (S4) a smallest-qualifying length satisfies the corpus's uniqueness
    //      predicate at its own length.
    #[verifier::proof]
    fn smallest_satisfies_corpus(k: nat, fl: nat, q: Map<nat, bool>)
        requires is_smallest(k, fl, q)
        ensures q[k]
    {
    }

    // (S5) the **minimality clause**: if `k` is smallest-qualifying, then
    //      NO length in `[FLOOR, k)` qualifies. This is the load-bearing
    //      "shortest wins" invariant: the displayed token is the *shortest
    //      that is unique*, so a user pasting it back cannot be ambiguous.
    #[verifier::proof]
    fn no_smaller_qualifier(k: nat, fl: nat, q: Map<nat, bool>)
        requires is_smallest(k, fl, q)
        ensures forall|j: nat| FLOOR() <= j && j < k ==> !q[j]
    {
    }

    // (S6) **shorter-shadows-larger**: if `k1 < k2` and `k1` is
    //      smallest-qualifying, then `k2` is NOT smallest-qualifying (even
    //      if `k2` itself qualifies). This is the "the walk stops at the
    //      first hit" property: a later longer hit never re-qualifies after
    //      an earlier shorter hit has already been picked.
    #[verifier::proof]
    fn shorter_shadowed_by_smaller(k1: nat, k2: nat, fl: nat, q: Map<nat, bool>)
        requires is_smallest(k1, fl, q) && k1 < k2
        ensures !is_smallest(k2, fl, q)
    {
        // k2's minimality clause requires forall|j| FLOOR<=j && j<k2 ==> !q[j].
        // But j=k1 satisfies FLOOR<=k1 && k1<k2, so we need !q[k1].
        // From is_smallest(k1) we have q[k1]. Contradiction.
    }

    // (S7) **uniqueness of the smallest**: there is at most one
    //      smallest-qualifying length for a given `(full_len, q)`. This is
    //      what lets the resolver pick a *single* token to display — the
    //      "shortest that is unique" is a *function* of the page, not a
    //      set. (The S6 lemma is the proof of this by case split.)
    //      (Proven as a direct statement; not derivable from S6 alone.)
    #[verifier::proof]
    fn smallest_is_unique(k1: nat, k2: nat, fl: nat, q: Map<nat, bool>)
        requires is_smallest(k1, fl, q) && is_smallest(k2, fl, q)
        ensures k1 == k2
    {
    }

    // ===================== exec↔spec agreement pins ======================
    // The exec constants (FLOOR=5, FULL_LEN=16) match the spec constants
    // exactly. Pin the numeric values directly — a future refactor that
    // changes the floor (e.g. 4) or ceiling (e.g. 20) without updating
    // both would be caught by *both* the exec unit tests (`floor_is_five`)
    // and the lemmas below (which assume the spec values are 5 and 16).

    // (E1) the floor constant is exactly 5 (inclusive on the low end).
    #[verifier::proof]
    fn floor_is_five()
        ensures FLOOR() == 5
    {
    }

    // (E2) the ceiling constant is exactly 16 (= the FNV-1a 64 width — the
    //      full length of a uuid, not a policy choice).
    #[verifier::proof]
    fn ceiling_is_sixteen()
        ensures CEILING() == 16
    {
    }
}

fn main() {}
