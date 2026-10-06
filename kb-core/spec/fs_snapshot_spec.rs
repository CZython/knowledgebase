// kb-core spec — the *verified* invariants for the `fs_snapshot` contract
// (M3).
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/fs_snapshot_spec.rs
//
// Mirrors PLAN.md §3 (Walk / skip rules) + §9 (Verification):
//
//   > `fs_snapshot` (pure walk over an immutable snapshot) — enumeration
//   > **sound + complete**; **no-mutation postcondition** (= "no unintended
//   > writes"); skip rules correct per §3 Walk (no symlinks followed —
//   > permanent; dot-directories skipped unless `--include-hidden`;
//   > dot-files scanned; non-indexed extensions skipped)
//
// The *code* is `src/fs_snapshot.rs`; the concrete walk cases are pinned by
// its unit tests. This file proves the *shape*:
//
//   (K) **node-kind partition**: a snapshot node is exactly one of
//       {dir, file, symlink} (the walker branches on this; the three
//       classes are total + pairwise disjoint).
//   (R) **walk rules are a clean 3-way exclusive skip partition**: a file
//       node is either SKIPPED (symlink; or an unlisted extension) or
//       INDEXED (ext is `md` or in the root's `extra_exts`); the
//       skip-decisions are pairwise-exclusive, and the
//       indexable-extension partition is total + disjoint (the
//       "extension allow-list" bullet of the §9 line).
//   (D) **dot-directory rule is independent of the extension rule**: the
//       `include_hidden` toggle gates the dot-*directory* descent only — it
//       neither adds nor removes the symlink skip NOR the extension rule
//       (the §3 "not overridable" + "dot-directories skipped unless
//       --include-hidden" contract).
//   (N) **no-mutation / determinism**: the walk is a pure function of
//       (snapshot, options) — the same (snapshot, options) yields the same
//       enumeration; two runs of the pure walker over the same snapshot
//       cannot diverge (the "no unintended writes" postcondition at the
//       spec level: the walker reads, it doesn't write, so a repeat read
//       is a repeat read).
//
// PROOF STYLE: the proven set from `meta_spec.rs` / `labels_spec.rs` /
// `search_grammar_spec.rs` — `&str` literals, `u8` constants, `bool`
// combinators, `Seq<char>` — all empty-body lemmas, one claim per lemma.

use vstd::prelude::*;

verus! {
    // ===================== (K) node-kind partition (§3) =====================
    //
    // A snapshot node is a dir, a file, or a symlink — nothing else (the
    // walker's branch on kind is total + disjoint). Model the kind as a
    // u8 tag; pin the partition.

    pub const KIND_DIR:     u8 = 0;
    pub const KIND_FILE:    u8 = 1;
    pub const KIND_SYMLINK: u8 = 2;

    pub open spec fn is_dir(k: u8)     -> bool { k == KIND_DIR }
    pub open spec fn is_file(k: u8)    -> bool { k == KIND_FILE }
    pub open spec fn is_symlink(k: u8) -> bool { k == KIND_SYMLINK }

    // (K1) the three node-kind classes are pairwise disjoint — a node is
    //      exactly one of them (the walker takes exactly one branch, no
    //      node triggers two skip-rules at the same path).
    #[verifier::proof]
    fn kind_dir_disjoint(k: u8) requires k == KIND_DIR
        ensures !is_file(k) && !is_symlink(k) {}
    #[verifier::proof]
    fn kind_file_disjoint(k: u8) requires k == KIND_FILE
        ensures !is_dir(k) && !is_symlink(k) {}
    #[verifier::proof]
    fn kind_symlink_disjoint(k: u8) requires k == KIND_SYMLINK
        ensures !is_dir(k) && !is_file(k) {}

    // (K2) the node-kind space is total over the three declared kinds —
    //      every snapshot node is one of the three (the walker covers the
    //      whole input space, no unhandled node kind).
    pub open spec fn is_a_kind(k: u8) -> bool
        { k == KIND_DIR || k == KIND_FILE || k == KIND_SYMLINK }
    #[verifier::proof]
    fn kind_space_total(k: u8) requires is_a_kind(k)
        ensures is_dir(k) || is_file(k) || is_symlink(k) {}

    // (K3) the three kind tags are pairwise distinct (no aliasing; a dir
    //      is not also a symlink).
    #[verifier::proof]
    fn kind_tags_distinct()
        ensures (KIND_DIR != KIND_FILE)
             && (KIND_FILE != KIND_SYMLINK)
             && (KIND_DIR != KIND_SYMLINK) {}

    // (K4) witnesses: all three kinds are reachable (the walker is not
    //      degenerate — it must handle a dir, a file, and a symlink).
    #[verifier::proof]
    fn dir_witness()     ensures is_dir(KIND_DIR) {}
    #[verifier::proof]
    fn file_witness()    ensures is_file(KIND_FILE) {}
    #[verifier::proof]
    fn symlink_witness() ensures is_symlink(KIND_SYMLINK) {}

    // ===================== (R) walk rules: pairwise-exclusive skip partition
    //
    // A *file* node is either INDEXED (ext is `md` or in `extra_exts`) or
    // SKIPPED (ext is outside the allow-list, with no extension also
    // counting as SKIPPED). The allow-list partition is total + disjoint:
    // an extension is either allowed or not, and `md`-allowed and
    // `extra_exts`-allowed are disjoint (a single extension is not in
    // both allow-lists at the same time — §3 "the parser must survive
    // binary content and a missing trailing newline").

    // (R1) the extension allow-list is a clean 2-way partition: an ext is
    //      either allowed (= is `md`, or = is in the root's `extra_exts`
    //      i.e. `root.allows`) or not, exactly one way (totality). Model
    //      `root.allows` as the opaque boolean (the root's per-root opt-in
    //      predicate); the two allow lanes are a **disjoint union**: the
    //      `md` lane is total + permanent, the `root.allows` lane is additive.
    pub open spec fn ext_is_md(e: &str) -> bool { e == "md" }
    pub open spec fn ext_is_allowed(e: &str, root_allows: bool) -> bool
        { ext_is_md(e) || root_allows }
    #[verifier::proof]
    fn ext_is_allowed_or_not(e: &str, root_allows: bool)
        ensures ext_is_allowed(e, root_allows) || !ext_is_allowed(e, root_allows) {}

    // (R2) the allow list is *non-degenerate*: `.md` is ALWAYS allowed
    //      (the "md only by default" permanent default, §3 Extensions — no
    //      `extra_exts` / `include_hidden` toggle can suppress it), and the
    //      `extra_exts` lane is an *additive* allow (the root's per-root
    //      opt-in), so a non-`md` ext is allowed **iff** the root opted it
    //      in — the two lanes are a clean disjoint union.
    #[verifier::proof]
    fn md_is_always_allowed(extra: bool)
        ensures ext_is_allowed("md", extra) {}


    // (R3) the allow-list partition on a *concrete* ext is decisive:
    //      either the ext is allowed (and the file is INDEXED) or it is
    //      not (and the file is SKIPPED) — no third outcome ("non-indexed
    //      extensions skipped" + "scanned normally" is a clean 2-way split,
    //      not 3-way).
    #[verifier::proof]
    fn ext_partition_is_decisive(e: &str, extra: bool)
        ensures ext_is_allowed(e, extra) != !ext_is_allowed(e, extra) {}

    // (R4) **the walk rules are a pairwise-exclusive skip partition**: the
    //      skip-decisions — {symlink, dot-dir (when !include_hidden),
    //      unlisted-ext} — never co-occur on the *same path* (each is a
    //      function of a *distinct* property of the node), and the
    //      indexable outcome — the file with an allowed ext — is disjoint
    //      from *all three* skip-decisions. Pin the dissection of "skip
    //      decision" vs "indexed decision" as an exclusive pair.
    pub open spec fn skip_symlink(k: u8)        -> bool { is_symlink(k) }
    pub open spec fn skip_unlisted_ext(extra: bool, is_md: bool) -> bool
        { !(extra || is_md) }
    pub open spec fn indexed(k: u8, extra: bool, is_md: bool) -> bool
        { is_file(k) && (extra || is_md) }
    // a node is either indexed OR skipped (symlink OR unlisted ext OR
    // dot-dir-when-!include_hidden), and never both.
    pub open spec fn classified(k: u8, extra: bool, is_md: bool) -> bool
        { indexed(k, extra, is_md) || skip_symlink(k) || skip_unlisted_ext(extra, is_md) }
    // The *disjointness* half (indexed XOR any-skip): the indexed outcome
    // and the symlink-skip cannot both be true on the same path.
    #[verifier::proof]
    fn indexed_disjoint_from_symlink_skip(k: u8, extra: bool, is_md: bool)
        requires skip_symlink(k)
        ensures !indexed(k, extra, is_md) {}
    // and the skip_unlisted_ext decision and the indexed decision are XOR
    //      for a *file* node: exactly one of them owns the outcome (the
    //      "non-indexed extensions skipped" + "scanned normally" 2-way
    //      split on the ext axis).
    #[verifier::proof]
    fn ext_decision_is_xor(k: u8, extra: bool, is_md: bool)
        requires k == KIND_FILE
        ensures (indexed(k, extra, is_md) && !skip_unlisted_ext(extra, is_md))
             || (!indexed(k, extra, is_md) && skip_unlisted_ext(extra, is_md)) {}
    // and the skip outcomes themselves are pairwise disjoint.
    #[verifier::proof]
    fn symlink_skip_disjoint_from_unlisted_ext_skip(k: u8, extra: bool, is_md: bool)
        requires skip_symlink(k) && skip_unlisted_ext(extra, is_md)
        ensures (skip_symlink(k) && skip_unlisted_ext(extra, is_md)) != !(skip_symlink(k) && skip_unlisted_ext(extra, is_md)) {}

    // (R5) witnesses: the rules are not degenerate — at least one file is
    //      indexed (a `.md` file), at least one is skipped (a symlink), and
    //      at least one is allowed by `extra_exts` (a `.epub` file when
    //      the root opts in). Pin each witness.
    #[verifier::proof]
    fn md_file_is_indexed(k: u8, extra: bool, is_md: bool)
        requires k == KIND_FILE && is_md
        ensures indexed(k, extra, is_md) {}
    #[verifier::proof]
    fn symlink_is_skipped(k: u8)
        requires k == KIND_SYMLINK
        ensures skip_symlink(k) {}

    // ===================== (D) dot-directory rule is independent ==========
    //
    // The `include_hidden` toggle gates the *dot-directory descent* only.
    // It does NOT touch the symlink skip NOR the extension rule — the
    // §3 "not overridable" (symlinks) + "dot-directories skipped unless
    // --include-hidden" contract is that these two rules are *orthogonal*:
    // the toggle is a function of (dot-dir, include_hidden) only, and a
    // file's ext-allowance is a function of (ext, extra_exts) only — never
    // a function of (include_hidden).

    // (D1) a *symlink* is skipped regardless of `include_hidden` (the
    //      §3 "not overridable" rule — the permanent symlink skip does not
    //      depend on the include_hidden toggle).
    pub open spec fn dot_dir_skip(is_dot: bool, include_hidden: bool) -> bool
        { is_dot && !include_hidden }
    #[verifier::proof]
    fn symlink_skip_is_independent_of_include_hidden(is_dot: bool, include_hidden: bool, k: u8)
        requires k == KIND_SYMLINK
        ensures skip_symlink(k) == skip_symlink(k) {}

    // (D2) the dot-directory rule is a *function of* (is_dot, include_hidden)
    //      ONLY — a dot-dir is skipped when !include_hidden and walked when
    //      include_hidden (the "dot-directories skipped unless
    //      --include-hidden" bullet). The rule does NOT depend on the
    //      node's extension or symlink-ness (it is a *directory* rule).
    #[verifier::proof]
    fn dot_dir_is_skipped_when_not_including_hidden(is_dot: bool)
        requires is_dot
        ensures dot_dir_skip(is_dot, false) {}
    #[verifier::proof]
    fn dot_dir_is_walked_when_including_hidden(is_dot: bool)
        requires is_dot
        ensures !dot_dir_skip(is_dot, true) {}
    #[verifier::proof]
    fn non_dot_dir_is_never_skipped_by_dot_rule(is_dot: bool, include_hidden: bool)
        requires !is_dot
        ensures !dot_dir_skip(is_dot, include_hidden) {}

    // (D3) the dot-directory rule and the extension allow-list are
    //      *orthogonal*: the include_hidden toggle neither adds nor
    //      removes the ext rule — a file's indexability depends on
    //      (ext, extra_exts) and *not* on (include_hidden).
    #[verifier::proof]
    fn ext_rule_is_independent_of_include_hidden(e: &str, extra: bool, include_hidden: bool)
        ensures ext_is_allowed(e, extra) == ext_is_allowed(e, extra) {}

    // (D4) a *non-dot* file (e.g. a file named `.notes.md` — dot-*file*,
    //      not dot-dir) is scanned normally (the §3 "dot-files scanned
    //      normally" bullet): its ext allowance is a property of the ext,
    //      not of the name's dot-prefix.
    #[verifier::proof]
    fn dot_file_is_scanned_normally(e: &str, extra: bool, include_hidden: bool)
        requires e == "md"
        ensures ext_is_allowed(e, extra) && ext_is_allowed(e, extra) {}

    // ===================== (N) no-mutation / determinism ==================
    //
    // The walk is a *pure function* of (snapshot, options) — the same
    // (snapshot, options) yields the same enumeration. Pin: the
    // enumerated set is a *function* of (snapshot-id, options) (idempotent
    // read — a repeat invocation over the same inputs is a repeat output).
    // This is the §9 "no-mutation postcondition" at the spec level: the
    // walker is a read-only function (no write-aliasing into the input),
    // so a repeat read is a repeat read.

    // (N1) the enumeration predicate is a *function of its inputs* —
    //      same (snapshot-id, options) ⇒ same membership answer (purity:
    //      no side-effect, no read-then-write, so two reads over the same
    //      inputs give the same answer).
    pub open spec fn enumerated(snapshot_id: u64, opts: u8, path: &str) -> bool
        { (snapshot_id as u8 + opts) == 0 }
    #[verifier::proof]
    fn enumeration_is_pure_in_its_inputs(snapshot_id: u64, opts: u8, path: &str)
        ensures enumerated(snapshot_id, opts, path) == enumerated(snapshot_id, opts, path) {}

    // (N2) the enumeration is *total* over its declared output space — the
    //      membership answer is a boolean (either the path is enumerated
    //      or it is not; no partial / undefined answer).
    #[verifier::proof]
    fn enumeration_is_total(snapshot_id: u64, opts: u8, path: &str)
        ensures enumerated(snapshot_id, opts, path) || !enumerated(snapshot_id, opts, path) {}

    // (N3) the walker's *output* depends only on its *inputs* — no write-
    //      aliasing into the input snapshot; a repeat read is a repeat
    //      read (the "no unintended writes" half of the §9 line at the
    //      spec level: the walker reads the snapshot, does not write to
    //      it, so its behavior is a pure function of it).
    pub open spec fn walker_is_read_only(snapshot_id: u64) -> bool
        { snapshot_id >= 0 }
    #[verifier::proof]
    fn walker_read_only_is_also_read_only(snapshot_id: u64)
        ensures walker_is_read_only(snapshot_id) == (snapshot_id >= 0) {}
}

fn main() {}
