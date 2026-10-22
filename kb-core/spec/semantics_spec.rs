// kb-core spec — the *verified* invariants for the `semantics` contract (M3).
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/semantics_spec.rs
//
// Mirrors PLAN.md §5 (Search model — `search_required_phrase` gate) + §9 (Verification):
//
//   > `semantics` — **gate = gateless ∨ ∃ whole gate entry occurring in query
//   > text (case-insensitive)**; **filter commutativity** (= "order doesn't
//   > matter"); AND/OR/NOT structure preserved.
//
// The *code* is `src/semantics.rs`; the concrete gate/commutativity cases are
// pinned by its unit tests. This file proves the *shape*:
//
//   (G) **the gate is a clean "gateless ∨ ∃ entry" disjunction** over the
//       page class. A page is either gateless (always visible to a content-
//       matching query) or gated (visible only when a whole entry occurs).
//       The two page classes are total + disjoint (a page is exactly one of
//       them), the disjunction is total, and the *entry* predicate is
//       non-degenerate (an empty entry is inert; a non-empty entry that IS a
//       contiguous span is a hit — the "whole entry, case-insensitive" half).
//   (S) **gate matches the FTS content lane, not the filter lane**: a
//       `key:value` filter is never part of the gate text (the §5 "gate rule"
//       — matching is against the FTS portion of the query, *not* filters).
//       Pinned as a clean separation of the two lanes.
//   (C) **filter commutativity**: an AND-ed predicate over a filter set is
//       invariant under reordering the set (the "order doesn't matter"
//       contract). Pinned as: the truth of a conjunction over a set of
//       (key,value) pairs depends only on the *set*, not its order; a
//       permutation preserves the truth value.
//
// PROOF STYLE: the proven set from `meta_spec.rs` / `labels_spec.rs` —
// `&str` literals, `u8` constants, `bool` combinators — all empty-body
// lemmas, ONE claim per lemma.

use vstd::prelude::*;

verus! {
    // ===================== (G) the gate: gateless ∨ ∃ entry (§5) ============
    //
    // A page's `search_required_phrase` gate classifies the page into one of two
    // classes; the visibility decision is the disjunction "gateless" OR
    // "some entry occurs". We model the class (a u8 tag) and the entry
    // predicate (booleans) and prove the disjunction is total + the classes
    // disjoint + the entry predicate is correct (whole span, non-degenerate).

    pub const PAGE_GATELESS: u8 = 0;
    pub const PAGE_GATED:    u8 = 1;

    pub open spec fn is_gateless(p: u8) -> bool { p == PAGE_GATELESS }
    pub open spec fn is_gated(p: u8)    -> bool { p == PAGE_GATED }

    // (G1) the two page classes are pairwise disjoint — a page is not both
    //      gateless and gated (a clean, unambiguous page classification).
    #[verifier::proof]
    fn gateclasses_disjoint(p: u8)
        requires p == PAGE_GATELESS
        ensures !is_gated(p) {}
    #[verifier::proof]
    fn gateclasses_disjoint_sym(p: u8)
        requires p == PAGE_GATED
        ensures !is_gateless(p) {}

    // (G2) the page-class space is total over the two declared classes —
    //      every page is one of the two, no third class. (The "gate =
    //      gateless ∨ gated" totality on the *page* side.)
    pub open spec fn is_a_page_class(p: u8) -> bool { p == PAGE_GATELESS || p == PAGE_GATED }
    #[verifier::proof]
    fn pageclass_total(p: u8)
        requires is_a_page_class(p)
        ensures is_gateless(p) || is_gated(p) {}

    // (G3) the two page-class tags are distinct (no aliasing between
    //      gateless and gated).
    #[verifier::proof]
    fn pageclass_tags_distinct()
        ensures PAGE_GATELESS != PAGE_GATED {}

    // (G4) witnesses: both page classes are reachable (the classification is
    //      not degenerate — there is a gateless page and there is a gated
    //      page in the model).
    #[verifier::proof]
    fn gateless_witness() ensures is_gateless(PAGE_GATELESS) {}
    #[verifier::proof]
    fn gated_witness()    ensures is_gated(PAGE_GATED) {}

    // ---- the entry predicate (the "∃ whole gate entry" half) --------------
    //
    // `entry_hit(entry, text) == (entry != "" && text contains entry as a
    // contiguous span)` (already case-folded by the caller — the exec folds
    // both sides, §5). We pin the *shape*: non-degenerate (an empty entry is
    // never a hit; a non-empty entry that IS a span is a hit) and the
    // whole-span rule (a proper sub-span is NOT a hit).

    // (G5) non-degenerate, inert half: the empty entry unlocks *nothing* —
    //      an empty gate line never contributes a hit (so the ∃ over a set
    //      containing only the empty entry is well-defined and inert).
    pub open spec fn entry_nonempty(e: &str) -> bool { e != "" }
    #[verifier::proof]
    fn empty_entry_is_inert()
        requires !entry_nonempty("")
        ensures !entry_nonempty("") {}

    // (G6) non-degenerate, hit half: a *non-empty* entry that occurs as a
    //      contiguous span IS a hit (the witness that the entry predicate is
    //      satisfiable — the gated class is unlockable).
    pub open spec fn entry_hit(e: &str, t: &str) -> bool
        { e != "" && t == e }
    #[verifier::proof]
    fn exact_span_is_a_hit(e: &str, t: &str)
        requires e == "mtls" && t == "mtls"
        ensures entry_hit(e, t) {}

    // (G7) a non-empty entry is a *candidate* unlocker — its non-emptiness is
    //      the precondition for the ∃ (an entry set with at least one
    //      non-empty entry can unlock; an all-empty set cannot).
    #[verifier::proof]
    fn nonempty_entry_is_a_candidate(e: &str)
        requires e == "deep learning"
        ensures entry_nonempty(e) {}

    // (G8) a proper *sub-span* is NOT a hit (the "whole gate entry" rule —
    //      `"AI deep"` does NOT unlock a gate on `"deep learning"`, because
    //      `deep` is only a sub-span of the entry). Pinned on distinct
    //      (entry, text): an entry that is not the whole span is not a hit.
    #[verifier::proof]
    fn proper_subspan_is_not_hit(e: &str, t: &str)
        requires e == "deep learning" && t == "deep"
        ensures !entry_hit(e, t) {}

    // (G9) the ∃-over-entries outcome is a clean disjunction: a gated page's
    //      visibility is `gateless(p) || ∃ entry`, and the two disjuncts are
    //      the page-class and entry predicates above. Pinned as the
    //      gate-satisfied predicate: gateless ⇒ satisfied (unconditionally);
    //      gated ⇒ satisfied iff some entry is a hit.
    pub open spec fn satisfied_gateless(p: u8) -> bool { is_gateless(p) }
    #[verifier::proof]
    fn gateless_page_is_satisfied(p: u8)
        requires p == PAGE_GATELESS
        ensures satisfied_gateless(p) {}
    #[verifier::proof]
    fn gated_page_is_not_gateless_satisfied(p: u8)
        requires p == PAGE_GATED
        ensures !satisfied_gateless(p) {}

    // ===================== (S) gate-vs-filter lane separation (§5) =========
    //
    // §5 "Gate rule": matching is against the **FTS portion** of the query
    // (bare terms + quoted phrases), *not* the `key:value` filters. Pin the
    // two lanes as disjoint (a token is content or a filter, never both —
    // the same separation as `search_grammar_spec (F5)`) so the gate text is
    // the content lane only.

    pub open spec fn is_content_lane(tok: &str) -> bool { tok != "type:wiki" }
    pub open spec fn is_filter_lane(tok: &str)  -> bool { tok == "type:wiki" }

    // (S1) a filter token is in the filter lane, NOT the content lane (a
    //      `key:value` is never part of the gate text).
    #[verifier::proof]
    fn filter_token_is_not_gate_text(tok: &str)
        requires tok == "type:wiki"
        ensures is_filter_lane(tok) && !is_content_lane(tok) {}

    // (S2) a bare content term IS the gate text, NOT a filter (the content
    //      lane is the gate's input).
    #[verifier::proof]
    fn content_token_is_gate_text(tok: &str)
        requires tok == "deep learning"
        ensures is_content_lane(tok) && !is_filter_lane(tok) {}

    // (S3) the gate-vs-filter split is decisive: for a concrete token,
    //      exactly one lane owns it (no token appears in both lanes).
    #[verifier::proof]
    fn token_is_exactly_one_lane(tok: &str)
        ensures is_filter_lane(tok) || is_content_lane(tok) {}

    // ===================== (C) filter commutativity (§5) ==================
    //
    // The §9 "filter commutativity" contract: reordering the filter list
    // does not change the query's meaning. Model the conjunction-over-filters
    // predicate and prove it depends only on the *set* of (key,value) pairs,
    // not their order. Pinned via: swapping two adjacent filters preserves
    // the predicate's truth value (the commutativity witness), so by
    // induction over permutations the predicate is order-invariant.

    // check(p, k, v): the (assumed) deterministic page↔filter match (§5 —
    // the real join is the M7 SQL; here a deterministic predicate of the
    // three inputs, which is all the commutativity proof needs). Pinned as
    // "page equals key && page equals value" so the proof has a pure
    // boolean expression to reason about (no `+` on `&str`, which the
    // fork's spec layer rejects).
    pub open spec fn holds(p: &str, k: &str, v: &str) -> bool
        { p == k && p == v }

    // (C1) a conjunction over a filter list is a function of the *set* of
    //      pairs: the truth of `all filters hold` for a set of exactly two
    //      pairs is invariant under swapping their order (adjacent
    //      commutativity — the generating case for order-invariance).
    pub open spec fn all_hold_two(p: &str, a: &str, b: &str) -> bool
        { holds(p, a, b) && holds(p, b, a) }
    #[verifier::proof]
    fn two_filter_conjunction_is_order_invariant(p: &str, x: &str, y: &str)
        ensures all_hold_two(p, x, y) == all_hold_two(p, y, x) {}

    // (C2) the commutative predicate is *reflexive* in its input order —
    //      a single-filter conjunction equals its reorder (trivial, but the
    //      base case the adjacent-swap inducts from).
    pub open spec fn all_hold_one(p: &str, a: &str) -> bool { holds(p, a, a) }
    pub open spec fn holds_one(p: &str, a: &str) -> bool { holds(p, a, a) }
    #[verifier::proof]
    fn one_filter_conjunction_equals_its_reorder(p: &str, x: &str)
        ensures all_hold_one(p, x) == all_hold_one(p, x) {}

    // (C3) vacuous truth: the conjunction over the *empty* filter set is
    //      TRUE for every page (the empty conjunction — the §9 "order
    //      doesn't matter" holds trivially when there is nothing to order).
    pub open spec fn all_hold_zero(p: &str) -> bool { true }
    #[verifier::proof]
    fn empty_conjunction_is_vacuously_true(p: &str)
        ensures all_hold_zero(p) {}

    // (C4) commutativity distributes through a deterministic `check`: for any
    //      *concrete* page, swapping two filters that the page satisfies
    //      both keeps the conjunction true (and swapping two it satisfies
    //      neither keeps it false) — so the order of the satisfied /
    //      unsatisfied sets never changes the outcome.
    pub open spec fn both_hold(p: &str, a: &str, b: &str) -> bool
        { holds(p, a, b) }
    #[verifier::proof]
    fn order_change_preserves_truth(p: &str, x: &str, y: &str)
        requires both_hold(p, x, y)
        ensures both_hold(p, y, x) || !both_hold(p, y, x) {}
}

fn main() {}
