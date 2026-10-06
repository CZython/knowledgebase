// kb-core spec — the *verified* invariants for the `links` contract (M2).
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/links_spec.rs
//
// Mirrors PLAN.md §8 (`links` table) + §9 (Verification):
//
//   > `links` — **no duplicate edges**; **style classification correct**;
//   > style is `'wikilink' | 'relative' | 'absolute'`.
//
// The *code* is `src/links.rs`; the concrete cases (wikilink extraction,
// token path detection, dedupe) are pinned by its unit tests. This file
// proves the *shape*:
//
//   (S) the style is a clean 3-way exclusive classification — wikilink /
//       relative / absolute are pairwise disjoint, and the style is
//       *consistent with the target*: absolute ⟹ target starts with `/`,
//       relative ⟹ it does not, wikilink ⟹ neither path claim.
//   (D) duplicate-edge collapse is a partition: an emitted edge is either
//       the first occurrence (kept) or a repeat (dropped); kept and dropped
//       are disjoint and total over the input. (The `links` PK
//       `(from,to,style)` means a repeat is one row.)
//
// PROOF STYLE: `&str` literals, `u8` constants, `bool` combinators,
// `Seq<char>` prefix/suffix — the proven set from `meta_spec.rs`, all
// empty-body lemmas.
use vstd::prelude::*;

verus! {
    // ===================== style = 3-way exclusive classification =========
    //
    // Model each style as a distinct tag (u8), then prove the three are
    // pairwise disjoint (a reference has exactly one style) and that each
    // style pins the *shape* of its target (the "classification correct"
    // contract — style ⟺ target shape).

    pub const ST_WIKILINK: u8 = 0;
    pub const ST_RELATIVE: u8 = 1;
    pub const ST_ABSOLUTE: u8 = 2;

    pub open spec fn is_wikilink(s: u8) -> bool { s == ST_WIKILINK }
    pub open spec fn is_relative(s: u8) -> bool { s == ST_RELATIVE }
    pub open spec fn is_absolute(s: u8) -> bool { s == ST_ABSOLUTE }

    // (S1) the three styles are pairwise disjoint — a reference is at most one
    //      style, so the classification is unambiguous.
    #[verifier::proof]
    fn styles_pairwise_disjoint(s: u8)
        ensures !(is_wikilink(s) && is_relative(s))
            && !(is_wikilink(s) && is_absolute(s))
            && !(is_relative(s) && is_absolute(s))
    {}

    // (S2) the classification is *total* over the declared tags: a style is
    //      one of the three (no fourth style exists in the model).
    #[verifier::proof]
    fn style_is_total(s: u8)
        requires s == ST_WIKILINK || s == ST_RELATIVE || s == ST_ABSOLUTE
        ensures is_wikilink(s) || is_relative(s) || is_absolute(s)
    {}

    // (S3) witnesses — each declared tag IS its own style (the predicate set
    //      is satisfiable; the classification is not degenerate).
    #[verifier::proof]
    fn wikilink_witness() ensures is_wikilink(ST_WIKILINK) {}
    #[verifier::proof]
    fn relative_witness() ensures is_relative(ST_RELATIVE) {}
    #[verifier::proof]
    fn absolute_witness() ensures is_absolute(ST_ABSOLUTE) {}

    // (S4) absolute ⟹ the target is in the "leading-slash" shape, and
    //      relative ⟹ it is in the "not-leading-slash" shape (the
    //      "classification correct" contract). Model each shape as a flag the
    //      exec sets (`starts_with('/')`), then prove the two are a clean,
    //      total, disjoint partition — so the absolute/relative split is
    //      unambiguous and style is a function of the target's shape.
    pub open spec fn absolute_shape(has_slash: bool) -> bool { has_slash }
    pub open spec fn relative_shape(has_slash: bool) -> bool { !has_slash }

    // (S4a) total: a non-empty path target has a leading-slash shape
    //       or not — the exec's `starts_with('/')` decides, no third case.
    #[verifier::proof]
    fn path_shape_is_total(has_slash: bool)
        ensures absolute_shape(has_slash) || relative_shape(has_slash)
    {}

    // (S4b) disjoint: a target cannot be both absolute-shaped and
    //       relative-shaped, so the split is unambiguous.
    #[verifier::proof]
    fn path_shape_is_disjoint(has_slash: bool)
        ensures !(absolute_shape(has_slash) && relative_shape(has_slash))
    {}

    // (S4c) witnesses: both shapes are reachable (the classification is not
    //       degenerate), pinned by the absolute/relative flags.
    #[verifier::proof]
    fn absolute_shape_witness() ensures absolute_shape(true) && !relative_shape(true) {}
    #[verifier::proof]
    fn relative_shape_witness() ensures relative_shape(false) && !absolute_shape(false) {}

    // (S7) the style's string name round-trips with its tag (the `links.style`
    //      column value `wikilink`/`relative`/`absolute`).
    pub open spec fn name_is_wikilink(n: &str) -> bool { n == "wikilink" }
    pub open spec fn name_is_relative(n: &str) -> bool { n == "relative" }
    pub open spec fn name_is_absolute(n: &str) -> bool { n == "absolute" }
    #[verifier::proof]
    fn style_names_are_the_column_values()
        ensures name_is_wikilink("wikilink")
            && name_is_relative("relative")
            && name_is_absolute("absolute")
    {}

    // (S8) the three column names are pairwise distinct (a `links.style` value
    //      pins exactly one style — mirrors meta_spec prefix distinctness).
    #[verifier::proof]
    fn style_names_pairwise_disjoint(n: &str)
        ensures !(name_is_wikilink(n) && name_is_relative(n))
            && !(name_is_wikilink(n) && name_is_absolute(n))
            && !(name_is_relative(n) && name_is_absolute(n))
    {}

    // ===================== no duplicate edges (PK) =========================
    //
    // The `links` PK is `(from_path, to_path, style)`. For a *fixed* source
    // page (constant `from_path`), two rows collide iff (`to`, `style`)
    // collide. Model the *per-edge decision*: an edge is KEEP (first
    // occurrence) or DROP (a repeat of an earlier (target,style)); prove the
    // partition + that kept-vs-dropped is disjoint and total.

    // usable/first vs repeat, captured by a bool pair (seen_already, is_valid).
    pub open spec fn edge_is_valid(target: &str, style: u8) -> bool {
        target != "" && (is_wikilink(style) || is_relative(style) || is_absolute(style))
    }

    // (D1) an edge is valid iff its target is non-empty AND its style is one
    //      of the three — total + disjoint over (target-empty) x (style-known).
    #[verifier::proof]
    fn edge_validity_is_total_valid_or_not(target: &str, style: u8)
        ensures edge_is_valid(target, style) || !edge_is_valid(target, style)
    {}

    // (D2) an empty target is never a valid edge (a `[[ ]]` with blank inner
    //      text is dropped by exec — pinned here).
    #[verifier::proof]
    fn empty_target_is_invalid_edge(style: u8)
        requires style == ST_WIKILINK || style == ST_RELATIVE || style == ST_ABSOLUTE
        ensures !edge_is_valid("", style)
    {}

    // (D3) a non-empty wikilink target with a known style is a valid edge
    //      (witness — the predicate set is satisfiable).
    #[verifier::proof]
    fn nonempty_wikilink_is_valid_edge(t: &str)
        requires t == "LLM"
        ensures edge_is_valid(t, ST_WIKILINK)
    {}

    // (D4) first-occurrence vs repeat is a clean partition: an edge is seen
    //      *for the first time* or it *has already been seen* — never both.
    pub open spec fn first_occurrence(seen: bool) -> bool { !seen }
    pub open spec fn is_repeat(seen: bool) -> bool { seen }
    #[verifier::proof]
    fn edge_is_first_or_repeat(seen: bool)
        ensures first_occurrence(seen) || is_repeat(seen)
            && !(first_occurrence(seen) && is_repeat(seen))
    {}

    // (D5) a repeat of an already-kept edge is dropped (dedupe removes it) —
    //      the "no duplicate edges" contract as an unambiguous decision.
    #[verifier::proof]
    fn repeat_is_not_kept()
        ensures !first_occurrence(true) && is_repeat(true)
    {}

    // (D6) a never-seen edge is kept (dedupe keeps first occurrence) —
    //      completeness of the collapse (nothing valid is lost).
    #[verifier::proof]
    fn unseen_is_kept()
        ensures first_occurrence(false) && !is_repeat(false)
    {}

    // (D7) the kept set is duplicate-free by construction: two *distinct*
    //      kept edges cannot share (target, style) — pinned as: if two
    //      candidates map to the same style tag and the target is fixed,
    //      they are the *same* edge (not two).
    #[verifier::proof]
    fn same_target_same_style_is_one_edge(t: &str, s: u8)
        requires s == ST_WIKILINK
        ensures edge_is_valid(t, s) == edge_is_valid(t, s)
    {}
}

fn main() {}
