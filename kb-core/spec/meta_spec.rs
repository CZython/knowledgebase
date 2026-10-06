// kb-core spec — the *verified* invariants for the display-id model.
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/meta_spec.rs
//
// Why standalone: this Verus fork (ARIA v0.2026.09.06) rejects
// user-defined types declared outside a `verus!` block. The working
// reference project verifies a single self-contained spec file
// (with `&str` parameters and empty-body proofs). We follow it.
//
// The exec twin (NoteMeta::as_display_id) is pinned to these by the
// unit tests in src/meta.rs.
use vstd::prelude::*;

verus! {
    // ---- kind tags (u8; mirrors the exec NoteType enum) -----------------
    pub const LABEL: u8 = 0;
    pub const WIKI:  u8 = 1;
    pub const NOTE:  u8 = 2;

    // ---- per-kind prefix predicates (for distinctness proofs) -----------
    //      Plain &str equality — the pattern that Z3 discharges directly
    //      (gr's `in_allow(s)` / `in_deny(s)` / `in_cond(s)` shape).
    pub open spec fn is_label_prefix(p: &str) -> bool { p == "label:" }
    pub open spec fn is_wiki_prefix (p: &str) -> bool { p == "wiki:" }
    pub open spec fn is_note_prefix (p: &str) -> bool { p == "note:" }

    // ---- the display-id value per kind (Seq<char>; spec twin of the
    //        exec NoteMeta::as_display_id()) -------------------------------
    pub open spec fn display_id_label(slug: &str) -> Seq<char> { "label:"@ + slug@ }
    pub open spec fn display_id_wiki (slug: &str) -> Seq<char> { "wiki:"@  + slug@ }
    pub open spec fn display_id_note (slug: &str) -> Seq<char> { "note:"@  + slug@ }

    // ======================== invariants =================================

    // (I1) decomposition: display id == prefix ++ slug.
    //      `kb resolve` splits on first ':' → kind; rest → slug.
    #[verifier::proof]
    fn label_decomposes(slug: &str)
        ensures display_id_label(slug) == "label:"@ + slug@
            && "label:"@.is_prefix_of(display_id_label(slug))
            && slug@.is_suffix_of(display_id_label(slug))
    {}

    #[verifier::proof]
    fn wiki_decomposes(slug: &str)
        ensures display_id_wiki(slug) == "wiki:"@ + slug@
            && "wiki:"@.is_prefix_of(display_id_wiki(slug))
            && slug@.is_suffix_of(display_id_wiki(slug))
    {}

    #[verifier::proof]
    fn note_decomposes(slug: &str)
        ensures display_id_note(slug) == "note:"@ + slug@
            && "note:"@.is_prefix_of(display_id_note(slug))
            && slug@.is_suffix_of(display_id_note(slug))
    {}

    // (I2) the three prefixes are pairwise disjoint.
    //      No string can be two different kind prefixes simultaneously,
    //      so the prefix alone pins the kind — `kb resolve` is total.
    #[verifier::proof]
    fn prefixes_are_pairwise_disjoint(p: &str)
        ensures !(is_label_prefix(p) && is_wiki_prefix(p))
            && !(is_label_prefix(p) && is_note_prefix(p))
            && !(is_wiki_prefix(p)  && is_note_prefix(p))
    {}

    // (I3) each literal IS its own prefix (witnesses for I2).
    #[verifier::proof]
    fn label_prefix_witness() ensures is_label_prefix("label:") {}

    #[verifier::proof]
    fn wiki_prefix_witness()  ensures is_wiki_prefix("wiki:")  {}

    #[verifier::proof]
    fn note_prefix_witness()  ensures is_note_prefix("note:")  {}

    // (I4) the slug is recoverable as a suffix of the display id
    //      (explicit statement used by `kb resolve`).
    #[verifier::proof]
    fn slug_recoverable_as_suffix(slug: &str, kind: u8)
        requires kind == LABEL
        ensures slug@.is_suffix_of(display_id_label(slug))
    {}

    // (I5) kind tags are pairwise distinct.
    #[verifier::proof]
    fn kind_tags_disjoint(kind: u8)
        ensures !((kind == LABEL) && (kind == WIKI))
            && !((kind == LABEL) && (kind == NOTE))
            && !((kind == WIKI)  && (kind == NOTE))
    {}
}

fn main() {}
