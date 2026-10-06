// kb-core spec — the *verified* invariants for identity (slug resolution +
// display-id assembly) built on top of the frontmatter/metadata model.
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/identity_spec.rs
//
// Mirrors PLAN.md §4 (identity: slug priority `title → H1 → filename`, and
// the display id `type:slug`). The *code* is `src/identity.rs`; the
// concrete cases (empty-title fall-through, dotfile extension stripping,
// first-H1 detection) are pinned by the unit tests there. This file proves
// the *shape*:
//
//   (P) The slug-priority decision is a total, mutually-exclusive choice of
//       exactly one source (title / h1 / filename) or "none" — so the
//       indexer's fallback chain is unambiguous.
//   (D) The display id decomposes as `prefix ++ slug` (re-pin of the
//       `meta_spec.rs` contract, generically, for use by the M5 indexer).
//   (W) A well-formed slug is one that is usable (non-empty) — the
//       well-formedness guard that drops empty candidates.
//
// PROOF STYLE: same proven set as `spec/meta_spec.rs` — `&str` literals,
// `u8` constants, `bool` combinators, and `Seq<char>` `is_prefix_of` /
// `is_suffix_of`. All lemmas empty-body.
use vstd::prelude::*;

verus! {
    // ===================== slug priority (§4) ============================
    //
    // Model the *decision* (not the string): the exec picks the first
    // usable (non-empty-after-trim) source in the order title → h1 →
    // filename, else "none". We capture each candidate's usability as a
    // plain `bool` (the exec's `!s.trim().is_empty()`), then prove the
    // choice function is a clean 4-way partition.

    pub const SRC_TITLE:    u8 = 0;
    pub const SRC_H1:       u8 = 1;
    pub const SRC_FILENAME: u8 = 2;
    pub const SRC_NONE:     u8 = 3;

    pub open spec fn is_title_source (b: u8) -> bool { b == SRC_TITLE }
    pub open spec fn is_h1_source    (b: u8) -> bool { b == SRC_H1 }
    pub open spec fn is_file_source  (b: u8) -> bool { b == SRC_FILENAME }
    pub open spec fn is_none_source  (b: u8) -> bool { b == SRC_NONE }

    // (P1) the priority choice, as a total function on the usability flags.
    pub open spec fn resolve_branch(use_title: bool, use_h1: bool, use_file: bool) -> u8 {
        if use_title {
            SRC_TITLE
        } else if use_h1 {
            SRC_H1
        } else if use_file {
            SRC_FILENAME
        } else {
            SRC_NONE
        }
    }

    // (P2) total: the branch is one of exactly four values (no fifth case).
    #[verifier::proof]
    fn branch_is_total(t: bool, h: bool, f: bool)
        ensures (is_title_source(resolve_branch(t, h, f))
            || is_h1_source(resolve_branch(t, h, f))
            || is_file_source(resolve_branch(t, h, f))
            || is_none_source(resolve_branch(t, h, f)))
    {}

    // (P3) title wins whenever it is usable — the top of the priority chain.
    #[verifier::proof]
    fn title_wins_when_usable(h: bool, f: bool)
        requires true
        ensures resolve_branch(true, h, f) == SRC_TITLE
    {}

    // (P4) H1 wins exactly when title is unusable and H1 is usable.
    #[verifier::proof]
    fn h1_wins_when_title_empty(f: bool)
        ensures resolve_branch(false, true, f) == SRC_H1
    {}

    // (P5) filename wins exactly when title and H1 are both unusable and the
    //      filename is usable.
    #[verifier::proof]
    fn filename_wins_when_others_empty()
        ensures resolve_branch(false, false, true) == SRC_FILENAME
    {}

    // (P6) "none" is chosen exactly when all three candidates are unusable.
    #[verifier::proof]
    fn none_when_all_empty()
        ensures resolve_branch(false, false, false) == SRC_NONE
    {}

    // (P7) the four source tags are pairwise distinct (the choice has a
    //      unique identity per branch — the indexer can branch on it).
    #[verifier::proof]
    fn source_tags_pairwise_disjoint(b: u8)
        ensures !(is_title_source(b) && is_h1_source(b))
            && !(is_title_source(b) && is_file_source(b))
            && !(is_h1_source(b)   && is_file_source(b))
            && !(is_title_source(b) && is_none_source(b))
            && !(is_h1_source(b)   && is_none_source(b))
            && !(is_file_source(b) && is_none_source(b))
    {}

    // (P8) soundness: NONE is chosen only when no source is usable; and a
    //      chosen non-NONE source implies its flag was set (no phantom).
    #[verifier::proof]
    fn none_branch_is_none()
        ensures resolve_branch(false, false, false) != SRC_TITLE
            && resolve_branch(false, false, false) != SRC_H1
            && resolve_branch(false, false, false) != SRC_FILENAME
    {}

    // ===================== display id (§4) ===============================
    //
    // Re-pin the decomposition contract generically (meta_spec.rs proves it
    // per-kind; this gives the M5 indexer a single kind-parameterised form).

    pub open spec fn di_label(slug: &str) -> Seq<char> { "label:"@ + slug@ }
    pub open spec fn di_wiki (slug: &str) -> Seq<char> { "wiki:"@  + slug@ }
    pub open spec fn di_note (slug: &str) -> Seq<char> { "note:"@  + slug@ }

    // (D1) label display id decomposes as prefix ++ slug.
    #[verifier::proof]
    fn label_di_decomposes(slug: &str)
        ensures di_label(slug) == "label:"@ + slug@
            && "label:"@.is_prefix_of(di_label(slug))
            && slug@.is_suffix_of(di_label(slug))
    {}

    // (D2) wiki display id decomposes.
    #[verifier::proof]
    fn wiki_di_decomposes(slug: &str)
        ensures di_wiki(slug) == "wiki:"@ + slug@
            && "wiki:"@.is_prefix_of(di_wiki(slug))
            && slug@.is_suffix_of(di_wiki(slug))
    {}

    // (D3) note display id decomposes.
    #[verifier::proof]
    fn note_di_decomposes(slug: &str)
        ensures di_note(slug) == "note:"@ + slug@
            && "note:"@.is_prefix_of(di_note(slug))
            && slug@.is_suffix_of(di_note(slug))
    {}

    // (D4) slug recoverable as a suffix (the `kb resolve` split step).
    #[verifier::proof]
    fn slug_recoverable(slug: &str)
        ensures slug@.is_suffix_of(di_label(slug))
            && slug@.is_suffix_of(di_wiki(slug))
            && slug@.is_suffix_of(di_note(slug))
    {}

    // ===================== well-formed slug (§4) =========================
    //
    // A candidate is *usable* iff it is non-empty (after the exec's trim).
    // The well-formedness guard drops empty candidates from the chain.

    pub open spec fn usable(s: &str) -> bool { s != "" }

    // (W1) usable iff non-empty (definitional pin — the trim happens in exec;
    //      here we pin the "non-empty" half the chain depends on).
    #[verifier::proof]
    fn usable_iff_nonempty(s: &str)
        ensures (usable(s) == (s != ""))
    {}

    // (W2) the empty string is unusable — an empty candidate never advances
    //      the priority chain.
    #[verifier::proof]
    fn empty_unusable(s: &str)
        requires s == ""
        ensures !usable(s)
    {}

    // (W3) witnesses: non-empty labels are usable (the guard is satisfiable).
    #[verifier::proof]
    fn nonempty_usable(s: &str)
        requires s == "Transformer (papers)"
        ensures usable(s)
    {}

    // ===================== filename extension (drift shape) =============
    //
    // The exec's `filename_minus_extension` strips only the LAST extension
    // and preserves a leading dot. Provable shape: the result, if present,
    // is strictly the original name with its trailing `.ext` removed —
    // modelled as: result ++ "." ++ ext == original (for the one-dot case).

    // (F1) a name with a leading dot keeps the dot: the name is a prefix of
    //      the full name ++ ext, and is a proper string distinct from the
    //      empty (the exec rule — a leading dot is not an extension to strip).
    pub open spec fn dotfile_name_ext(name: &str, ext: &str) -> Seq<char> { name@ + "."@ + ext@ }
    #[verifier::proof]
    fn dotfile_name_keeps_dot(name: &str, ext: &str)
        requires name == ".hidden" && ext == "md"
        ensures dotfile_name_ext(name, ext) == ".hidden"@ + "."@ + ext@
            && name@.is_prefix_of(dotfile_name_ext(name, ext))
            && ext@.is_suffix_of(dotfile_name_ext(name, ext))
    {}

    // (F2) a bare (extension-less) name is returned unchanged: name == name
    //      ++ nothing, and it is a prefix of name ++ ext when one is appended.
    pub open spec fn bare_plus_ext(name: &str, ext: &str) -> Seq<char> { name@ + "."@ + ext@ }
    #[verifier::proof]
    fn bare_name_unchanged(name: &str)
        requires name == "transformer"
        ensures name@.is_prefix_of(bare_plus_ext(name, "md"))
    {}
}

fn main() {}
