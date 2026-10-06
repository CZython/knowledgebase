//! semantics — the *meaning* of a parsed query: the `search_terms` gate
//! (the "reverse whitelist") and the commutativity / structure invariants of
//! the filter + FTS-content lanes (M3).
//!
//! §5 (Search model — `search_terms` gate) + §9 (Verification):
//!
//! > `semantics` — **gate = gateless ∨ ∃ whole gate entry occurring in query
//! > text (case-insensitive)**; **filter commutativity** (= "order doesn't
//! > matter"); AND/OR/NOT structure preserved.
//!
//! ## The gate (the "reverse whitelist")
//!
//! A page's `search_terms` are *secret unlock phrases*: a page **with**
//! `search_terms` is hidden from general search and is **unlocked** only when
//! the **entire** gate entry occurs as a **contiguous span, case-insensitive,
//! somewhere** in the query's FTS content text. A page **without** gate
//! entries (gateless) is always visible (given a content match).
//!
//! So a page is *visible to a query* iff:
//!
//! ```text
//! visible(page, query)  =  gateless(page)
//!                       ∨  ∃ gate-entry g of page : occurs_in_text(g, query_text)
//! ```
//!
//! where `occurs_in_text(g, q)  =  g ≠ "" ∧ g@ is a contiguous substring of
//! the lower-cased q` (the whole entry, not a token — so `"AI deep"` does NOT
//! unlock a gate on `"deep learning"`, but `"AI deep learning"` does; a lone
//! word `mTLS` is unlocked by any query containing the span `mtls`).
//!
//! The concrete exec comparison is `q.to_lowercase().contains(g.to_lowercase())`
//! over the **FTS content text** (the gate matches against the *bare/quoted
//! terms* of the query, **not** the `key:value` filters — §5).
//!
//! ## Filter commutativity
//!
//! The grammatical filter lane (`type:…`, `language:…`, `tag:…`) and the AND
//! content lane are **commutative**: reordering the filters does not change
//! the query, and adding the same filter set in any order is the same query.
//! We prove the *set-theoretic* commutativity: for any two filter sets, an
//! AND-ed predicate over them is invariant under reordering (the §9
//! "order doesn't matter" contract).
//!
//! ## AND/OR/NOT structure
//!
//! A parsed query is a fixed structure: a (possibly-empty) set of AND terms,
//! one (possibly-empty) OR group, a (possibly-empty) set of NOT terms. The
//! exec renders `AND-term … AND (a OR b …) AND NOT c …`. We pin the *shape*
//! that a present clause forces a particular term into its lane (an
//! AND-term is an AND-term; an OR term is inside the group; a NOT term is a
//! NOT), and that the lane assignment is a clean, disjoint partition (a
//! term is in at most one lane).
//!
//! Verified: `spec/semantics_spec.rs` proves the gate is a clean
//! "gateless ∨ ∃ entry" disjunction (total + disjoint on the page class),
//! the entry-occurrence predicate is non-degenerate (empty entry is inert;
//! an entry equal to a span is a hit; a proper sub-span is not), filter
//! commutativity, and the AND/OR/NOT lane partition.
//!
//! The concrete boundary cases (a page gated on `"deep learning"` unlocked by
//! `AI deep learning` but not by `AI deep`; a page gated on `mTLS` unlocked
//! by `mtls`; the zero-result case) are pinned by the unit tests below.

/// The page class with respect to the gate (§5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateClass {
    /// No `search_terms` — always visible to a (content-matching) query.
    Gateless,
    /// Has one or more gate entries — visible only when an entry is unlocked.
    Gated,
}

/// The gate outcome for a single gate entry against a query.
#[inline]
pub fn entry_unlocks(lowered_entry: &str, lowered_text: &str) -> bool {
    // (a) non-empty entry (an empty gate line unlocks nothing), AND
    // (b) the whole entry occurs as a contiguous span in the query text.
    // Both already lower-cased by the caller (case-insensitive, §5).
    !lowered_entry.is_empty() && lowered_text.contains(lowered_entry)
}

/// True iff **any** of the page's gate entries is unlocked by the query text
/// (the `exists over entries` half of the gate disjunction).
#[inline]
pub fn any_entry_unlocks(lowered_entries: &[String], lowered_text: &str) -> bool {
    lowered_entries.iter().any(|e| entry_unlocks(e.as_str() /* already lowered */, lowered_text))
}

/// The full **visibility** decision for a page under a query's *content text*
/// (the FTS lane, not the filters — §5 gate rule): gateless pages are always
/// visible; gated pages are visible iff one of their entries occurs in the
/// text. `gate_class` and `lowered_entries` (the page's lower-cased `terms`,
/// empty for a gateless page) model the page; `lowered_text` is the lower-
/// cased FTS content portion of the query (bare + phrase terms, **not** the
/// `key:value` filters).
///
/// # Rules (mirrored by `spec/semantics_spec.rs`)
/// - `visible` is **total** for a given (page, query) and unambiguous.
/// - a **gateless** page is `visible` **iff** the content matched upstream
///   (we pin the gate half: gateless ⇒ the gate is *satisfied* unconditionally
///   — the content match is a separate FTS concern).
/// - a **gated** page is `visible` iff `∃ entry` that occurs in the text.
/// - the two page classes are **disjoint and total** (a page is one of them).
#[inline]
pub fn gate_satisfied(gc: GateClass, lowered_entries: &[String], lowered_text: &str) -> bool {
    match gc {
        GateClass::Gateless => true,
        GateClass::Gated => any_entry_unlocks(lowered_entries, lowered_text),
    }
}

/// Case-fold a string for gate comparison (exec helper for tests / the M7
/// layer). Pure; ASCII + Unicode lower-case is fine here because this is the
/// *reference* impl — the spec pins the *shape* (non-empty + contiguous
/// containment), not the exact fold algorithm.
#[inline]
pub fn fold_lower(s: &str) -> String {
    s.to_lowercase()
}

/// A filter as an opaque `key × value` pair. Mirrors
/// `search_grammar::Filter` but kept as a value here so the commutativity
/// lemma is independent of any ordering / container.
pub type FilterPair = (String, String);

/// The **conjunction**-over-filters predicate a page `p` satisfies: every
/// `(k, v)` in the filter set must hold on `p`. We prove this predicate is
/// **commutative** over its input (order of the filters does not change the
/// truth value) — the §9 "filter commutativity" contract.
///
/// `check(p, k, v)` is *assumed* (a real column / tag match by the M7 SQL);
/// we only need that it is a *deterministic function* of `(p, k, v)`, which
/// the commutativity proof uses. (Exec: `check` is the `pages.k = v` join.)
#[inline]
pub fn all_filters_hold(p: &str, filters: &[FilterPair]) -> bool {
    // every (k,v) must hold — a conjunction with vacuous truth on the empty set.
    // The concrete `check(page, k, v)` is the SQLite join (assumed, §9);
    // the *shape* we pin is that it is a conjunction, which is commutative
    // over any deterministic `check`. We return the model predicate here so
    // that a unit test can drive it with a concrete `Eq`-based `check`.
    filters.iter().all(|(k, v)| filter_match_on_page(p, k, v.as_str()))
}

/// Page ↔ filter match, deterministic: a page's identity (its abs_path) is
/// fixed and a filter is (k,v) — so `filter_match_on_page` is a pure
/// predicate of three inputs. In the M7 SQL layer it is the actual column
/// / tag join; here we model the *shape* a commutativity proof needs:
/// a deterministic function. (It is *not* the semantic match — see the unit
/// test, which uses an *Eq-based* stand-in to demonstrate commutativity
/// concretely.)
pub fn filter_match_on_page(_page: &str, _key: &str, _value: &str) -> bool {
    // A *real* implementation would read `pages.<key> = <value>` (or, for
    // `tag:`, `page_tags ∧ tag_aliases`). For the pure core we cannot touch
    // the DB — the exec side is a **placeholder** that is only exercised by
    // the concrete commutativity unit test, where the *predicate shape*
    // (conjunction over a deterministic function) carries the proof.
    //
    // Concretely, we do NOT claim a real match here; we return a
    // *deterministic* placeholder (e.g. false) so that the *shape*
    // (a commutative conjunction) is what we prove and the unit tests
    // re-express the contract with a real stand-in predicate.
    false
}

/// A concrete stand-in `check` for the commutativity test: a page "matches"
/// a filter iff it *lists* that exact `key=value` pair in its raw text
/// (a deterministic predicate of the three inputs — all the proof needs).
fn eq_check_on_page(page: &str, key: &str, value: &str) -> bool {
    page.contains(&format!("{key}={value}"))
}

/// Prove (unit-test) that a conjunction over a deterministic predicate is
/// **commutative** in its input set: for two filter lists that are
/// permutations of each other, the predicate value is unchanged (order
/// doesn't matter — §9).
#[inline]
pub fn filters_commute(a: &[FilterPair], b: &[FilterPair], p: &str) -> bool {
    // We use a deterministic stand-in `check` (not the placeholder `filter_match_on_page`);
    // the *property* is independent of the choice of `check`: any deterministic
    // predicate yields a commutative conjunction under reordering.
    a.iter().all(|(k, v)| eq_check_on_page(p, k.as_str(), v.as_str()))
        == b.iter().all(|(k, v)| eq_check_on_page(p, k.as_str(), v.as_str()))
}

// =============================================================================
// Unit tests — pin exec `semantics` to the spec shape (gate + commutativity).
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- gate: gateless vs gated (total + disjoint page class) ------------

    #[test]
    fn page_is_gateless_or_gated() {
        // total: a page's gate class is one of the two.
        let x = GateClass::Gateless;
        let y = GateClass::Gated;
        let cls = [x, y];
        for c in cls {
            assert!(matches!(c, GateClass::Gateless | GateClass::Gated));
        }
    }

    #[test]
    fn gateless_satisfies_unconditionally() {
        assert!(gate_satisfied(GateClass::Gateless, &[], "ai"));
        assert!(gate_satisfied(GateClass::Gateless, &[], ""));
        // (the content match is a separate FTS concern — gate alone is ✓)
    }

    #[test]
    fn gated_requires_an_entry() {
        let entries: Vec<String> = vec!["deep learning".to_string(), "mtls".to_string()];
        assert!(gate_satisfied(GateClass::Gated, &entries, "ai deep learning")); // unlocks
        assert!(gate_satisfied(GateClass::Gated, &entries, "mtls"));             // unlocks
        assert!(!gate_satisfied(GateClass::Gated, &entries, "ai deep"));         // does NOT unlock
    }

    #[test]
    fn empty_entry_unlocks_nothing() {
        assert!(!entry_unlocks("", "a is here"));
        assert!(entry_unlocks("a", "a is here"));
        // the empty-string entry is an inert line (no match on its own)
    }

    #[test]
    fn gate_case_insensitive() {
        // Both sides of entry_unlocks are lower-cased by the *caller* (the
        // contract): here the caller folds the query text too, so a mixed-case
        // query still matches its folded gate entry (case-insensitive, §5).
        let entries: Vec<String> = vec![fold_lower("mTLS"), fold_lower("Deep Learning")];
        assert!(entry_unlocks(&entries[0], &fold_lower("mtls everywhere")));
        assert!(entry_unlocks(&entries[0], &fold_lower("xMTLSx")));
        assert!(entry_unlocks(&entries[1], &fold_lower("i am studying AI deep learning now")));
        assert!(!entry_unlocks(&entries[1], &fold_lower("deep, learning"))); // not contiguous
    }

    // ---- filter commutativity ---------------------------------------------

    #[test]
    fn filters_commute_under_permutation() {
        // a page "text" with the raw filters listed — a deterministic
        // stand-in `check` (a page lists its own `k=v` pairs).
        let page = "type=wiki language=en tag=llm";
        let a: Vec<FilterPair> = vec![
            ("type".into(), "wiki".into()),
            ("language".into(), "en".into()),
            ("tag".into(), "llm".into()),
        ];
        let b: Vec<FilterPair> = vec![
            ("tag".into(), "llm".into()),
            ("language".into(), "en".into()),
            ("type".into(), "wiki".into()),
        ];
        assert!(filters_commute(&a, &b, page));
        // also: both sides are TRUE when the page lists every pair.
        assert!(a.iter().all(|(k,v)| page.contains(&format!("{k}={v}"))));
        assert!(b.iter().all(|(k,v)| page.contains(&format!("{k}={v}"))));
    }

    #[test]
    fn filters_commute_under_partial_match() {
        // a page with only some of the filters — the conjunction is still
        // invariant under reordering of the filter list (both sides FALSE
        // → equal).
        let page = "type=wiki";
        let a: Vec<FilterPair> = vec![
            ("type".into(), "wiki".into()),
            ("language".into(), "en".into()),   // page has no language=en
        ];
        let b: Vec<FilterPair> = vec![
            ("language".into(), "en".into()),
            ("type".into(), "wiki".into()),
        ];
        assert!(filters_commute(&a, &b, page)); // both sides false → equal
    }

    #[test]
    fn empty_filter_conjunction_is_vacuously_true() {
        // an empty filter set is a TRUE conjunction (vacuous truth).
        let a: Vec<FilterPair> = Vec::new();
        let b: Vec<FilterPair> = Vec::new();
        assert!(filters_commute(&a, &b, "any-page"));
    }

    // ---- AND / OR / NOT lane partition ------------------------------------

    #[test]
    fn lanes_are_mutually_exclusive() {
        // (unit-test mirror of the spec's "lane partition" lemma) — each term
        // is in *at most one lane*: we assert the exec's parse puts a `~`
        // term in the OR lane and not the AND lane, a `-term` in NOT and
        // not AND, and a bare word in AND and not OR/NOT.
        let q = super::super::search_grammar::parse("~llm ~mllm -gpt apple tag:x");
        // tag:x is a filter → in the filter lane (not in AND)
        assert!(q.filters.iter().any(|f| f.key == "tag"));
        assert!(!q.and_terms.iter().any(|t| matches!(t, super::super::search_grammar::Term::Token{ text, ..} if text == "tag:x")));
        assert_eq!(q.or_group.len(), 2);   // llm, mllm in the OR group
        assert_eq!(q.not_terms.len(), 1);  // gpt in NOT
        assert_eq!(q.and_terms.len(), 1);  // apple only
        assert!(matches!(
            &q.and_terms[0],
            super::super::search_grammar::Term::Token { text, phrase: false } if text == "apple"
        ));
    }
}
