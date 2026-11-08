//! resolve — cross-page reference resolution (M7b, §4 "Resolution order").
//!
//! This maps a *raw written reference* — a `[[wikilink]]` inner string, a
//! `type:slug` display id, a relative/absolute path, or a bare slug/substring —
//! to **a specific page** in a corpus, honoring §4's priority waterfall and
//! the §4 contract that *a collision is legal*: the outcome is a **candidate
//! list, never a hard error**.
//!
//! §4 (PLAN.md:94):
//!
//! > 1. `type:slug` exact — match `type` + slug; if exactly one page, return its `abs_path`
//! > 2. any-type exact slug, **if the slug is unique** across the corpus → its `abs_path`
//! > 3. explicit path ref (`root/rel/path`) → that `abs_path`
//! > 4. unique substring of a slug → its `abs_path`
//! > 5. else → return a **candidate list** of `abs_path` (no error)
//!
//! ## Model
//!
//! A corpus is a sequence of [`Page`] rows (the `pages` table projected to its
//! identity columns). A `Page` carries only what resolution reads: the
//! canonical `abs_path`, the display `slug`, the `type`, and the
//! root-relative `rel_path`. The resolver is **pure** — no IO, no `rusqlite` —
//! so it runs under `cargo build`/`cargo test` *and* its outcome shape is
//! pinned by `spec/resolve_spec.rs` (the 3-way `Unique | Candidates | Empty`
//! partition, the uniqueness⟹single contract, `type:slug`-stage precedence,
//! and candidate determinism).
//!
//! Separation-of-concerns vs. `cmd_resolve`: the SQL shell previously did a
//! flat `LIKE` over `id`/`title`/`abs_path` with no `type:`-qualified stage, no
//! uniqueness-gate, and no priority. This module is the *deciding* half; the
//! shell now loads the corpus and defers to [`resolve`], then renders whatever
//! outcome it returns (the two halves agree by construction — pinned by the
//! M7b integration tests, not by this pure core alone).

/// One page in the resolution corpus — the `pages` table projected to the
/// columns resolution reads. The identity columns decide *which* page a ref
/// points at; `body` (T8) is carried so the shell can run the **section-level
/// gate** on the page a link targets (see [`section_present`]) without a
/// second DB round-trip. `description`, `tags`, `mtime`, etc. remain
/// irrelevant to resolution and are not carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    /// Canonical absolute path — the page's identity (PK, §4/§8). This is what
    /// a *successful* resolution returns.
    pub abs_path: String,
    /// The display slug (frontmatter `title` → H1 → filename, §4). Matched by
    /// stages 1/2/4 (case-insensitive).
    pub slug: String,
    /// The `type` (`wiki` / `note` / `label`). Stage 1 matches on
    /// `(type, slug)`; other stages are type-agnostic.
    pub typ: String,
    /// Root-relative path (e.g. `wiki/attention.md`). Stage 3 (explicit path
    /// ref) matches against this and `abs_path`.
    pub rel_path: String,
    /// The page's body text (the `pages.body` column, `''` when NULL). Not read
    /// by stages 1–4 (identity resolution); carried for the T8 section-level
    /// gate — [`section_present`] checks a written `#section` against this.
    pub body: String,
}

/// The §4 resolution outcome. `Unique` names one page; `Candidates` names a
/// *set* (a collision — §4 "no hard error"); `Empty` names none. `Candidates`
/// is always **sorted by `abs_path`** (determinism — pinned by
/// `spec/resolve_spec.rs` and the unit tests below).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one page matches (stages 1–4 each gate on uniqueness).
    Unique(String),
    /// More than one page matches the highest-priority non-empty stage — the
    /// collision case, returned *not* as an error. Sorted by `abs_path`.
    Candidates(Vec<String>),
    /// No page matches any stage. (A *missing* reference — the shell may show
    /// it as "unresolved"; still not an error in this pure core.)
    Empty,
}

impl Resolution {
    /// The number of candidate `abs_path`s (1 for `Unique`, `n` for
    /// `Candidates(n)`, 0 for `Empty`). Lets the shell render uniformly.
    #[inline]
    pub fn count(&self) -> usize {
        match self {
            Resolution::Unique(_) => 1,
            Resolution::Candidates(v) => v.len(),
            Resolution::Empty => 0,
        }
    }

    /// `true` iff this outcome names exactly one page (the `Unique` arm).
    #[inline]
    pub fn is_unique(&self) -> bool {
        matches!(self, Resolution::Unique(_))
    }

    /// The *single* `abs_path` iff [`is_unique`], else `None` (for `Empty` and
    /// any multi-`Candidates` collision).
    #[inline]
    pub fn as_unique(&self) -> Option<&str> {
        match self {
            Resolution::Unique(p) => Some(p.as_str()),
            _ => None,
        }
    }
}

/// Split a `type:slug` display id on its *first* `:`. Returns `(type, slug)`
/// only when there is a non-empty type and a non-empty slug before/after the
/// colon. A bare slug (no `:`), a leading colon (`:foo`), or a trailing colon
/// (`foo:`) are *not* a display id → `None` (they fall through to the
/// slug/path/substring stages). Mirrors the `search_grammar::split_key_value`
/// shape, here for the `type:` prefix.
fn split_type_slug(s: &str) -> Option<(&str, &str)> {
    match s.find(':') {
        Some(p) if p > 0 && p + 1 < s.len() => Some((&s[..p], &s[p + 1..])),
        _ => None,
    }
}

/// Stage 3 — does a *path* query match a page? Case-sensitive (paths are
/// case-sensitive on the target OS), matching the query against the page's
/// `abs_path`, its root-relative `rel_path`, and the path *suffix* after the
/// root (so `wiki/attention.md` matches a page at `/root/wiki/attention.md`).
/// A bare slug or a `type:slug` id is *not* a path (stage 3 is tried only
/// after stages 1/2 yield nothing, and only compares path-shaped values).
fn path_matches(query: &str, page: &Page, suffix: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return false;
    }
    if q == page.abs_path || q == page.rel_path {
        return true;
    }
    // suffix form: `…/q` — accept a query that is the tail of the stored path
    // (e.g. `wiki/attention.md` → `/root/wiki/attention.md`). `suffix` is the
    // precomputed `/q` (see stage 3 below) — SECURITY_REPORT 3.16: hoisting
    // the `format!("/{}", q)` out of the per-page closure removes 2
    // allocations per page.
    page.abs_path.ends_with(suffix) || page.rel_path.ends_with(suffix)
}

/// Resolve a raw reference against a corpus, in §4 priority order.
///
/// # Priority (first non-empty stage wins)
/// 1. `type:slug` exact — pages whose `(type, slug)` matches the id.
/// 2. bare-slug exact — pages whose `slug` matches (type-agnostic).
/// 3. path ref — page whose `abs_path`/`rel_path`/path-suffix matches.
/// 4. substring — pages whose `slug` contains the query (a *unique* substring
///    yields one page; several yield candidates).
///
/// Each stage's match set, once non-empty, is the *final* set: if it is a
/// singleton → [`Resolution::Unique`]; if larger → [`Resolution::Candidates`]
/// (the §4 collision). If no stage matches → [`Resolution::Empty`]. Candidates
/// are sorted by `abs_path`; matching throughout is **case-insensitive** on
/// the display/slug axis (paths excepted, per `path_matches`).
///
/// # Purity
/// No allocation beyond the candidate `Vec`s; deterministic in the corpus
/// (independent of input order — the sort is on `abs_path`).
pub fn resolve(query: &str, pages: &[Page]) -> Resolution {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Resolution::Empty;
    }

    // ---- stage 1: `type:slug` (display-id) exact ---------------------------
    // SECURITY_REPORT 3.6: only return early when stage 1 actually hit
    // something. Previously the `return finish(hits)` was unconditional, so
    // `note2:gpt` (no page of type `note2` with slug `gpt`) was resolved as
    // `Empty` even though a `type:slug`-shaped query with a non-existent type
    // might still match a page at stage 2 (bare-slug exact), 3 (path), or 4
    // (substring). Only return `finish` for a non-empty stage 1; otherwise
    // fall through.
    if let Some((typ, slug)) = split_type_slug(query) {
        let typ_l = typ.to_lowercase();
        let slug_l = slug.to_lowercase();
        let hits: Vec<&Page> = pages
            .iter()
            .filter(|p| p.typ.to_lowercase() == typ_l && p.slug.to_lowercase() == slug_l)
            .collect();
        if !hits.is_empty() {
            return finish(hits);
        }
    }

    // ---- stage 2: bare-slug exact, any type -------------------------------
    let exact: Vec<&Page> = pages
        .iter()
        .filter(|p| p.slug.to_lowercase() == q)
        .collect();
    if !exact.is_empty() {
        return finish(exact);
    }

    // ---- stage 3: explicit path ref ---------------------------------------
    // SECURITY_REPORT 3.16: compute the `/q` suffix *once* (not per page) and
    // pass it into `path_matches`.
    //
    // NOTE: uses `query.trim()` — the *original* (case-sensitive) query —
    // to keep `path_matches`' documented case-sensitive semantic. `q` from
    // the stage-0 setup is the lowercased form and would be the wrong key.
    let stage3_suffix = format!("/{}", query.trim());
    let pathy: Vec<&Page> = pages
        .iter()
        .filter(|p| path_matches(query, p, &stage3_suffix))
        .collect();
    if !pathy.is_empty() {
        return finish(pathy);
    }

    // ---- stage 4: unique substring of a slug -------------------------------
    let substr: Vec<&Page> = pages
        .iter()
        .filter(|p| p.slug.to_lowercase().contains(&q))
        .collect();
    if !substr.is_empty() {
        return finish(substr);
    }

    Resolution::Empty
}

/// Fold a stage's match set into an outcome: singleton → `Unique`, larger →
/// `Candidates` (sorted by `abs_path`, first-seen dupes are inherent to a set
/// of distinct `abs_path`s), empty → `Empty`.
fn finish(hits: Vec<&Page>) -> Resolution {
    if hits.is_empty() {
        return Resolution::Empty;
    }
    if hits.len() == 1 {
        return Resolution::Unique(hits[0].abs_path.clone());
    }
    let mut v: Vec<String> = hits.iter().map(|p| p.abs_path.clone()).collect();
    v.sort();
    v.dedup();
    Resolution::Candidates(v)
}

/// T8 — does a **written `#section`** name a section that exists in a page
/// body? Pure (no DB / IO): delegates section identity to the same 4-stage
/// `section::section_lookup` cascade the rest of kb uses.
///
/// Contract:
/// - `true`  — the section resolves to a unique heading (the link is *fully*
///   resolved: page + section).
/// - `false` — the section names nothing in the body (`NoSection` / `EmptyRef`
///   / `NotFound`). Combined with a page that *already* resolved uniquely,
///   this is the T8 "page found, section missing" case.
/// - `false` also on `Err(Ambiguous)` — the body has 2+ matching headings, so
///   the `#section` itself needs disambiguation; the caller treats both
///   "missing" and "ambiguous section" as "not resolved to a *single* section".
///
/// Empty body / empty section ref → `false` (no section to find), so a link
/// with a `#` tail but an empty target body degrades to ambiguous, as
/// intended.
pub fn section_present(body: &str, section: &str) -> bool {
    if body.is_empty() || section.trim().is_empty() {
        return false;
    }
    matches!(crate::section::section_lookup(body, section), Ok(Some(_)))
}

// =============================================================================
// Unit tests — pin exec `resolve` to the §4 shape (drift-checked by Verus).
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn page(abs: &str, slug: &str, typ: &str, rel: &str, body: &str) -> Page {
        Page {
            abs_path: abs.into(),
            slug: slug.into(),
            typ: typ.into(),
            rel_path: rel.into(),
            body: body.into(),
        }
    }

    fn page_no_body(abs: &str, slug: &str, typ: &str, rel: &str) -> Page {
        page(abs, slug, typ, rel, "")
    }

    fn corpus() -> Vec<Page> {
        vec![
            page_no_body(
                "/root/wiki/attention.md",
                "Attention",
                "wiki",
                "wiki/attention.md",
            ),
            page_no_body(
                "/root/wiki/Attention2.md",
                "Attention",
                "note",
                "wiki/attention2.md",
            ),
            page_no_body("/root/note/llm.md", "LLM", "note", "note/llm.md"),
            page_no_body(
                "/root/wiki/transformer.md",
                "Transformer (architecture)",
                "wiki",
                "wiki/transformer.md",
            ),
        ]
    }

    // ---- outcome partition (Unique | Candidates | Empty) ------------------

    #[test]
    fn every_resolution_is_wellformed() {
        // unique
        let r = resolve("llm", &corpus());
        assert!(r.is_unique(), "LLM is a unique slug");
        assert_eq!(r.as_unique(), Some("/root/note/llm.md"));
        assert_eq!(r.count(), 1);
        // candidates (Attention is shared by two pages)
        let r = resolve("attention", &corpus());
        assert!(!r.is_unique());
        assert_eq!(r.count(), 2);
        // empty
        let r = resolve("zzz-no-such-slug", &corpus());
        assert!(!r.is_unique());
        assert_eq!(r.count(), 0);
    }

    // ---- stage 1: type:slug precedence over bare-slug ----------------------

    #[test]
    fn type_qualified_slug_beats_bare_slug() {
        // "Attention" is ambiguous (wiki + note); "wiki:Attention" pins one.
        let b = resolve("Attention", &corpus());
        assert!(!b.is_unique(), "bare Attention is a collision");
        let t = resolve("wiki:Attention", &corpus());
        assert_eq!(
            t,
            Resolution::Unique("/root/wiki/attention.md".into()),
            "wiki:Attention selects the wiki page"
        );
    }

    #[test]
    fn colon_slug_falls_through_to_bare_slug_stage() {
        // SECURITY_REPORT 3.6: a `type:slug`-shaped query whose *type* doesn't
        // match must not short-circuit the resolver. `split_type_slug("foo:bar")`
        // is `Some(("foo","bar"))`, so before the fix stage 1 returned
        // `finish(empty) == Empty` unconditionally and stages 2/3/4 never ran.
        // A page whose slug is *literally* "foo:bar" is only reachable via the
        // stage-2 bare-slug exact match — which the old code skipped.
        let pages = vec![page_no_body("/root/x.md", "foo:bar", "note", "x.md")];
        let r = resolve("foo:bar", &pages);
        assert_eq!(
            r,
            Resolution::Unique("/root/x.md".into()),
            "stage-1 type miss must fall through to stage-2 bare-slug match"
        );
    }

    #[test]
    fn type_qualified_unknown_type_is_empty() {
        // label:Attention → no page has type `label` with that slug (and the
        // bare/substring stages don't fire because stage 1 is non-empty? no —
        // stage 1 yields EMPTY for an unmatched type, so we fall through).
        let r = resolve("label:Attention", &corpus());
        // stage 1 empty → stage 2 bare-slug "label:attention" (no match) →
        // stage 3 path (no match) → stage 4 substring (no slug contains
        // "label:attention") → Empty.
        assert_eq!(
            r.count(),
            0,
            "unknown type yields no stage-1 hit and no later stage matches"
        );
    }

    // ---- stage 2: exact slug, type-agnostic --------------------------------

    #[test]
    fn exact_slug_multiple_pages_is_a_collision() {
        let r = resolve("attention", &corpus());
        // sorted by abs_path (byte order): 'A'(65) < 'a'(97), so Attention2 first.
        assert_eq!(
            r,
            Resolution::Candidates(vec![
                "/root/wiki/Attention2.md".into(),
                "/root/wiki/attention.md".into(),
            ]),
            "two `Attention` pages → sorted candidate list, not an error"
        );
    }

    // ---- stage 3: path ref -------------------------------------------------

    #[test]
    fn absolute_path_resolves_uniquely() {
        let r = resolve("/root/wiki/transformer.md", &corpus());
        assert_eq!(r, Resolution::Unique("/root/wiki/transformer.md".into()));
    }

    #[test]
    fn relative_path_resolves_uniquely() {
        let r = resolve("wiki/transformer.md", &corpus());
        assert_eq!(r, Resolution::Unique("/root/wiki/transformer.md".into()));
    }

    // ---- stage 4: unique substring -----------------------------------------

    #[test]
    fn unique_substring_resolves() {
        // "transformer" is a unique substring (one page); "archi" likewise.
        let r = resolve("transformer", &corpus());
        assert_eq!(r, Resolution::Unique("/root/wiki/transformer.md".into()));
        let r = resolve("archi", &corpus());
        assert_eq!(
            r,
            Resolution::Unique("/root/wiki/transformer.md".into()),
            "substring of the long slug still selects that one page"
        );
    }

    // ---- case-insensitivity on the slug axis --------------------------------

    #[test]
    fn slug_match_is_case_insensitive() {
        assert_eq!(
            resolve("ATTENTION", &corpus()).count(),
            resolve("attention", &corpus()).count()
        );
        assert_eq!(resolve("lLm", &corpus()), resolve("LLM", &corpus()));
    }

    // ---- determinism (order-independent, sorted) ---------------------------

    #[test]
    fn candidates_are_sorted_and_order_independent() {
        let mut c2 = corpus();
        c2.reverse();
        let r1 = resolve("attention", &corpus());
        let r2 = resolve("attention", &c2);
        assert_eq!(r1, r2, "outcome is independent of corpus order");
        if let Resolution::Candidates(v) = &r1 {
            let mut s = v.clone();
            s.sort();
            assert_eq!(v, &s, "candidate list is sorted by abs_path");
        } else {
            panic!("expected candidates");
        }
    }

    // ---- empty / degenerate -----------------------------------------------

    #[test]
    fn blank_query_is_empty() {
        assert_eq!(resolve("   ", &corpus()), Resolution::Empty);
        assert_eq!(resolve("", &corpus()), Resolution::Empty);
    }

    #[test]
    fn empty_corpus_is_empty() {
        assert_eq!(resolve("anything", &[]), Resolution::Empty);
    }

    // ---- split_type_slug (display-id shape) --------------------------------

    #[test]
    fn split_type_slug_shape() {
        assert_eq!(
            split_type_slug("wiki:Attention"),
            Some(("wiki", "Attention"))
        );
        assert_eq!(
            split_type_slug("transformer (a):b"),
            Some(("transformer (a)", "b"))
        );
        assert_eq!(split_type_slug("Attention"), None);
        assert_eq!(split_type_slug(":no-type"), None);
        assert_eq!(split_type_slug("type:"), None);
        assert_eq!(split_type_slug(""), None);
    }

    // ---- T8: section_present (page resolves, #section exists?) ---------------

    #[test]
    fn section_present_true_when_section_in_body() {
        let body = "# Page\n\n## Target\ncontent here\n\n## Other\ntoo\n";
        assert!(section_present(body, "Target"), "unique title match → present");
        assert!(section_present(body, "target"), "case-insensitive → present");
        assert!(section_present(body, "1.1"), "dotted number → present");
        assert!(section_present(body, "Page > Other"), "title path → present");
    }

    #[test]
    fn section_present_false_when_missing() {
        let body = "# Page\n\n## A\nx\n\n## B\ny\n";
        assert!(!section_present(body, "C"), "section not in body → missing");
        assert!(!section_present(body, "9.9"), "dotted ref not in body → missing");
        assert!(!section_present("", "A"), "empty body → missing");
        assert!(!section_present(body, ""), "empty section ref → missing");
        assert!(!section_present(body, "   "), "whitespace section ref → missing");
    }

    #[test]
    fn section_present_false_when_ambiguous() {
        let body = "# Root\n\n## Dup\na\n\n## Dup\nb\n";
        assert!(!section_present(body, "Dup"), "two match → ambiguous → not present");
    }

    #[test]
    fn section_present_single_h1() {
        // a page that is itself one H1: `# Only` → section "Only" at line 0
        let body = "# Only\nbody\n";
        assert!(section_present(body, "Only"));
        assert!(section_present(body, "1"));
    }
}
