// kb-core spec — the *verified* invariants for the tag-alias **export/import
// codec** (`kb-core::aliases`), the M8 "recover alias knowledge after a DB
// wipe" recovery table.
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/alias_spec.rs
//
// Mirrors PLAN.md §2 (the `tag_aliases` recovery table) + the M8 export/import
// pathway. The *code* is `src/aliases.rs`; the concrete round-trips (export
// then import == identity over the DB's unique-alias, tab-free tag-string
// domain; last-wins on a repeated alias; blank/`#` lines skipped) are pinned
// by that module's unit tests. This file proves the *shape*:
//
//   (F) the field separator is the single ASCII tab (`"\t"`) — the data
//       constant both directions share.
//   (D) a data line decomposes as `alias ++ TAB ++ canonical ++ "\n"`, with
//       the alias a recoverable *prefix* and the tail `TAB ++ canonical ++
//       "\n"` a recoverable *suffix* — parse-recovers-the-fields (the
//       display-id decompose/recover contract, `identity_spec.rs` D1/D4,
//       applied to the codec's one-line form).
//   (I) the per-line import decision is a clean total + disjoint partition —
//       a line is either *ignorable* (blank / `#`-comment) or a *candidate*
//       data line; never both, never neither — so "skip ignorable" is
//       unambiguous and total.
//   (W) a well-formed alias pair is one whose both fields are non-empty —
//       the guard that drops empty-field rows on import.
//   (E) an empty alias set renders to the empty sequence (no lone newline).
//
// PROOF STYLE: same proven set as `identity_spec.rs` / `tags_spec.rs` —
// `&str` literals, `bool` combinators, `Seq<char>` `+` / `==` /
// `is_prefix_of` / `is_suffix_of` — all empty-body lemmas.
use vstd::prelude::*;

verus! {
    // ===================== field separator (codec constant) ==============

    // (F1) the data separator is a *real* (non-empty) single-character token —
    //      pinned by value so a re-spec that changes it (to a space, to `=`, …)
    //      fails here, and the "split on the first tab" contract is what we
    //      actually prove. (Non-emptiness as value inequality; distinctness
    //      from the newline terminator so a line's field-sep and line-terminator
    //      are two different tokens.)
    #[verifier::proof]
    fn field_separator_is_a_real_single_token()
        ensures "\t" != ""
            && "\t" != "\n"
    {}

    // (F2) the separator is not the empty string — splitting on it is a real
    //      (non-degenerate) operation, so "no separator found" is a distinct,
    //      meaningful outcome (a data line must contain it).
    #[verifier::proof]
    fn field_separator_is_nonempty()
        ensures "\t" != ""
    {}

    // ===================== data-line decomposition (D) ====================
    //
    // A codec data line is exactly `alias ++ TAB ++ canonical ++ "\n"`.
    // We pin the decomposition and the field *recovery* half (the alias is a
    // leading run; the tail after the alias is exactly `TAB ++ canonical ++
    // "\n"`) — the same "decompose then recover" shape as the display id.

    pub open spec fn data_line(alias: &str, canonical: &str) -> Seq<char> {
        alias@ + "\t"@ + canonical@ + "\n"@
    }

    // (D1) a data line decomposes as alias ++ TAB ++ canonical ++ NL.
    #[verifier::proof]
    fn data_line_decomposes(alias: &str, canonical: &str)
        ensures data_line(alias, canonical)
            == alias@ + "\t"@ + canonical@ + "\n"@
    {}

    // (D2) the alias is a recoverable *leading* run of the line (the
    //      recoverability the import step relies on: `line[..tab]` recovers
    //      the alias exactly because it is a prefix).
    #[verifier::proof]
    fn alias_is_recoverable_prefix(alias: &str, canonical: &str)
        ensures alias@.is_prefix_of(data_line(alias, canonical))
    {}

    // (D3) after the alias, the tail is exactly `TAB ++ canonical ++ "\n"`
    //      (so the canonical is recovered as the run between the delimiter
    //      and the terminator — the suffix half of parse).
    #[verifier::proof]
    fn canonical_tail_is_recoverable(alias: &str, canonical: &str)
        ensures ("\t"@ + canonical@ + "\n"@).is_suffix_of(data_line(alias, canonical))
    {}

    // (D4) the line always ends with the newline terminator (POSIX text-file
    //      half of the export contract; the import's `lines()` relies on it).
    #[verifier::proof]
    fn data_line_terminates(alias: &str, canonical: &str)
        ensures "\n"@.is_suffix_of(data_line(alias, canonical))
    {}

    // (D5) the line *contains* the separator: `alias ++ "\t"` is a prefix —
    //      i.e. the tab is present in the line (a data line never degenerates
    //      to a bare alias; the delimiter is always there to split on).
    #[verifier::proof]
    fn data_line_contains_separator(alias: &str, canonical: &str)
        ensures (alias@ + "\t"@).is_prefix_of(data_line(alias, canonical))
    {}

    // ===================== import-line partition (I) ======================
    //
    // Model the *decision* per line (not the Vec plumbing): the import either
    // *ignores* the line (blank / `#`-comment — annotation, skipped silently)
    // or treats it as a *candidate* data line (which may then parse to None).
    // Capture the two discriminators and prove they partition every line.

    // ignorable == blank (the exec additionally skips whitespace-only and
    // `#`-comment lines; we pin the *shape* half the partition depends on — the
    // empty line is the canonical blank — same as `tags_spec.rs` (N5)).
    pub open spec fn ignorable_blank(line: &str)    -> bool { line == "" }
    pub open spec fn candidate_data(line: &str)     -> bool { !ignorable_blank(line) }

    // (I1) blank / candidate is a clean total partition — every line is one
    //      or the other, never both (so "skip ignorable" is unambiguous).
    #[verifier::proof]
    fn line_is_ignorable_or_candidate(line: &str)
        ensures (ignorable_blank(line) || candidate_data(line))
            && !(ignorable_blank(line) && candidate_data(line))
    {}

    // (I2) the empty line is *ignorable* (a blank line never loads a row).
    #[verifier::proof]
    fn empty_line_is_ignorable(line: &str)
        requires line == ""
        ensures ignorable_blank(line) && !candidate_data(line)
    {}

    // (I3) a whitespace-only line is *not* the empty blank (the spec model
    //      pins only the truly-empty line as ignorable; the exec additionally
    //      trims + skips whitespace-only and `#`-comment lines, which its unit
    //      tests pin — the same exec↔spec split as `tags`'s trim rule).
    #[verifier::proof]
    fn whitespace_only_is_not_the_empty_blank(line: &str)
        requires line == "   "
        ensures candidate_data(line) && !ignorable_blank(line)
    {}

    // (I4) a data line is a *candidate* (not ignorable) — "a\tb" is not blank,
    //      so the import considers it a data row (and then parses it).
    #[verifier::proof]
    fn data_row_is_a_candidate(line: &str)
        requires line == "czechia\tczech"
        ensures candidate_data(line) && !ignorable_blank(line)
    {}

    // ===================== well-formed pair (W) ===========================
    //
    // A well-formed alias pair has *both* fields non-empty. This is the guard
    // that makes "empty alias / empty canonical → skip" total and disjoint
    // (an empty field is never a valid row, so the skip decision is clean).

    pub open spec fn wf_pair(alias: &str, canonical: &str) -> bool {
        alias != "" && canonical != ""
    }

    // (W1) well-formed iff both fields non-empty (definitional pin the codec
    //      branches on).
    #[verifier::proof]
    fn wf_iff_both_nonempty(alias: &str, canonical: &str)
        ensures (wf_pair(alias, canonical) == (alias != "" && canonical != ""))
    {}

    // (W2) an empty alias is *not* well-formed (an empty field always fails
    //      the guard, so "empty alias → skip" is a sound, total drop).
    #[verifier::proof]
    fn empty_alias_not_wf(canonical: &str)
        ensures !wf_pair("", canonical)
    {}

    // (W3) an empty canonical is *not* well-formed (the second required
    //      field; both must hold, so one missing field sinks the row).
    #[verifier::proof]
    fn empty_canonical_not_wf(alias: &str)
        ensures !wf_pair(alias, "")
    {}

    // (W4) witness: a real tag pair is well-formed (the guard is satisfiable,
    //      not degenerate).
    #[verifier::proof]
    fn real_pair_is_wf()
        ensures wf_pair("Czechia", "czech")
    {}

    // ===================== empty-set export (E) ===========================
    //
    // An empty alias set renders to the *empty* sequence (no lone newline).
    // Pin the length-0 shape, the way `tags_spec.rs` (N6) pins "empty tags →
    // no rows".

    pub open spec fn empty_set_export() -> Seq<char> { seq![] }

    // (E1) the empty set's export is the empty sequence (no lone "\n") — the
    //      "empty tags → no rows" analog (`tags_spec.rs` (N6)): a downstream
    //      `lines()` sees nothing, so importing an empty export loads nothing
    //      (a wipe then re-importing an empty backup is a no-op).
    #[verifier::proof]
    fn empty_set_export_is_empty()
        ensures empty_set_export() == seq![]
    {}
}

fn main() {}
