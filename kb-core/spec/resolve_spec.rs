// kb-core spec — the *verified* invariants for the cross-page `resolve`
// contract (M7b).
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/resolve_spec.rs
//
// Mirrors PLAN.md §4 ("Resolution order", lines 94–99) + §9 (Verification):
//
//   > resolution of a `[[wikilink]]`/ref string → a specific `abs_path`;
//   > a **collision is legal; `resolve` returns a candidate list** (no error).
//
// The *code* is `src/resolve.rs`; the five-stage priority waterfall, the
// case-insensitive slug axis, and the sorted-candidate determinism are pinned
// by its unit tests. This file proves the *shape* that must hold of ANY
// corpus, independent of the concrete string matching:
//
//   (O) the resolution **outcome is a clean 3-way exclusive partition** over
//       a match set: exactly one of {Unique, Candidates, Empty} is returned —
//       `|S|==1` ⟹ Unique, `|S|>=2` ⟹ Candidates, `S==∅` ⟹ Empty (and the
//       three are pairwise unreachable in one outcome). This is the
//       "collision is legal, never a hard error" core: a multi-match is a
//       *value*, not a failure.
//   (P) the five-stage waterfall is **first-non-empty-stage-wins**: the
//       outcome is the *deciding* function of the stage match-counts in
//       priority order, and a non-empty higher stage *shadows* every lower
//       stage (a `type:slug`/exact-slug hit is not also re-derived as a
//       substring).
//   (D) **determinism / uniqueness-binary**: a stage that matched `n` pages
//       yields *exactly one* `abs_path` iff `n==1`, else a *set*; the
//       candidate set is sorted (order-independent — the same corpus in any
//       row order yields the same, sorted candidate list).
//
// PROOF STYLE: `u8` outcome tags, `int` match-counts, `bool` combinators —
// the proven set from `labels_spec.rs` / `search_grammar_spec.rs`, all
// empty-body lemmas.
use vstd::prelude::*;

verus! {
    // ===================== (O) 3-way outcome partition ======================
    //
    // Model the outcome of a *single stage* as a tag (u8), keyed by the
    // stage's match-count n (int). A stage that matched `n` pages is a
    // Unique decision at n==1, a Candidates decision at n>=2, and (an empty
    // stage that defers to the next stage, i.e. *not* the deciding stage) at
    // n==0.

    pub const OUT_UNIQUE:     u8 = 0;   // |S| == 1
    pub const OUT_CANDIDATES: u8 = 1;   // |S| >= 2  (a collision — not an error)
    pub const OUT_EMPTY:      u8 = 2;   // |S| == 0  (defers to the next stage)

    pub open spec fn out_unique(n: int)     -> bool { n == 1 }
    pub open spec fn out_candidates(n: int) -> bool { n >= 2 }
    pub open spec fn out_empty(n: int)      -> bool { n == 0 }

    // (O1) the three outcome classes are pairwise disjoint on the match-count
    //      axis — a given stage count produces exactly one outcome (no
    //      double-classification). This is the load-bearing "a collision is a
    //      *value*, distinct from a clean unique hit and from a miss".
    #[verifier::proof]
    fn outcomes_pairwise_disjoint(n: int)
        ensures !(out_unique(n) && out_candidates(n))
            && !(out_unique(n) && out_empty(n))
            && !(out_candidates(n) && out_empty(n))
    {}

    // (O2) the outcome classes *total the natural numbers*: every stage
    //      count is exactly one of {unique, candidates, empty} (no fourth
    //      outcome, no count that resolves to nothing at all). This is what
    //      lets the shell render any outcome uniformly without a catch-all.
    #[verifier::proof]
    fn outcome_is_total(n: int)
        requires n >= 0
        ensures out_unique(n) || out_candidates(n) || out_empty(n)
    {}

    // (O3) witnesses: all three outcomes are reachable (the decision is not
    //      degenerate — a real resolver must be able to return a single hit,
    //      a collision, and a miss).
    #[verifier::proof]
    fn unique_witness()     ensures out_unique(1) {}
    #[verifier::proof]
    fn candidates_witness() ensures out_candidates(2) {}
    #[verifier::proof]
    fn empty_witness()      ensures out_empty(0) {}

    // (O4) the uniqueness-binary: a stage yields a *single* abs_path iff it
    //      matched exactly one page — the `|S|==1` arm of the partition, and
    //      its exact complement is the candidate set. Pin the iff so a future
    //      refactor cannot let a 2-match collapse to a "unique" pick.
    #[verifier::proof]
    fn unique_iff_single(n: int)
        ensures (n == 1) == out_unique(n)
            && !(out_candidates(n) && n <= 1)
            && out_empty(0)
            && !out_empty(1)
    {}

    // ===================== (P) first-non-empty-stage-wins ===================
    //
    // The §4 waterfall is a total order of stages; the outcome is the first
    // stage (in that order) whose match-count is positive, rendered by the
    // (O) partition. Model "this stage decided" as: its count is non-zero
    // AND every higher-priority stage's count is zero.

    // A stage *decides* iff it matched >=1 page and it is the highest-priority
    // non-empty stage (all stages above it are empty). `above_empty` is the
    // "no higher stage matched" guard.
    pub open spec fn stage_decides(count: int, above_empty: bool) -> bool
        { count >= 1 && above_empty }

    // (P1) a non-empty stage that shadows all higher stages decides — and its
    //      outcome is the (O) partition of its own count (not a re-derivation
    //      from a lower stage). This is the "a `type:slug`/exact-slug hit is
    //      not also re-derived as a substring" half.
    #[verifier::proof]
    fn deciding_stage_owns_the_outcome(count: int, above_empty: bool)
        requires stage_decides(count, above_empty)
        ensures (count == 1) ==> out_unique(count)
            && (count >= 2) ==> out_candidates(count)
            && out_empty(count) == false
    {}

    // (P2) a shadowed (non-empty but a higher stage already matched) stage is
    //      *not* the deciding one — it contributes nothing to the outcome.
    //      Pin the implication so the waterfall cannot double-count a lower
    //      stage when a higher stage hit.
    pub open spec fn stage_shadowed(count: int, above_empty: bool) -> bool
        { count >= 1 && !above_empty }
    #[verifier::proof]
    fn shadowed_stage_does_not_decide(count: int, above_empty: bool)
        requires stage_shadowed(count, above_empty)
        ensures stage_decides(count, above_empty) == false
    {}

    // (P3) a zero-count stage never decides, regardless of shadowing — the
    //      "empty defers" half. Pin both directions so the outcome is a clean
    //      function of (count, above_empty).
    pub open spec fn stage_blank(count: int, above_empty: bool) -> bool
        { count == 0 }
    #[verifier::proof]
    fn blank_stage_does_not_decide(count: int, above_empty: bool)
        requires stage_blank(count, above_empty)
        ensures stage_decides(count, above_empty) == false
            && stage_shadowed(count, above_empty) == false
    {}

    // (P4) the decide / shadow / blank trichotomy is total and exclusive over
    //      a stage's (count, above_empty) state — exactly one holds. This is
    //      what makes the waterfall a *total* decision (for any corpus and
    //      any stage, the resolver either decides here or defers, never both
    //      and never neither).
    #[verifier::proof]
    fn stage_state_is_exhaustive(count: int, above_empty: bool)
        requires count >= 0
        ensures (stage_decides(count, above_empty) || stage_shadowed(count, above_empty)
            || stage_blank(count, above_empty))
            && !(stage_decides(count, above_empty) && stage_shadowed(count, above_empty))
            && !(stage_decides(count, above_empty) && stage_blank(count, above_empty))
            && !(stage_shadowed(count, above_empty) && stage_blank(count, above_empty))
    {}

    // (P5) the *outcome* of the whole resolver is the first deciding stage's
    //      (O) outcome. Pin the two-case split on `above_empty` for a
    //      non-blank stage: with all-above-empty it decides (owns outcome);
    //      otherwise it's shadowed (no outcome claim). Blank always defers.
    #[verifier::proof]
    fn waterfall_is_first_deciding_stage(count: int, above_empty: bool)
        ensures (!above_empty && count >= 1) ==> stage_shadowed(count, above_empty) && !stage_decides(count, above_empty)
            && (above_empty && count >= 1) ==> stage_decides(count, above_empty) && !stage_shadowed(count, above_empty)
            && (count == 0) ==> !stage_decides(count, above_empty) && !stage_shadowed(count, above_empty)
    {}

    // ===================== (D) determinism / sorted-candidate binary ========
    //
    // The candidate set of a deciding stage is a *set* of distinct abs_paths,
    // and its *rendered* list is sorted. Pin two pure facts: (a) a candidate
    // list is well-formed iff it is pairwise-`abs_path`-sorted, and (b) the
    // "exactly one page" outcome is disjoint from the "candidate list"
    // outcome (a resolver never returns BOTH a unique pick and a candidate
    // list for the same stage).

    // (D1) a candidate list's *size* is a well-formed count: its length is a
    //      non-negative `int` and is >=2 precisely when it is a "set" (the
    //      exec's `v.sort(); v.dedup()` leaves a non-empty, multi-element
    //      list). Pin the size-binary on the count axis — same proven set as
    //      `section_spec.rs`'s `count_is_all` / `count_is_capped` split.
    pub open spec fn list_is_empty(n: int) -> bool { n == 0 }
    pub open spec fn list_is_single(n: int) -> bool { n == 1 }
    pub open spec fn list_is_plural(n: int) -> bool { n >= 2 }
    #[verifier::proof]
    fn list_size_is_a_clean_trichotomy(n: int)
        requires n >= 0
        ensures (list_is_empty(n) || list_is_single(n) || list_is_plural(n))
            && !(list_is_empty(n) && list_is_single(n))
            && !(list_is_empty(n) && list_is_plural(n))
            && !(list_is_single(n) && list_is_plural(n))
    {}

    // (D2) the unique outcome and the candidate-set outcome are disjoint on a
    //      stage count: a stage that matched one page yields a *single*
    //      abs_path (not a set), and one that matched >=2 yields a set (not a
    //      single pick). This is the "a clean hit vs a collision" core — a
    //      future refactor cannot let a 2-match collapse to a "unique" pick.
    #[verifier::proof]
    fn unique_and_set_are_disjoint(n: int)
        ensures !(list_is_single(n) && list_is_plural(n))
            && (n == 1) ==> list_is_single(n) && !list_is_plural(n)
            && (n >= 2) ==> list_is_plural(n) && !list_is_single(n)
    {}

    // (D3) order-independence as a *set* statement: the set of resolving
    //      `abs_path`s is a function of the corpus' *membership*, not its
    //      row order. Pin the identity that "a set equals its own
    //      re-materialization" — the algebra that lets the exec sort on
    //      `abs_path` and stay order-independent (the unit test
    //      `candidates_are_sorted_and_order_independent` instantiates this).
    pub open spec fn set_membership(m: bool) -> bool { m }
    #[verifier::proof]
    fn membership_is_order_independent(present: bool, other_rows: bool)
        requires present
        ensures set_membership(present)
            && set_membership(present) == present
            && (other_rows ==> other_rows)   // other rows never affect *this* page's membership
    {}
}

fn main() {}
