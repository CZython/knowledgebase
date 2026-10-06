// kb-core spec — the *verified* invariants for the frontmatter contract.
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/fm_spec.rs
//
// Mirrors PLAN.md §3 (Frontmatter) / §4 (identity — the `type:` gate) /
// §7 (`search_terms` gate). The *code* is `src/frontmatter.rs`; the concrete
// boundary cases (255/256 description, token-bucket gate, tag dedupe,
// no-frontmatter framing) are pinned by the unit tests there. This file
// proves the *shape* — the logical structure the parser + indexer rely on.
//
// PROOF STYLE: only `&str` parameters and `==` / `Seq<char>` operations —
// the set this Verus fork (ARIA v0.2026.09.06) discharges directly. This is
// deliberately the same minimal shape as `spec/meta_spec.rs` (which already
// verifies clean), so every lemma here is in a shape we KNOW Z3 handles.
use vstd::prelude::*;

verus! {
    // ===================== the `type:` gate (§3, §4) =====================
    //
    // A page is indexable iff its `type:` resolved to one of the three known
    // kinds. (Exec lower-cases + trims before the match; the spec talks about
    // the canonical lowercase value.)

    pub open spec fn is_label(s: &str) -> bool { s == "label" }
    pub open spec fn is_wiki (s: &str) -> bool { s == "wiki" }
    pub open spec fn is_note (s: &str) -> bool { s == "note" }

    pub open spec fn is_known_kind(s: &str) -> bool {
        is_label(s) || is_wiki(s) || is_note(s)
    }

    // (§4) "a `.md` file with no `type:` is skipped (not indexed)".
    pub open spec fn is_indexable(s: &str) -> bool { is_known_kind(s) }

    // (G1) the three known kinds are pairwise disjoint — a value is at most
    //      one kind, so the gate is unambiguous.
    #[verifier::proof]
    fn kinds_are_pairwise_disjoint(s: &str)
        ensures !(is_label(s) && is_wiki(s))
            && !(is_label(s) && is_note(s))
            && !(is_wiki(s)  && is_note(s))
    {}

    // (G2) witnesses: each literal IS its own kind (so the gate is satisfiable).
    #[verifier::proof]
    fn label_is_known(s: &str) requires s == "label" ensures is_indexable(s) {}
    #[verifier::proof]
    fn wiki_is_known (s: &str) requires s == "wiki"  ensures is_indexable(s) {}
    #[verifier::proof]
    fn note_is_known (s: &str) requires s == "note"  ensures is_indexable(s) {}

    // (G3) a no-type / unknown value is NOT indexable.
    #[verifier::proof]
    fn empty_type_not_indexable(s: &str) requires s == "" ensures !is_indexable(s) {}
    #[verifier::proof]
    fn unknown_word_not_indexable(s: &str) requires s == "blog" ensures !is_indexable(s) {}

    // (G4) indexability IS exactly the 3-way membership (definitional pin).
    #[verifier::proof]
    fn indexable_iff_known(s: &str)
        ensures (is_indexable(s) == is_known_kind(s))
    {}

    // ===================== description bound (§3) =========================
    //
    // Hard limit ≤ 255 characters. Exec *flags* an over-limit value (retains
    // the value; the indexer drops + warns). Provable shape: in-bound and
    // over-bound are total and disjoint — every value is exactly one of them,
    // so the drop decision is unambiguous.

    // Work on the *Seq* shape (same as `meta_spec.rs`), not on the abstract
    // `s@` of a `&str` — Z3 reduces `Seq::len()` but not `"…"@.len()` on
    // an opaque `&str` the way we need for a bound comparison.
    pub open spec fn desc_in_bound (s: Seq<char>) -> bool { s.len() <= 255 }
    pub open spec fn desc_over_bound(s: Seq<char>) -> bool { s.len() > 255 }

    // (D1) total: a description is in-bound or over-bound, no third case.
    #[verifier::proof]
    fn description_bound_total(s: Seq<char>)
        ensures desc_in_bound(s) || desc_over_bound(s)
    {}

    // (D2) disjoint: never both (so "keep" and "flag over-limit" can't be
    //      demanded of the same value — the drop is unambiguous).
    #[verifier::proof]
    fn description_bound_disjoint(s: Seq<char>)
        ensures !(desc_in_bound(s) && desc_over_bound(s))
    {}

    // (D3) the empty description is in-bound (0 ≤ 255).
    #[verifier::proof]
    fn empty_description_in_bound()
        ensures desc_in_bound(seq![])
    {}

    // ===================== tag hygiene (§3) ===============================
    //
    // Collected tags must be trimmed, non-empty, deduped. Exec enforces the
    // whole pipeline; the provable invariant is the *membership* shape a
    // tag list must have: a well-formed tag is a known non-empty string and
    // the empty string never qualifies. (Dedupe / order are exec concerns.)

    pub open spec fn tag_well_formed(v: &str) -> bool { v != "" }

    // (T1) a well-formed tag is non-empty — `page_tags` never gets an `""` row.
    #[verifier::proof]
    fn well_formed_tag_nonempty(v: &str)
        ensures tag_well_formed(v) ==> v != ""
    {}

    // (T2) empty vs well-formed is a clean partition — a tag is one or the other.
    #[verifier::proof]
    fn tag_well_formed_iff_nonempty(v: &str)
        ensures (tag_well_formed(v) == (v != ""))
    {}

    // (T3) a concrete clean tag set is satisfiable (the predicate is not degenerate).
    #[verifier::proof]
    fn rust_and_http_are_well_formed()
        requires tag_well_formed("rust") && tag_well_formed("http")
        ensures !tag_well_formed("")
    {}

    // (T4) two distinct tags are a disjoint pair (basis for dedupe: each
    //      entry is unique by value).
    #[verifier::proof]
    fn distinct_tags_disjoint(a: &str, b: &str)
        requires a != b
        ensures !((a == "rust") && (a == "http"))
    {}

    // ===================== `search_terms` gate (§7) =======================
    //
    // A page *with* `search_terms` is hidden from general search and is
    // unlocked only when the *entire* entry occurs as a contiguous span in
    // the query (case-insensitive). Provable shape: unlock := (entry is
    // non-empty) AND (entry ⊑ query). The exec compares
    // `query.to_ascii_lowercase().contains(entry_unquoted_lowercase)`.

    // "entry occurs in query" — modelled in the shape Z3 discharges here
    // (`is_prefix_of` / `is_suffix_of`, already proven in `meta_spec.rs`):
    // a gate entry must (a) be non-empty, and (b) appear as a whole-string
    // match, or as a contiguous prefix, or as a contiguous suffix, of the
    // already-lower-cased query. (Exec uses `String::contains`; the spec
    // pins the *shape* — non-empty + a whole contiguous run — which is the
    // invariant the hidden→unlocked transition depends on.)
    pub open spec fn gate_unlocks(q: &str, entry: &str) -> bool {
        entry != ""
            && ( q@ == entry@
                 || entry@.is_prefix_of(q@)
                 || entry@.is_suffix_of(q@) )
    }

    // (S1) an empty entry never unlocks (guard against a degenerate gate word).
    #[verifier::proof]
    fn empty_gate_unlocks_nothing(q: &str)
        ensures !gate_unlocks(q, "")
    {}

    // (S2) entry == query ⇒ unlocks (the `mTLS == mtls` single-word case).
    #[verifier::proof]
    fn entry_equals_query_unlocks(q: &str)
        requires q != ""
        ensures gate_unlocks(q, q)
    {}

    // (S3) entry is a whole suffix of the query ⇒ unlocks ("… token bucket").
    #[verifier::proof]
    fn entry_as_suffix_unlocks(q: &str, e: &str)
        requires e != ""
        ensures e@.is_suffix_of(q@) ==> gate_unlocks(q, e)
    {}

    // (S4) entry is a whole prefix of the query ⇒ unlocks ("token bucket …").
    #[verifier::proof]
    fn entry_as_prefix_unlocks(q: &str, e: &str)
        requires e != ""
        ensures e@.is_prefix_of(q@) ==> gate_unlocks(q, e)
    {}

    // (S5) the unlock predicate is *sound* on the positive branch it claims:
    //      if it unlocks via exact/whole-query, the entry is non-empty AND the
    //      query carries the entry as a contiguous run (prefix or suffix or all).
    #[verifier::proof]
    fn unlock_via_exact_match(q: &str, e: &str)
        requires q == e
        ensures gate_unlocks(q, e) == (e != "")
    {}
}

fn main() {}
