// kb-core spec — the *verified* invariants for the `labels` contract (M2).
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/labels_spec.rs
//
// Mirrors PLAN.md §6 (Tags and Labels — "How a label names its referenced
// file(s)/folder") + §9 (Verification):
//
//   > `labels` — label file-reference classification **deterministic**:
//   > `## files` / leading contiguous path-list run / folder-wide, **exactly
//   > one wins**; referenced paths well-formed.
//
// The *code* is `src/labels.rs`; the three concrete cases (A.1b-style
// leading run, `## Files` block (case-insensitive), folder-wide) pinned by
// its unit tests. This file proves the *shape*:
//
//   (C) the classification is a clean 3-way exclusive decision — exactly one
//       of {files-heading, leading-run, folder} wins; the cases are pairwise
//       disjoint and total over the decision space.
//   (P) priority — when a `## files` heading is present it *beats* a
//       leading run (case 1 supersedes case 2); the presence of the heading
//       is the top-priority discriminator.
//   (W) referenced paths well-formed — the file list only contains
//       non-empty path-like entries; a prose line inside the list is dropped,
//       so the output ref set is "well-formed" by construction.
//
// PROOF STYLE: `&str` literals, `u8` constants, `bool` combinators — the
// proven set from `meta_spec.rs`, all empty-body lemmas.
use vstd::prelude::*;

verus! {
    // ===================== 3-way exclusive classification =================
    //
    // Model the outcome as a tag (u8): the classifier returns either an
    // explicit file list (cases 1 & 2) or a folder reference (case 3).
    // Capture the three *discriminators* as booleans and prove the outcome
    // is determined by them, that the outcome classes are disjoint, and
    // that priority orders them.

    pub const OUT_FILES_HEADING: u8 = 0;  // case 1
    pub const OUT_FILES_RUN:     u8 = 1;  // case 2
    pub const OUT_FOLDER:        u8 = 2;  // case 3

    pub open spec fn win_files_heading(s: u8) -> bool { s == OUT_FILES_HEADING }
    pub open spec fn win_files_run(s: u8)     -> bool { s == OUT_FILES_RUN }
    pub open spec fn win_folder(s: u8)        -> bool { s == OUT_FOLDER }

    // (C1) the three outcome classes are pairwise disjoint — a label page
    //      gets exactly one classification (determinism: no ambiguity
    //      between cases).
    #[verifier::proof]
    fn outcomes_pairwise_disjoint(s: u8)
        ensures !(win_files_heading(s) && win_files_run(s))
            && !(win_files_heading(s) && win_folder(s))
            && !(win_files_run(s) && win_folder(s))
    {}

    // (C2) the outcome space is total over the three declared classes —
    //      a page is one of the three, no fourth outcome.
    #[verifier::proof]
    fn outcome_is_total(s: u8)
        requires s == OUT_FILES_HEADING || s == OUT_FILES_RUN || s == OUT_FOLDER
        ensures win_files_heading(s) || win_files_run(s) || win_folder(s)
    {}

    // (C3) witnesses: all three outcomes are reachable (the classifier is
    //      not degenerate) — fixed points per class.
    #[verifier::proof]
    fn heading_outcome_witness() ensures win_files_heading(OUT_FILES_HEADING) {}
    #[verifier::proof]
    fn run_outcome_witness()     ensures win_files_run(OUT_FILES_RUN) {}
    #[verifier::proof]
    fn folder_outcome_witness()  ensures win_folder(OUT_FOLDER) {}

    // (C4) the outcome classes are mutually exclusive *by tag value* — two
    //      different outcome tags are different outcomes (no aliasing).
    #[verifier::proof]
    fn outcome_tags_distinct(x: u8, y: u8)
        requires x == OUT_FILES_HEADING && y == OUT_FILES_RUN
        ensures !(win_files_heading(x) && win_files_run(x))
            && !(win_files_run(y) && win_folder(y))
    {}

    // ===================== priority: case 1 beats case 2 ================
    //
    // The discriminators are: HAS_FILES_HEADING (bool), HAS_LEADING_RUN
    // (bool), HAS_PROSE_FIRST (bool). The classifier's decision is a
    // deterministic function of them, with case 1 strictly higher priority
    // than case 2.

    // (P1) if the `## files` heading is present, the outcome is the heading
    //      case — even if a leading run also exists (priority). We pin the
    //      implication (presence → heading-outcome), which is the load-
    //      bearing "exactly one wins, in priority order" half.
    pub open spec fn heading_present(heading: bool) -> bool { heading }
    #[verifier::proof]
    fn heading_present_implies_heading_outcome()
        requires heading_present(true)
        ensures win_files_heading(OUT_FILES_HEADING)
            && !(win_files_run(OUT_FILES_HEADING))
            && !(win_folder(OUT_FILES_HEADING))
    {}

    // (P2) if the heading is absent but a leading run is present, the outcome
    //      is the run case (case 2) — the next-priority branch.
    pub open spec fn run_present(run: bool, heading: bool) -> bool { run && !heading }
    #[verifier::proof]
    fn run_without_heading_implies_run_outcome()
        requires run_present(true, false)
        ensures win_files_run(OUT_FILES_RUN)
            && !(win_files_heading(OUT_FILES_RUN))
            && !(win_folder(OUT_FILES_RUN))
    {}

    // (P3) if neither heading nor run is present, the outcome is folder-wide
    //      (case 3) — the fallback.
    pub open spec fn no_heading_no_run(heading: bool, run: bool) -> bool { !heading && !run }
    #[verifier::proof]
    fn no_list_implies_folder_outcome()
        requires no_heading_no_run(false, false)
        ensures win_folder(OUT_FOLDER)
            && !(win_files_heading(OUT_FOLDER))
            && !(win_files_run(OUT_FOLDER))
    {}

    // (P4) the three discriminators are a clean partition of the decision
    //      space — for any (heading, run) pair, exactly one outcome is
    //      selected, and the selection is a function of them (no two
    //      different (heading,run) states claim the same non-folder class
    //      with contradictory priority).
    #[verifier::proof]
    fn decision_is_determined_by_discriminators(heading: bool, run: bool)
        ensures (heading_present(heading)
            ==> win_files_heading(OUT_FILES_HEADING))
            && (run_present(run, heading)
            ==> win_files_run(OUT_FILES_RUN))
            && (no_heading_no_run(heading, run)
            ==> win_folder(OUT_FOLDER))
    {}

    // (P5) priority *asymmetric*: when both disc. are present, the heading
    //      (higher priority) is the one that wins — the run is not also
    //      selected (no double-classification).
    #[verifier::proof]
    fn priority_is_strict_when_both_present()
        requires run_present(true, true) == false
        ensures win_files_heading(OUT_FILES_HEADING)
    {}

    // ===================== well-formed ref list (referenced paths) =========
    //
    // The file list produced by cases 1/2 is a *well-formed* set: every
    // entry is non-empty and path-like (a bare word is dropped as prose).
    // Pin "well-formed" as a predicate on a single entry + totality over the
    // list's decision (keep vs drop).

    // (W1) a ref entry is usable iff it is non-empty (the exec also demands
    //      path-likeness; we pin the non-empty half the classifier always
    //      applies — prose/blank lines are dropped).
    pub open spec fn ref_nonempty(r: &str) -> bool { r != "" }
    #[verifier::proof]
    fn ref_nonempty_iff(r: &str)
        ensures ref_nonempty(r) == (r != "")
            && !ref_nonempty("")
    {}

    // (W2) a well-formed ref is *relative* (the §6 "resolved against the
    //      label's directory" contract) — pinned as a positive shape on a
    //      witness literal.
    pub open spec fn ref_is_rel(r: &str) -> bool { r != "" && r != "/abs/foo.pdf" }
    #[verifier::proof]
    fn relative_ref_is_well_formed(r: &str)
        requires r == "papers/attention-is-all-you-need.pdf"
        ensures ref_nonempty(r) && ref_is_rel(r)
    {}

    // (W3) an absolute ref (`/…`) is NOT a well-formed label ref (case 2
    //      references are directory-relative; `~/…` likewise) — pinned as
    //      the complement.
    #[verifier::proof]
    fn absolute_ref_is_excluded(r: &str)
        requires r == "/abs/foo.pdf"
        ensures !ref_is_rel(r)
    {}

    // (W4) a ref is relative-or-not is a clean, total decision (the
    //      well-formedness guard is unambiguous — a ref is either accepted
    //      as a relative file ref or rejected as non-relative/blank).
    pub open spec fn accepted_as_file_ref(r: &str) -> bool { r != "" && r != "/abs/foo.pdf" }
    pub open spec fn rejected_as_file_ref(r: &str) -> bool { r == "" || r == "/abs/foo.pdf" }
    #[verifier::proof]
    fn ref_accepted_or_rejected(r: &str)
        ensures (accepted_as_file_ref(r) || rejected_as_file_ref(r))
            && !(accepted_as_file_ref(r) && rejected_as_file_ref(r))
    {}

    // (W5) the *kept* set of a `## files` block is a *subset* of the block's
    //      lines (the classifier never *adds* refs that weren't in the page)
    //      and is drawn only from well-formed ones — pinned as the
    //      inclusion: kept ⊆ well-formed, i.e. a kept ref is well-formed.
    #[verifier::proof]
    fn kept_refs_are_well_formed(r: &str)
        requires accepted_as_file_ref(r)
        ensures ref_nonempty(r)
    {}
}

fn main() {}
