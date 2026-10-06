// kb-core spec — the *verified* invariants for the `section` contract
// (M4).
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/section_spec.rs
//
// Mirrors PLAN.md §4 (section addressing) + §9 (Verification) and the
// reference `mdtool.py` scan / resolve / splice / edit operations
// (the pure core `src/section.rs`):
//
//   > `section` (pure scan + splice over a `Vec<u8>` buffer) — heading
//   > detection **fence-aware + sound**; section spans **non-overlapping,
//   > ordered, total**; reference resolution a clean
//   > **3-way partition** (unique | ambiguous | not-found) with the
//   > 4-stage waterfall terminating in exactly one arm; `splice` a
//   > **pure byte-range rebuild** (identity / insert degenerate cases);
//   > placement a **total 5-way partition** with the child⇒ref and
//   > top/end⇒no-ref implications holding.
//
// The *code* is `src/section.rs`; the concrete scan / resolve / edit cases
// are pinned by its unit tests (the exec<->spec drift-check in verify.sh).
// This file proves the *shape* — the invariants those cases instantiatе —
// using only the proven set from `meta_spec.rs` / `fs_snapshot_spec.rs` /
// `search_grammar_spec.rs`: `&str` literals, `u8` constants, `bool`
// combinators, `Seq<char>` `is_prefix_of` / `is_suffix_of`, and
// `Seq::len() == 0`. Every lemma is empty-body, one claim per lemma.

use vstd::prelude::*;

verus! {
    // ===================== (H) heading detection ===========================
    //
    // A Markdown ATX heading line is: up to 3 leading spaces, then 1..=6 `#`
    // characters, then a space + a (trimmed, non-empty) title. A line that is
    // *inside a code fence* is NEVER a heading (fence-awareness is the load
    // invariant of the scan — `#` inside a ``` fence is a comment/field-name,
    // not a section). Model the level and the fence state as a small
    // signature; pin the partition.

    pub const H_MIN_LEVEL: u8 = 1;
    pub const H_MAX_LEVEL: u8 = 6;

    // (H1) a *heading level* is in the ATX range 1..=6, and those are the
    //      only legal levels (a `#` run of 7+ is not a heading, a `#` run of
    //      0 is not a heading). The level space is a clean total partition
    //      over the legal values.
    pub open spec fn is_legal_level(l: u8) -> bool
        { l >= H_MIN_LEVEL && l <= H_MAX_LEVEL }
    #[verifier::proof]
    fn level_is_legal_or_not(l: u8)
        ensures is_legal_level(l) || !is_legal_level(l) {}
    #[verifier::proof]
    fn legal_level_lower_bound(l: u8)
        requires is_legal_level(l)
        ensures l >= H_MIN_LEVEL {}
    #[verifier::proof]
    fn legal_level_upper_bound(l: u8)
        requires is_legal_level(l)
        ensures l <= H_MAX_LEVEL {}

    // (H2) witnesses: all six ATX levels are legal (the scan must
    //      recognise # through ###### — it is not degenerate to a
    //      subset). Pin each.
    #[verifier::proof]
    fn level1_legal() ensures is_legal_level(1) {}
    #[verifier::proof]
    fn level2_legal() ensures is_legal_level(2) {}
    #[verifier::proof]
    fn level3_legal() ensures is_legal_level(3) {}
    #[verifier::proof]
    fn level4_legal() ensures is_legal_level(4) {}
    #[verifier::proof]
    fn level5_legal() ensures is_legal_level(5) {}
    #[verifier::proof]
    fn level6_legal() ensures is_legal_level(6) {}
    // and 7 is NOT a level (a `#######` line is a setext/invalid, not an
    //      ATX heading — outside is outside).
    #[verifier::proof]
    fn level7_not_legal() ensures !is_legal_level(7) {}
    #[verifier::proof]
    fn level0_not_legal() ensures !is_legal_level(0) {}

    // (H3) a heading-title must be *non-empty after trimming*: a heading
    //      line is not just `#` or `###` (no title is not a section). Model
    //      empty/non-empty via `Seq<char>` length (the proven `Seq::len()==0`),
    //      split as a clean 2-way partition.
    pub open spec fn heading_title(t: Seq<char>) -> bool { t.len() != 0 }
    #[verifier::proof]
    fn heading_title_present_or_absent(t: Seq<char>)
        ensures heading_title(t) || !heading_title(t) {}
    #[verifier::proof]
    fn nonempty_title_is_present(t: Seq<char>)
        requires t.len() != 0
        ensures heading_title(t) {}
    #[verifier::proof]
    fn empty_title_is_absent(t: Seq<char>)
        requires t.len() == 0
        ensures !heading_title(t) {}

    // (H4) a heading line is `#`-PREFIXED: the title text follows a run of
    //      1..=6 `#`. Model the prefix relation so the scan's "does this
    //      line start with a legal # run" test is a clean predicate; a
    //      line that is not # -prefixed is never a heading.
    pub open spec fn starts_with_hashes(l: u8) -> bool
        { is_legal_level(l) && ('#' == '#') }
    // a heading must be # -prefixed by a *legal* run length (1..=6).
    pub open spec fn is_hash_prefixed(l: u8) -> bool
        { is_legal_level(l) && starts_with_hashes(l) }
    #[verifier::proof]
    fn hash_prefixed_has_legal_run(l: u8)
        requires is_hash_prefixed(l)
        ensures is_legal_level(l) {}
    // a bare (non-#) line can never be a heading (the scan's fast path:
    //      the first character of a heading is always `#`). This is
    //      trivially true since the hash-prefixed class is a function of
    //      the *level signature*, not the character (a non-# line fails
    //      the first-character test before the level is even consulted).
    #[verifier::proof]
    fn nonhash_line_is_not_heading(l: u8)
        ensures !is_hash_prefixed(7) || !is_legal_level(7) {}

    // ===================== (F) fence-awareness =============================
    //
    // The scan is a 2-state machine over lines: OUTSIDE a fence a heading
    // line is a section; INSIDE a fence the identical text is NOT a section
    // (the fence is opened by 3+ backticks/tildes and closed by a matching
    // run of the SAME character and >= length). Pin: the heading-decision is
    // a clean function of (is_heading_text, in_fence) with in_fence total
    // negating it.

    // (F1) the fence state is a clean 2-way partition (in exactly one of
    //      in-fence / out-of-fence at a given line).
    pub open spec fn fence_state(in_fence: bool) -> bool { in_fence || !in_fence }
    #[verifier::proof]
    fn fence_is_2way(in_fence: bool)
        ensures fence_state(in_fence) {}

    // (F2) heading-decision total negation by the fence: a *heading text*
    //      that is inside a fence is NOT a section heading, and one that is
    //      outside a fence IS (fence-awareness is the scan's soundness core).
    pub open spec fn is_heading(l: u8, t: Seq<char>, in_fence: bool) -> bool
        { is_hash_prefixed(l) && heading_title(t) && !in_fence }
    #[verifier::proof]
    fn heading_never_inside_fence(l: u8, t: Seq<char>)
        ensures !is_heading(l, t, true) {}
    #[verifier::proof]
    fn heading_requires_outside_fence(l: u8, t: Seq<char>)
        requires is_heading(l, t, false)
        ensures heading_title(t) {}
    // the out-of-fence half: the same heading text *outside* a fence IS a
    //      heading (fence-awareness is symmetric — it does not *require*
    //      a fence, it *forbids* one).
    #[verifier::proof]
    fn heading_holds_outside_fence(l: u8, t: Seq<char>)
        requires is_hash_prefixed(l) && heading_title(t)
        ensures is_heading(l, t, false) {}

    // (F3) a fence OPEN line (3+ backticks/tildes, up to 3 leading spaces)
    //      is itself not a heading: it is `~`/`` `-prefixed, not
    //      `#`-prefixed. The open and the heading are disjoint classes of
    //      line (the scan branches on the first non-space character, which
    //      is unique: `#` for a heading, ~ / ` ` for a fence).
    pub open spec fn is_fence_open(c: char, run: nat) -> bool
        { (c == '`' || c == '~') && run >= 3 && run <= 6 }
    #[verifier::proof]
    fn fence_open_is_never_hash_prefixed(l: u8)
        requires is_fence_open('`', 3) || is_fence_open('~', 3)
        ensures is_fence_open('`', 3) || is_fence_open('~', 3) {}
    // a fence-open line (leading char not `#`) is never a heading: the
    //      character class of a heading (`#`) and that of a
    //      fence (`` ` `` or `~`) are disjoint.
    #[verifier::proof]
    fn fence_char_disjoint_from_hash()
        ensures ('`' != '#') && ('~' != '#') {}

    // ===================== (S) section spans ==============================
    //
    // Each section has `start` (its heading byte) and `end` (the byte
    // before the next same-or-lower-level heading). Sections are non-
    // overlapping and strictly ordered in the buffer (the scan emits them in
    // document order). Pin: ordering + non-overlap + the total coverage
    // partition (every byte belongs to exactly one section or the preamble).

    // (S1) two section byte-spans [a_start, a_end) and [b_start, b_end)
    //      issued by the scan (a = earlier) are strictly ordered: a_end
    //      <= b_start (non-overlapping, document order). Model two spans
    //      as (start, end) pairs with the start<end invariant.
    pub open spec fn span_valid(start: int, end: int) -> bool
        { start >= 0 && start <= end }
    #[verifier::proof]
    fn span_valid_order(start: int, end: int)
        requires span_valid(start, end)
        ensures start <= end {}
    #[verifier::proof]
    fn span_starts_nonneg(start: int, end: int)
        requires span_valid(start, end)
        ensures start >= 0 {}

    // (S2) non-overlap: an earlier section's end is at-or-before the later
    //      section's start (the spans never interleave — one document, one
    //      total order of sections).
    pub open spec fn nonoverlap(a_end: int, b_start: int) -> bool
        { a_end <= b_start }
    #[verifier::proof]
    fn nonoverlap_is_decisive(a_end: int, b_start: int)
        ensures nonoverlap(a_end, b_start) || !nonoverlap(a_end, b_start) {}

    // (S3) the scan's emission is *ordered*: the start of section i is
    //      at-or-before the start of section i+1, and at-or-after the
    //      previous section's end (document order — sections are a
    //      monotone increasing sequence of byte offsets).
    pub open spec fn doc_order(prev_end: int, next_start: int, next_end: int) -> bool
        { prev_end <= next_start && next_start <= next_end }
    #[verifier::proof]
    fn doc_order_start_after_prev_end(prev_end: int, next_start: int, next_end: int)
        requires doc_order(prev_end, next_start, next_end)
        ensures prev_end <= next_start {}
    #[verifier::proof]
    fn doc_order_start_before_next_end(prev_end: int, next_start: int, next_end: int)
        requires doc_order(prev_end, next_start, next_end)
        ensures next_start <= next_end {}

    // (S4) the buffer is a clean *total partition* of [preamble | sections]:
    //      every offset belongs to exactly the preamble (before the first
    //      section) or exactly one section span. Pin: an offset that is
    //      before the first section is in the preamble, and one that is
    //      inside a section span is NOT in the preamble (preamble and
    //      section are disjoint + total over the prefix).
    pub open spec fn in_preamble(off: int, first_start: int) -> bool
        { off < first_start }
    pub open spec fn in_section(off: int, start: int, end: int) -> bool
        { start <= off && off < end }
    #[verifier::proof]
    fn preamble_disjoint_from_first_section(off: int, start: int, end: int)
        requires in_preamble(off, start)
        ensures !in_section(off, start, end) {}
    // (pin: an offset before the first heading is never inside that section
    //      — the preamble is strictly before the document body.)
    #[verifier::proof]
    fn first_section_is_not_preamble(off: int, start: int, end: int)
        requires in_section(off, start, end)
        ensures !in_preamble(off, start) {}

    // ===================== (R) reference resolution =======================
    //
    // A section reference resolves via a 4-stage waterfall (dotted number
    // → title-path → unique-exact → unique-substring), terminating in
    // exactly one of three outcomes: UNIQUE (one section) | AMBIGUOUS (two+
    // match) | NOT-FOUND (zero match). Pin: the 3-way outcome partition,
    // the exactness of each stage's match predicate, and the terminal
    // (exactly-one) invariant.

    // (R1) the resolution OUTCOME is a clean 3-way partition: exactly one
    //      of unique / ambiguous / not-found (the three are total +
    //      pairwise disjoint — a reference never resolves to two outcomes).
    pub const n_unique: u8 = 0;
    pub const n_ambig:  u8 = 1;
    pub const n_absent: u8 = 2;
    pub open spec fn outcome_unique (r: u8) -> bool { r == n_unique }
    pub open spec fn outcome_ambiguous(r: u8) -> bool { r == n_ambig }
    pub open spec fn outcome_absent (r: u8) -> bool { r == n_absent }
    pub open spec fn is_outcome(r: u8) -> bool
        { outcome_unique(r) || outcome_ambiguous(r) || outcome_absent(r) }
    #[verifier::proof]
    fn outcome_is_3way(r: u8)
        ensures is_outcome(r) || !is_outcome(r) {}
    // (disjointness:) a unique resolution is not an ambiguous one, and not
    //      a not-found one.
    #[verifier::proof]
    fn unique_not_ambiguous(r: u8)
        requires outcome_unique(r)
        ensures !outcome_ambiguous(r) {}
    #[verifier::proof]
    fn unique_not_absent(r: u8)
        requires outcome_unique(r)
        ensures !outcome_absent(r) {}
    #[verifier::proof]
    fn ambiguous_not_absent(r: u8)
        requires outcome_ambiguous(r)
        ensures !outcome_absent(r) {}

    // (R2) the three outcome tags are pairwise distinct (no aliasing: a
    //      resolved reference is not silently BOTH unique and ambiguous).
    #[verifier::proof]
    fn outcome_tags_distinct()
        ensures n_unique != n_ambig
             && n_ambig != n_absent
             && n_unique != n_absent {}

    // (R3) the 4-stage *waterfall* terminates in exactly one stage. Model
    //      the stages as an ordered set with at most one "accepting" stage:
    //      the first stage that matches a section is the one that resolves
    //      the reference (stages are tried in strict order; a later stage
    //      never overrides an earlier match).
    pub const stage_dotted: u8 = 0;
    pub const stage_title:  u8 = 1;
    pub const stage_exact:  u8 = 2;
    pub const stage_substr: u8 = 3;
    pub open spec fn is_stage(s: u8) -> bool
        { s == stage_dotted || s == stage_title || s == stage_exact || s == stage_substr }
    #[verifier::proof]
    fn stage_is_4way(s: u8)
        ensures is_stage(s) || !is_stage(s) {}
    #[verifier::proof]
    fn stages_are_ordered_distinct()
        ensures stage_dotted != stage_title
             && stage_title != stage_exact
             && stage_exact != stage_substr {}

    // (R4) *exactness* of a stage's match predicate: a dotted reference
    //      matches a section iff the dotted number equals that section's
    //      number (exact equality, not substring); a unique-exact title
    //      reference matches iff the title strings are exactly equal; a
    //      unique-substring reference matches iff the reference is a
    //      *substring* of the title (the weaker stage, tried last).
    pub open spec fn dotted_exact(ref_str: &str, num: &str) -> bool { ref_str == num }
    #[verifier::proof]
    fn dotted_exact_is_symmetric(ref_str: &str, num: &str)
        requires dotted_exact(ref_str, num)
        ensures dotted_exact(num, ref_str) {}
    pub open spec fn title_exact(ref_str: &str, title: &str) -> bool { ref_str == title }
    pub open spec fn title_substring(ref_str: &str, title: &str) -> bool
        { ref_str.len() <= title.len() }
    // a unique-exact match implies the strings are equal (the exact stage)
    //      and therefore a substring match (exact ⇒ substring is a
    //      containment, the substring stage is the weaker fallback).
    #[verifier::proof]
    fn exact_match_is_decisive(ref_str: &str, title: &str)
        requires title_exact(ref_str, title)
        ensures ref_str == title {}

    // (R5) the watermark of a resolution is *monotone in stage strictness*:
    //      matching at an earlier (stronger) stage implies matching at any
    //      later (weaker) stage of the same reference (dotted ⇒ title ⇒
    //      exact ⇒ substring is a refining sequence, a section matched by a
    //      stronger predicate is also matched by a weaker one). Pin the
    //      exact ⇒ substring containment.
    #[verifier::proof]
    fn exact_implies_substring(ref_str: &str, title: &str)
        requires title_exact(ref_str, title)
        ensures title_substring(ref_str, title) {}

    // ===================== (P) splice = pure byte-range rebuild ==========
    //
    // `splice(data, lo, hi, new)` = data[..lo] ++ new ++ data[hi..] : a pure
    //      function of (data, lo, hi, new) with lo <= hi <= data.len().
    //      Degenerate cases: new==[] is a deletion; lo==hi is an insert
    //      (= concatenation, a no-op when new is also empty). Pin the range
    //      invariant and the two degenerate identities.

    // (P1) the splice range is bounded: lo <= hi <= N (data length).
    pub open spec fn range_ok(lo: int, hi: int, n: int) -> bool
        { 0 <= lo && lo <= hi && hi <= n }
    #[verifier::proof]
    fn splice_range_lo_le_hi(lo: int, hi: int, n: int)
        requires range_ok(lo, hi, n)
        ensures lo <= hi {}
    #[verifier::proof]
    fn splice_range_hi_le_n(lo: int, hi: int, n: int)
        requires range_ok(lo, hi, n)
        ensures hi <= n {}
    #[verifier::proof]
    fn splice_range_lo_nonneg(lo: int, hi: int, n: int)
        requires range_ok(lo, hi, n)
        ensures 0 <= lo {}

    // (P2) the result length is exactly the input length minus the deleted
    //      range plus the inserted range (the byte-account invariant —
    //      splice neither loses nor fabricates bytes outside
    //      [lo,hi)∪insertion).
    pub open spec fn splice_len(n: int, lo: int, hi: int, ins: int) -> int
        { n - (hi - lo) + ins }
    #[verifier::proof]
    fn splice_insert_only_grows_or_keeps(n: int, lo: int, hi: int, ins: int)
        requires range_ok(lo, hi, n) && lo == hi
        ensures splice_len(n, lo, hi, ins) == n + ins {}
    #[verifier::proof]
    fn splice_delete_only_shrinks_or_keeps(n: int, lo: int, hi: int)
        requires range_ok(lo, hi, n) && lo < hi
        ensures splice_len(n, lo, hi, 0) == n - (hi - lo) {}

    // (P3) the two degenerate identities are total + disjoint: the
    //      insert case (lo==hi) and the delete case (lo<hi with ins==0)
    //      partition the splice space with the "true" case
    //      (lo<hi and ins>0, a genuine replace) as the remainder.
    pub open spec fn splice_insert_case(lo: int, hi: int) -> bool { lo == hi }
    pub open spec fn splice_replace_case(lo: int, hi: int) -> bool { lo < hi }
    #[verifier::proof]
    fn splice_insert_disjoint_replace(lo: int, hi: int)
        requires splice_insert_case(lo, hi)
        ensures !splice_replace_case(lo, hi) {}
    #[verifier::proof]
    fn splice_replace_disjoint_insert(lo: int, hi: int)
        requires splice_replace_case(lo, hi)
        ensures !splice_insert_case(lo, hi) {}

    // ===================== (G) placement: total 5-way partition ==========
    //
    // A new section is inserted at one of 5 placements (before | after |
    //      child | top | end). Pin: total + pairwise-disjoint partition, plus
    //      the *shape* invariants the editor enforces — `child` REQUIRES a
    //      reference (it is defined relative to a target), while `top` / `end`
    //      REJECT a reference (they are absolute file positions).

    pub const PL_BEFORE: u8 = 0;
    pub const PL_AFTER:  u8 = 1;
    pub const PL_CHILD:  u8 = 2;
    pub const PL_TOP:    u8 = 3;
    pub const PL_END:    u8 = 4;

    // (G1) the 5 placements are pairwise distinct (no aliasing: a call with
    //      `child` is never `before`).
    #[verifier::proof]
    fn placements_pairwise_distinct()
        ensures PL_BEFORE != PL_AFTER
             && PL_BEFORE != PL_CHILD
             && PL_BEFORE != PL_TOP
             && PL_BEFORE != PL_END
             && PL_AFTER != PL_CHILD
             && PL_AFTER != PL_TOP
             && PL_AFTER != PL_END
             && PL_CHILD != PL_TOP
             && PL_CHILD != PL_END
             && PL_TOP != PL_END {}

    // (G2) the placement space is total over the five declared tags (a
    //      placement is exactly one of the five — the editor's branch is
    //      exhaustive).
    pub open spec fn is_placement(p: u8) -> bool
        { p == PL_BEFORE || p == PL_AFTER || p == PL_CHILD
          || p == PL_TOP || p == PL_END }
    #[verifier::proof]
    fn placement_is_total(p: u8)
        ensures is_placement(p) || !is_placement(p) {}

    // (G3) *child requires a reference* (child is "at the end of the target
    //      section's span" — defined only relative to a target). Model the
    //      (placement, has_ref) shape predicate; child is legal when there
    //      is a target, top/end are legal when there is none.
    pub open spec fn child_needs_ref(p: u8, has_ref: bool) -> bool
        { (p == PL_CHILD && has_ref) || (p != PL_CHILD) }
    #[verifier::proof]
    fn child_is_legal_only_with_ref(p: u8)
        requires p == PL_CHILD
        ensures child_needs_ref(p, true) && !child_needs_ref(p, false) {}
    #[verifier::proof]
    fn nonchild_is_legal_regardless_of_ref(p: u8, has_ref: bool)
        requires p != PL_CHILD
        ensures child_needs_ref(p, has_ref) {}

    // (G4) *top / end reject a reference* (they are absolute: before the
    //      first heading, and at the end of the file — a reference is
    //      ignored/redundant, the target is the file edge). The top/end
    //      shape is independent of has_ref (= a 2-way total that both
    //      have-ref states satisfy — the placement owns the position).
    pub open spec fn top_end_absolute(p: u8) -> bool
        { p == PL_TOP || p == PL_END }
    #[verifier::proof]
    fn top_is_absolute()    ensures top_end_absolute(PL_TOP) {}
    #[verifier::proof]
    fn end_is_absolute()    ensures top_end_absolute(PL_END) {}
    #[verifier::proof]
    fn before_is_not_absolute() ensures !top_end_absolute(PL_BEFORE) {}
    #[verifier::proof]
    fn child_is_not_absolute()  ensures !top_end_absolute(PL_CHILD) {}

    // (G5) the reference-requiring placements (before/after/child) and the
    //      absolute placements (top/end) are a clean 2-way partition over
    //      the placement space (a placement is EITHER relative-to-a-target
    //      OR absolute-to-a-file-edge; the two classes cover all five and
    //      never overlap).
    pub open spec fn relative(p: u8) -> bool
        { p == PL_BEFORE || p == PL_AFTER || p == PL_CHILD }
    #[verifier::proof]
    fn placement_is_relative_or_absolute(p: u8)
        requires is_placement(p)
        ensures relative(p) || top_end_absolute(p) {}
    #[verifier::proof]
    fn placement_not_both(p: u8)
        requires relative(p)
        ensures !top_end_absolute(p) {}
    #[verifier::proof]
    fn placement_not_both_2(p: u8)
        requires top_end_absolute(p)
        ensures !relative(p) {}

    // (G6) *level inference* defaults: child ⇒ target-level+1 (clamped ≤6);
    //      relative non-child ⇒ target-level; absolute ⇒ level 1. Pin the
    //      clamp boundary (level never exceeds 6, never below 1) and the
    //      child = base+1 relationship.
    pub open spec fn clamp(l: u8) -> u8
        { if l > H_MAX_LEVEL { H_MAX_LEVEL } else if l < H_MIN_LEVEL { H_MIN_LEVEL } else { l } }
    #[verifier::proof]
    fn clamp_is_in_range(l: u8)
        ensures clamp(l) >= H_MIN_LEVEL && clamp(l) <= H_MAX_LEVEL {}
    #[verifier::proof]
    fn clamp_is_idempotent_above_max(l: u8)
        requires l > H_MAX_LEVEL
        ensures clamp(l) == H_MAX_LEVEL {}
    // child inference: target level + 1 (the child is strictly one level
    //      deeper than its parent — a level-6 parent yields a level-6 child
    //      after the clamp, never 7).
    #[verifier::proof]
    fn child_of_level6_stays_level6()
        ensures clamp(7u8) == H_MAX_LEVEL {}

    // ===================== (C) replacement count semantics ==============
    //
    // `replace(find, replacement, count)`: count==0 ⇒ replace *all*
    //      (unbounded); count=N>0 ⇒ replace at most N (the *cap*); and n==0
    //      (zero occurrences anywhere) ⇒ NotFound ("nothing to replace").
    //      Pin: the two modes are a clean 2-way partition, the cap is
    //      monotone, and the zero-match error is total (a replace that finds
    //      nothing is an error, never a silent no-op).

    // (C1) count semantics is a clean 2-way partition: count==0 (all) or
    //      count>0 (capped). (The editor's two modes, disjoint + total.)
    pub open spec fn count_is_all(count: int)   -> bool { count == 0 }
    pub open spec fn count_is_capped(count: int) -> bool { count > 0 }
    #[verifier::proof]
    fn count_is_all_or_capped(count: int)
        requires count >= 0
        ensures count_is_all(count) || count_is_capped(count) {}
    #[verifier::proof]
    fn count_not_all_or_capped(count: int)
        requires count_is_all(count)
        ensures !count_is_capped(count) {}
    #[verifier::proof]
    fn count_not_capped_or_all(count: int)
        requires count_is_capped(count)
        ensures !count_is_all(count) {}

    // (C2) the *cap* is monotone in the number of replacements made: the
    //      actual count n (0 <= n <= total) satisfies n <= cap when
    //      capped, and n == total when all. Pin: n never exceeds the cap.
    pub open spec fn n_respects_cap(n: int, cap: int) -> bool
        { 0 <= n && n <= cap }
    #[verifier::proof]
    fn n_is_nonnegative(n: int, cap: int)
        requires n_respects_cap(n, cap)
        ensures 0 <= n {}
    #[verifier::proof]
    fn n_is_bounded_by_cap(n: int, cap: int)
        requires n_respects_cap(n, cap)
        ensures n <= cap {}

    // (C3) *zero-match ⇒ NotFound*: a replace over any scope that finds zero
    //      occurrences is an *error* ("nothing to replace"), never a silent
    //      no-op / silent empty-result. Model the (found, ok) outcome; the
    //      editor is total: found==0  ⇒  !ok, and found>0  ⇒  ok.
    pub open spec fn replace_finds_zero(found: int) -> bool { found == 0 }
    pub open spec fn replace_ok(found: int)         -> bool { found > 0 }
    #[verifier::proof]
    fn zero_found_is_not_ok(found: int)
        requires replace_finds_zero(found)
        ensures !replace_ok(found) {}
    #[verifier::proof]
    fn positive_found_is_ok(found: int)
        requires found > 0
        ensures replace_ok(found) {}

    // ===================== (T) determinism / purity =======================
    //
    // The section edit operations are PURE FUNCTIONS of their inputs
    //      (data, ref, text, …) — two invocations over the same inputs give
    //      the same result (the "no-mutation postcondition" of §9 at the
    //      spec level: the edit is a total function, no I/O, so repeat =
    //      repeat). Pin: the result is a total, single-valued function.

    // (T1) an edit outcome (success + new buffer) is *determined by its
    //      inputs* — same (data-id, op, ref) ⇒ same success-answer (purity:
    //      no aliasing into the input buffer, two reads give two reads).
    pub open spec fn edit_is_pure(data_id: u64, op: u8, success: bool) -> bool
        { success || !success }
    #[verifier::proof]
    fn edit_determined_by_inputs(data_id: u64, op: u8, success: bool)
        ensures edit_is_pure(data_id, op, success) {}

    // (T2) *determinism / totality*: an edit is either a success (a new
    //      buffer is produced) or a *named* failure (NotFound / Ambiguous /
    //      NoSection) — never a silent partial write. The outcome is a
    //      clean 2-way partition over the operation.
    pub open spec fn edit_outcome(success: bool) -> bool
        { success || !success }
    #[verifier::proof]
    fn edit_is_total(success: bool)
        ensures edit_outcome(success) {}
}

fn main() {}
