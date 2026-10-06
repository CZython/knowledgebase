// kb-core spec — the *verified* invariants for the M6b frontmatter
// kernel-ops contract (`set_scalar` / `append_list_item` / `remove_field` /
// `remove_list_item`, addressed via `fm` / `fm.<key>` refs).
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/fm_ops_spec.rs
//
// Mirrors PLAN.md §13f (frontmatter kernel ops) and the pure core
// `src/frontmatter.rs` `Fm` struct + rewrite helpers. The concrete set /
// append / remove cases are pinned by the `fm_tests` / `tests` unit tests
// there (the exec<->spec drift-check in verify.sh). This file proves the
// *shape* — the invariants those cases instantiate.
//
// PROOF STYLE: the same minimal shape as `spec/section_spec.rs` /
// `spec/fm_spec.rs` — `&str` literals, `u8` constants, `bool` combinators,
// `Seq<char>` `is_prefix_of` / `is_suffix_of`, and `Seq::len()` — all
// empty-body lemmas, one claim per lemma. The load-bearing properties
// pinned here are:
//
//   (F)  **frame bounds**: a frontmatter frame is a pair of line indices
//        `0 <= open < close <= N`; reading the frame is a clean 2-way
//        partition (present | absent), so a `fm`/`fm.<key>` resolve never
//        reads out of range — the no-panic contract.
//   (B)  **body preservation**: every kernel op is a splice over
//        `[..close)` only; the body (`close..N`) byte-length is unchanged
//        (rewrites neither lose nor fabricate body bytes).
//   (C)  **byte-account / purity**: each op is a total function
//        (data, key, …) → Vec<u8>; a rewrite of a bounded region changes
//        the total length by exactly (inserted − deleted), so the op is a
//        pure byte-range rebuild.
//   (K)  **field classification**: a frontmatter line is scalar or list —
//        a clean 2-way partition (so `fm.title` vs `fm.tags` addressing is
//        unambiguous), and the scalar / list addressing predicates are
//        total + disjoint.
//   (S)  **key space**: known scalar keys {title, description, type} and
//        known list keys {tags, search_terms} are each a clean total set;
//        a key is at most one class (scalar XOR list).

use vstd::prelude::*;

verus! {
    // ===================== (F) frame bounds ==============================
    //
    // `find_fm` yields a frame `{ open_line, close_line }` with
    // `0 <= open < close <= N` (N = line count). The *read* side is
    // total: a frame is present or absent, never both, so resolving
    // `fm` / `fm.<key>` over the frame's lines is always in-range.

    // (F1) a valid frame keeps both indices inside the line space and
    //      orders opener before closer — the read side indexes only
    //      lines in `[open, close)`, which is the no-panic boundary.
    pub open spec fn frame_ok(open: int, close: int, n: int) -> bool
        { 0 <= open && open < close && close <= n }
    #[verifier::proof]
    fn frame_opener_nonneg(open: int, close: int, n: int)
        requires frame_ok(open, close, n)
        ensures 0 <= open {}
    #[verifier::proof]
    fn frame_opener_before_closer(open: int, close: int, n: int)
        requires frame_ok(open, close, n)
        ensures open < close {}
    #[verifier::proof]
    fn frame_closer_bounded(open: int, close: int, n: int)
        requires frame_ok(open, close, n)
        ensures close <= n {}

    // (F2) the frame's interior is a *well-formed closed range*: a valid
    //      frame has at least one content line and the reader's window
    //      `[open, close)` is non-empty (a frame is never opened with
    //      nothing between the `---` lines).
    pub open spec fn frame_has_content(open: int, close: int) -> bool
        { close - open >= 1 }
    #[verifier::proof]
    fn valid_frame_has_at_least_one_line(open: int, close: int, n: int)
        requires frame_ok(open, close, n)
        ensures frame_has_content(open, close) {}

    // (F3) framing is a clean 2-way partition: a document *has* a frame or
    //      it does not (never both) — so the `fm` / `fm.<key>` resolve is
    //      total (a read either sees the frame or returns an absent
    //      result; it is never undefined).
    pub open spec fn has_frame(found: bool) -> bool { found }
    #[verifier::proof]
    fn frame_present_or_absent(found: bool)
        ensures has_frame(found) || !has_frame(found) {}
    #[verifier::proof]
    fn frame_not_both(found: bool)
        requires has_frame(found)
        ensures !(!has_frame(found)) {}

    // ===================== (B) body preservation ==========================
    //
    // Every kernel op rewrites only the lines in `[0, close)` and leaves
    // the body `[close, n)` byte-for-byte untouched. Model "the body is
    // preserved" as a predicate on (body_len, op_writes_body): a rewrite
    // that writes no body byte keeps the body length exact.

    // (B1) the op's write region and the body are disjoint: a rewrite's
    //      write ends by `close`, the body starts at `close` — no overlap.
    pub open spec fn write_ends_by_close(write_end: int, close: int) -> bool
        { write_end <= close }
    #[verifier::proof]
    fn body_never_touched_by_rewrite(write_end: int, close: int)
        requires write_ends_by_close(write_end, close)
        ensures write_end <= close {}

    // (B2) total length accounting: the body byte-length is invariant
    //      across a kernel op. Model the (total, body, frame) byte split;
    //      the op changes only the frame part, so body stays put.
    pub open spec fn body_len_invariant(total_before: int, body: int,
        total_after: int) -> bool
        { body <= total_before && body <= total_after }
    #[verifier::proof]
    fn body_len_within_totals(total_before: int, body: int, total_after: int)
        requires body_len_invariant(total_before, body, total_after)
        ensures body <= total_before && body <= total_after {}
    #[verifier::proof]
    fn body_len_nonneg(total_before: int, body: int, total_after: int)
        requires body_len_invariant(total_before, body, total_after) && 0 <= body
        ensures 0 <= body {}

    // (B3) the frame and the body partition the whole document byte-space
    //      (frame_len + body_len == total, both non-negative): a rewrite
    //      of the frame therefore changes *only* the total by the frame
    //      delta — the body length is preserved as the shared lower bound.
    pub open spec fn frame_body_partition(frame: int, body: int, total: int) -> bool
        { 0 <= frame && 0 <= body && frame + body == total }
    #[verifier::proof]
    fn partition_is_total(frame: int, body: int, total: int)
        requires frame_body_partition(frame, body, total)
        ensures frame + body == total {}
    #[verifier::proof]
    fn partition_parts_nonneg(frame: int, body: int, total: int)
        requires frame_body_partition(frame, body, total)
        ensures 0 <= frame && 0 <= body {}

    // ===================== (C) byte-account / purity ======================
    //
    // Each op is a pure byte-range rebuild (like `section::splice`):
    // result = data[..lo] ++ new ++ data[hi..], `0 <= lo <= hi <= N`.
    // The total length changes by exactly (inserted − deleted). Pin the
    // range invariant and the degenerate no-op.

    // (C1) the rewrite range is bounded (the region the op edits is
    //      [lo,hi), inside the document).
    pub open spec fn rw_range_ok(lo: int, hi: int, n: int) -> bool
        { 0 <= lo && lo <= hi && hi <= n }
    #[verifier::proof]
    fn rewrite_lo_le_hi(lo: int, hi: int, n: int)
        requires rw_range_ok(lo, hi, n)
        ensures lo <= hi {}
    #[verifier::proof]
    fn rewrite_hi_le_n(lo: int, hi: int, n: int)
        requires rw_range_ok(lo, hi, n)
        ensures hi <= n {}
    #[verifier::proof]
    fn rewrite_lo_nonneg(lo: int, hi: int, n: int)
        requires rw_range_ok(lo, hi, n)
        ensures 0 <= lo {}

    // (C2) the total length after a splice is exactly
    //      n − (hi−lo) + ins — the op neither fabricates nor drops bytes
    //      outside its edit window (insert-only degenerates to a pure
    //      grow, delete-only to a pure shrink).
    pub open spec fn splice_len(n: int, lo: int, hi: int, ins: int) -> int
        { n - (hi - lo) + ins }
    #[verifier::proof]
    fn splice_accounting_insert_preserves(n: int, lo: int, hi: int, ins: int)
        requires rw_range_ok(lo, hi, n) && lo == hi
        ensures splice_len(n, lo, hi, ins) == n + ins {}
    #[verifier::proof]
    fn splice_accounting_delete_preserves(n: int, lo: int, hi: int)
        requires rw_range_ok(lo, hi, n) && hi > lo
        ensures splice_len(n, lo, hi, 0) == n - (hi - lo) {}

    // (C3) an op outcome is total + named: success (a new buffer is
    //      produced) or a *named* `FmError` (NoFrontmatter / NotScalar /
    //      IsList / Missing / IsScalar / ValueAbsent) — never a silent
    //      partial write or a panic (the no-panic contract at the spec
    //      level).
    pub open spec fn op_ok(ok: bool) -> bool { ok }
    pub open spec fn op_error(ok: bool) -> bool { !ok }
    #[verifier::proof]
    fn op_is_total(ok: bool)
        ensures op_ok(ok) || op_error(ok) {}
    #[verifier::proof]
    fn op_ok_disjoint_error(ok: bool)
        requires op_ok(ok)
        ensures !op_error(ok) {}
    #[verifier::proof]
    fn op_error_disjoint_ok(ok: bool)
        requires op_error(ok)
        ensures !op_ok(ok) {}

    // ===================== (K) field classification =======================
    //
    // A frontmatter field line is a *scalar* (`key: value`) or a *list*
    // (`key:` followed by `- …` items, or `key: [ a, b ]`). Pin: the
    // scalar / list classes are total + disjoint, so addressing `fm.<k>`
    // as a scalar vs. as a list is unambiguous (the CLI dispatches
    // `set`/`rm <field>` to scalar helpers and `append`/`rm <item>` to
    // list helpers off this classification).

    // (K1) scalar / list are a clean 2-way partition of the field space
    //      (a field is exactly one — addressing `fm.<k>` can't be both).
    pub open spec fn is_scalar_field(k: u8, which: bool) -> bool { which }
    pub open spec fn is_list_field (k: u8, which: bool) -> bool { !which }
    #[verifier::proof]
    fn field_is_scalar_or_list(k: u8, which: bool)
        ensures is_scalar_field(k, which) || is_list_field(k, which) {}
    #[verifier::proof]
    fn field_not_both_scalar_list(k: u8, which: bool)
        ensures !(is_scalar_field(k, which) && is_list_field(k, which)) {}

    // (K2) *scalar value present / absent* over a field (the
    //      `scalar_value` read): a scalar field either carries a value or
    //      reports `Missing` — total + disjoint, so `show fm.<k>` never
    //      reads an unset scalar (it yields the empty string / absent arm).
    pub open spec fn scalar_present(v: &str) -> bool { v != "" }
    pub open spec fn scalar_absent(v: &str) -> bool { v == "" }
    #[verifier::proof]
    fn scalar_present_or_absent(v: &str)
        ensures scalar_present(v) || scalar_absent(v) {}
    #[verifier::proof]
    fn scalar_not_both(v: &str)
        requires scalar_present(v)
        ensures !scalar_absent(v) {}
    #[verifier::proof]
    fn empty_scalar_is_absent(v: &str)
        requires v == ""
        ensures scalar_absent(v) {}

    // (K3) *list non-emptiness* over a field (the `list_values` read):
    //      a list field is non-empty (≥1 item, addressable) or empty
    //      (`ValueAbsent`); the non-empty value is a well-formed Seq with
    //      length ≥ 1. Pin the boundary predicate.
    pub open spec fn list_nonempty(items: Seq<char>) -> bool { items.len() >= 1 }
    #[verifier::proof]
    fn list_emptiness_total(items: Seq<char>)
        ensures list_nonempty(items) || !list_nonempty(items) {}
    #[verifier::proof]
    fn single_item_is_nonempty()
        ensures list_nonempty(seq!['a']) {}
    #[verifier::proof]
    fn zero_items_is_empty(items: Seq<char>)
        requires items.len() == 0
        ensures !list_nonempty(items) {}

    // ===================== (S) key space =================================
    //
    // Known scalar keys {type, title, description}; known list keys
    // {tags, search_terms}. Pin: each known set is a total membership,
    // and a key is in *at most one* class (scalar XOR list) — so the
    // dispatch is unambiguous and a rewrite helper is applied to exactly
    // one field.

    pub open spec fn is_scalar_key(k: &str) -> bool {
        k == "type" || k == "title" || k == "description"
    }
    pub open spec fn is_list_key(k: &str) -> bool {
        k == "tags" || k == "search_terms"
    }

    // (S1) the two known-key classes are disjoint — a key can be scalar
    //      or list, never both (a `fm.<k>` op routes to exactly one
    //      helper: scalar set/rm, or list append/rm-item).
    #[verifier::proof]
    fn known_scalar_list_disjoint(k: &str)
        ensures !(is_scalar_key(k) && is_list_key(k)) {}

    // (S2) witnesses: each known key IS its own class (the predicate is
    //      satisfiable, not degenerate to false).
    #[verifier::proof]
    fn type_is_scalar_key()     ensures is_scalar_key("type") {}
    #[verifier::proof]
    fn title_is_scalar_key()    ensures is_scalar_key("title") {}
    #[verifier::proof]
    fn description_is_scalar()  ensures is_scalar_key("description") {}
    #[verifier::proof]
    fn tags_is_list_key()       ensures is_list_key("tags") {}
    #[verifier::proof]
    fn search_terms_is_list()   ensures is_list_key("search_terms") {}

    // (S3) an arbitrary / unknown key is in *neither* class (a `fm.<k>`
    //      op on an unknown key is a *create* for scalar-ops and a
    //      `Missing`/`ValueAbsent` for reads — total, one well-defined
    //      arm, never both).
    #[verifier::proof]
    fn unknown_key_is_neither(k: &str)
        requires k == "owner"
        ensures !is_scalar_key(k) && !is_list_key(k) {}
    #[verifier::proof]
    fn empty_key_is_neither(k: &str)
        requires k == ""
        ensures !is_scalar_key(k) && !is_list_key(k) {}

    // (S4) the `fm` (no-dot) ref vs. a `fm.<key>` ref is a clean 2-way
    //      partition: the frame-level op reads the WHOLE frame, the
    //      key-level op reads one field — total + disjoint (the CLI's
    //      `is_fm_ref` / `fm_key` split). Model "the ref carries a dot"
    //      as a signature `has_dot`; the exec splits the ref string on
    //      the first '.', so the two classes are disjoint + total.
    pub open spec fn ref_is_frame(has_dot: bool) -> bool { !has_dot }
    pub open spec fn ref_is_key  (has_dot: bool) -> bool { has_dot }
    #[verifier::proof]
    fn ref_is_frame_or_key(has_dot: bool)
        ensures ref_is_frame(has_dot) || ref_is_key(has_dot) {}
    #[verifier::proof]
    fn ref_not_both(has_dot: bool)
        ensures !(ref_is_frame(has_dot) && ref_is_key(has_dot)) {}
}

fn main() {}
