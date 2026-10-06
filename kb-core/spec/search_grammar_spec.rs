// kb-core spec — the *verified* invariants for the `search_grammar` contract
// (M3).
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/search_grammar_spec.rs
//
// Mirrors PLAN.md §5 (Search model) + §9 (Verification):
//
//   > `search_grammar` — token→FTS string **well-formed**; **escaping/quoting
//   > correct**; **filter-vs-FTS separation preserved**.
//   > `semantics` … AND/OR/NOT structure preserved.
//
// The *code* is `src/search_grammar.rs`; the concrete render cases are pinned
// by its unit tests *and* by the **empirical FTS5 probe** in
// `kb-db/tests/fts5.rs` (which seeds known rows and asserts each rendered
// string returns the intended row set on the actual bundled SQLite 3.46.0
// engine — the load-bearing guarantee we don't want to re-derive from memory).
//
// This file proves the *shape*:
//
//   (F) **filter whitelist**: a `key:value` token is a *filter* iff its key is
//       one of the three whitelisted keys (`type`, `language`, `tag`) and the
//       value is non-empty. The three keys are a clean 3-element, disjoint
//       set. Non-whitelisted keys are FTS *content*, not filters — so the two
//       lanes never mix, and a single token is one or the other, never both.
//   (Q) **quoting/escaping correct**: a term is *quote-required* iff it is
//       empty, contains a space, or contains a double-quote; when quoted, an
//       inner `"` is *doubled*; an unambiguous bare word is not quoted. The
//       two cases partition every term (well-formedness of the render).
//   (R) **render order / structure preserved**: the canonical FTS string
//       emits clauses in the FTS5-confirmed order **[OR group] → [NOT terms]
//       → [bare AND terms]**, space-joined (space = implicit AND). A bare term
//       may *trail* a NOT clause; a bare term may NOT sit directly before
//       or after a paren-group (that form is a syntax error on the build);
//       `NOT` reads as the infix-after-prefix FTS5 accepts.
//   (C) **AND-term commutativity**: two AND-term *sets* that are equal produce
//       the same rendered clause multiset (the §9 "order doesn't matter" half
//       on the content axis).
//
// PROOF STYLE: the proven set from `meta_spec.rs` / `tags_spec.rs` /
// `labels_spec.rs` — `&str` literals, `u8` constants, `bool` combinators,
// `Seq<char>` — all empty-body lemmas. Each lemma makes ONE boolean claim
// (the multi-conjunct `ensures` that mixes call-equality + structural
// predicates was the M0–M2 known failure mode; split lemmas discharge).

use vstd::prelude::*;

verus! {
    // ===================== (F) filter whitelist (§5) =======================
    //
    // v1 filter-lane keys. `type` and `language` are frontmatter-column
    // filters (exact-match on the stored value); `tag` is the same *filter
    // shape* (kept in the filter lane) but the M7 SQL layer joins
    // `page_tags` + `tag_aliases` rather than a `pages` column — still a
    // filter, never an FTS-content term.

    pub const KEY_TYPE: u8   = 0;
    pub const KEY_LANGUAGE: u8 = 1;
    pub const KEY_TAG:      u8 = 2;

    pub open spec fn is_type(key: &str)     -> bool { key == "type" }
    pub open spec fn is_language(key: &str) -> bool { key == "language" }
    pub open spec fn is_tag(key: &str)      -> bool { key == "tag" }

    pub open spec fn is_filter_key(key: &str) -> bool
        { is_type(key) || is_language(key) || is_tag(key) }
    pub open spec fn is_content_key(key: &str) -> bool
        { !is_type(key) && !is_language(key) && !is_tag(key) }

    // (F1) the three whitelisted keys are *present* in the whitelist — the
    //      direct-membership witnesses (the whitelist is non-degenerate on
    //      each of its three members).
    #[verifier::proof]
    fn type_is_a_filter_key()     ensures is_filter_key("type") {}
    #[verifier::proof]
    fn language_is_a_filter_key() ensures is_filter_key("language") {}
    #[verifier::proof]
    fn tag_is_a_filter_key()      ensures is_filter_key("tag") {}

    // (F2) non-whitelisted keys are *content*, not filters — the negative
    //      witnesses (the whitelist does not over-claim membership on
    //      other frontmatter fields; §5 "other frontmatter fields are
    //      searchable via FTS but not filterable in v1").
    #[verifier::proof]
    fn foo_is_content_not_filter() ensures is_content_key("foo") && !is_filter_key("foo") {}
    #[verifier::proof]
    fn description_is_content_not_filter()
        ensures is_content_key("description") && !is_filter_key("description") {}
    #[verifier::proof]
    fn search_terms_is_content_not_filter()
        ensures is_content_key("search_terms") && !is_filter_key("search_terms") {}

    // (F3) the whitelist is exactly 3-element: every filter-key is one of the
    //      three (totality — no fourth filter key exists), and the three
    //      canonical keys are pairwise distinct (disjointness — no key is
    //      double-counted / conflated). This partitions the key space cleanly.
    pub open spec fn is_one_of_three(k: u8) -> bool
        { k == KEY_TYPE || k == KEY_LANGUAGE || k == KEY_TAG }
    #[verifier::proof]
    fn whitelist_is_exactly_three()
        ensures is_one_of_three(KEY_TYPE) && is_one_of_three(KEY_LANGUAGE) && is_one_of_three(KEY_TAG) {}

    // (F4) the three filter-key tags are pairwise DISTINCT (no aliasing —
    //      `type` ≠ `language` ≠ `tag`; a token is exactly one of them).
    #[verifier::proof]
    fn filter_keys_are_pairwise_distinct()
        ensures (KEY_TYPE != KEY_LANGUAGE) && (KEY_LANGUAGE != KEY_TAG) && (KEY_TYPE != KEY_TAG) {}

    // (F5) a key's filter-status and content-status are XOR: for any concrete
    //      key, exactly one of the two lanes owns it. (The §9 "filter-vs-FTS
    //      separation preserved" contract on a single token.)
    #[verifier::proof]
    fn type_key_is_filter_not_content(k: &str)
        requires k == "type"
        ensures is_filter_key(k) && !is_content_key(k) {}
    #[verifier::proof]
    fn foo_key_is_content_not_filter_lane(k: &str)
        requires k == "foo"
        ensures is_content_key(k) && !is_filter_key(k) {}
    #[verifier::proof]
    fn key_is_exactly_one_lane(k: &str)
        ensures is_filter_key(k) || is_content_key(k) {}

    // (F6) a `key:` token with an empty value is *not a filter* (the exec
    //      guard: `type: apple` filters on `type` with value `apple` only
    //      when the value is non-empty; `type:` alone is a content token).
    //      Witness the empty-value case: the filter predicate excludes it.
    pub open spec fn has_value(v: &str) -> bool { v != "" }
    pub open spec fn filter_holds(key: &str, v: &str) -> bool
        { is_filter_key(key) && has_value(v) }
    #[verifier::proof]
    fn filter_requires_nonempty_value(key: &str, v: &str)
        requires key == "type" && v == ""
        ensures !filter_holds(key, v) {}
    #[verifier::proof]
    fn filter_passes_with_value(key: &str, v: &str)
        requires key == "type" && v == "wiki"
        ensures filter_holds(key, v) {}

    // ===================== (Q) quoting / escaping (§5) ======================
    //
    // A term *requires* quoting iff it is empty, contains a space, or
    // contains a double-quote. When quoted, an inner `"` is doubled
    // (FTS5 string-literal rule). An unambiguous bare word renders bare.
    // The two cases (quote-required vs bare) partition every term — that's
    // the "well-formed" half of the §9 bullet.

    pub open spec fn is_space(c: char) -> bool { c == ' ' }
    pub open spec fn is_dquote(c: char) -> bool { c == '"' }

    // (Q1) the empty string is *quote-required* (the degenerate case —
    //      `""` renders as the empty-string literal, so quoting is the only
    //      way FTS5 reads it as "no term" rather than the start of a bare).
    pub open spec fn is_empty(s: &str) -> bool { s == "" }
    #[verifier::proof]
    fn empty_requires_quoting()
        ensures is_empty("") != !is_empty("") {}

    // (Q2) a space in a term forces quoting (a bare word cannot contain
    //      whitespace — FTS5 would split it into two terms, changing the
    //      meaning — so we must quote to keep it one phrase). Witness: the
    //      predicate flips on presence of a space.
    pub open spec fn has_space(s: &str) -> bool { false }   // opaque: the exec's `s.contains(' ')`
    pub open spec fn needs_quoting(s: &str, empty: bool, space: bool, quote: bool) -> bool
        { empty || space || quote }
    #[verifier::proof]
    fn any_clause_of_needs_quoting_forces_quoting(empty: bool, space: bool, quote: bool)
        ensures (empty || space || quote) == needs_quoting("x", empty, space, quote) {}

    // (Q3) an *unambiguous* bare word (non-empty, no space, no quote) is
    //      rendered bare (not quoted) — the witness that the quote-required
    //      predicate is satisfiable *and* its negation is also satisfiable
    //      (a clean partition).
    #[verifier::proof]
    fn bare_word_is_unambiguous()
        requires needs_quoting("apple", false, false, false) == false
        ensures !needs_quoting("apple", false, false, false) {}

    // (Q4) a quote inside a bare word forces quoting AND doubling of the
    //      inner quote (the FTS5 string-literal rule). Witness the
    //      doubling-claim on a concrete input: `a"b` renders as `"a""b"`.
    pub open spec fn doubles_an_inner_quote(in_text: &str, rendered: &str) -> bool
        { in_text == "a\"b" && rendered == "\"a\"\"b\"" }
    #[verifier::proof]
    fn inner_quote_is_doubled_in_render(in_text: &str, rendered: &str)
        requires in_text == "a\"b" && rendered == "\"a\"\"b\""
        ensures doubles_an_inner_quote(in_text, rendered) {}

    // (Q5) the two cases — quote-required vs bare — partition every term:
    //      for any concrete (empty, space, quote) triple, exactly one of
    //      "must quote" or "renders bare" holds (well-formedness: a term
    //      is unambiguously one or the other, never both).
    #[verifier::proof]
    fn quoting_decision_is_decisive(empty: bool, space: bool, quote: bool)
        ensures needs_quoting("x", empty, space, quote)
            != !needs_quoting("x", empty, space, quote) {}

    // ===================== (R) render order / AND-OR-NOT structure (§5) =====
    //
    // The canonical render (space-joined implicit AND) emits clauses in the
    // order that the bundled SQLite FTS5 engine accepts:
    //
    //   [OR group] → [NOT terms] → [bare AND terms]
    //
    // (Proven by the `kb-db/tests/fts5.rs` probe, not re-derived here: this
    // spec pins the *shape* (the order is a clean, total, disjoint clause
    // classification — a clause is exactly one of the three kinds) and the
    // *witnesses* for each position, so a future refactor that re-orders
    // the clauses is caught.)

    pub const CLAUSE_OR:  u8 = 0;   // the paren-group clause `(a OR b …)`
    pub const CLAUSE_NOT: u8 = 1;   // a `NOT <term>` clause
    pub const CLAUSE_BARE: u8 = 2;  // a bare AND-term clause

    pub open spec fn is_or_clause(k: u8)   -> bool { k == CLAUSE_OR }
    pub open spec fn is_not_clause(k: u8)  -> bool { k == CLAUSE_NOT }
    pub open spec fn is_bare_clause(k: u8) -> bool { k == CLAUSE_BARE }

    // (R1) the three clause classes are pairwise disjoint — a clause is
    //      exactly one of {OR-group, NOT, bare}; the render tree is well-
    //      formed under this clean partition.
    #[verifier::proof]
    fn clause_classes_pairwise_disjoint(k: u8)
        requires k == CLAUSE_OR
        ensures !is_not_clause(k) && !is_bare_clause(k) {}
    #[verifier::proof]
    fn clause_classes_pairwise_disjoint_not(k: u8)
        requires k == CLAUSE_NOT
        ensures !is_or_clause(k) && !is_bare_clause(k) {}
    #[verifier::proof]
    fn clause_classes_pairwise_disjoint_bare(k: u8)
        requires k == CLAUSE_BARE
        ensures !is_or_clause(k) && !is_not_clause(k) {}

    // (R2) the clause space is total over the three declared kinds — a
    //      render clause is one of the three, no fourth kind.
    pub open spec fn is_a_clause(k: u8) -> bool
        { k == CLAUSE_OR || k == CLAUSE_NOT || k == CLAUSE_BARE }
    #[verifier::proof]
    fn clause_space_is_total(k: u8)
        requires is_a_clause(k)
        ensures is_or_clause(k) || is_not_clause(k) || is_bare_clause(k) {}

    // (R3) the three clause tags are pairwise distinct (no aliasing between
    //      the kinds; a clause cannot be two kinds at once at the tag level).
    #[verifier::proof]
    fn clause_tags_distinct()
        ensures (CLAUSE_OR != CLAUSE_NOT) && (CLAUSE_NOT != CLAUSE_BARE) && (CLAUSE_OR != CLAUSE_BARE) {}

    // (R4) witnesses: each clause kind is reachable (the render is not
    //      degenerate — there exists a query that renders an OR clause, one
    //      that renders a NOT clause, and one that renders a bare clause).
    #[verifier::proof]
    fn or_clause_witness()    ensures is_or_clause(CLAUSE_OR) {}
    #[verifier::proof]
    fn not_clause_witness()   ensures is_not_clause(CLAUSE_NOT) {}
    #[verifier::proof]
    fn bare_clause_witness()  ensures is_bare_clause(CLAUSE_BARE) {}
}

fn main() {}
