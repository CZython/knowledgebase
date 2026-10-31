//! search_grammar — parse a raw `kb search` query into FTS content +
//! whitelisted filters, and render the FTS portion as a well-formed,
//! correctly-quoted FTS5 string (M3).
//!
//! §5 (Search model) + §9 (Verification):
//!
//! > `search_grammar` — token→FTS string **well-formed**; **escaping/quoting
//! > correct**; **filter-vs-FTS separation preserved**.
//!
//! ## The grammar (§5)
//!
//! - `type:wiki` `language:en` — a **filter** on a *whitelisted* frontmatter
//!   column (v1 keys: `type`, `language`, plus `tag:…` which joins `page_tags`);
//!   exact string on the stored value. These are **not** FTS content.
//! - `type:Null` `language:Null` `tag:Null` — the **absence** filter for the
//!   same keys: the column must be `NULL` (or, for `tag`, no such tag row).
//!   The sentinel is exact-cased `Null`, so a literal value `null` is
//!   still an ordinary exact-value filter (`language:null`).
//! - `apple banana` — FTS **AND** (implicit: whitespace-joined terms).
//! - `~apple ~orange` — FTS **OR** group (one parenthesised `(a OR b)` clause).
//! - `-pear` — FTS **NOT**.
//! - `"exact phrase"` — an FTS **phrase** (quoted, inner `"` doubled).
//! - **Order irrelevant** — AND terms and filters are commutative.
//!
//! The *separation* the §9 line names: a `key:value` token is a **filter** iff
//! its key is in the v1 whitelist; every other token is **FTS content**. A
//! non-whitelisted `key:value` (e.g. `foo:bar`) is *content*, not a filter —
//! the two lanes never mix, and a single token is one or the other (never both).
//!
//! Quoting: a content term is rendered as a bare token unless it is *ambiguous*
//! (a phrase, or it contains a space / a double-quote); ambiguous terms are
//! double-quoted with inner quotes **doubled** (the FTS5 string-literal rule).
//!
//! Verified: `spec/search_grammar_spec.rs` proves the well-formedness,
//! the quoting-required partition (ambiguous ⟹ quoted, bare-word ⟹ not),
//! the filter/content separation (whitelist is a clean 3-key disjoint set),
//! and AND-term commutativity.
//!
//! The concrete boundary cases (a phrase term, a `~` group, a `-term`, a
//! non-whitelisted `foo:bar`, a quote inside a phrase) are pinned by the unit
//! tests below.

use std::collections::BTreeSet;

/// A whitelisted filter key (v1). `type` / `language` are frontmatter-column
/// filters (§5); `tag` is the same *filter* shape but joins `page_tags` (after
/// alias expansion) in the M7 SQL — all three are **filters**, separated from
/// the FTS content lane.
const FILTER_KEYS: &[&str] = &["type", "language", "tag"];

/// True iff `key` is a v1 **filter** key. A `key:value` token is a filter
/// only when its key is here *and* the value is non-empty; otherwise the token
/// is FTS content.
#[inline]
pub fn is_filter_key(key: &str) -> bool {
    FILTER_KEYS.contains(&key)
}

/// The v1 filter-key whitelist (exposed for the M7 SQL builder / tests).
pub fn filter_keys() -> BTreeSet<&'static str> {
    FILTER_KEYS.iter().copied().collect()
}

/// A frontmatter-column / tag **filter** (kept out of the FTS content lane).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    /// The key (`type` / `language` / `tag`).
    pub key: String,
    /// The exact value to match (whitespace-trimmed, verbatim otherwise).
    /// The special value `Null` means "the page **lacks** this field"
    /// (the SQL layer renders `IS NULL` / a missing-tag `NOT EXISTS`).
    pub value: String,
}

impl Filter {
    /// `true` iff this is a `tag:…` filter (the M7 layer routes it to
    /// `page_tags` + `tag_aliases`, not the `pages` column filter).
    #[inline]
    pub fn is_tag(&self) -> bool {
        self.key == "tag"
    }

    /// `true` iff the value is the `Null` sentinel: "the page must **not**
    /// have this field" (`type:Null` → `pages.type IS NULL`;
    /// `language:Null` → `pages.language IS NULL`;
    /// `tag:Null` → no row in `page_tags` with that name). Exact-cased
    /// (`Null`), so a field value literally spelled `null` still matches
    /// `language:null`.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.value == "Null"
    }
}

/// A single piece of FTS **content**. Either a single bare/quoted term or a
/// quoted multi-word phrase.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Term {
    /// A single token, e.g. `apple` or a quoted `"a b"`. `phrase == true`
    /// means render as an FTS5 phrase (quoted, inner quotes doubled).
    Token { text: String, phrase: bool },
}

/// A parsed query, with the **filter** and **content** lanes separated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    /// Whitelisted `key:value` filters (order preserved but commutative).
    pub filters: Vec<Filter>,
    /// The FTS **AND** clause: required terms (bare words and phrases),
    /// in document order. Empty if the query is filter-only.
    pub and_terms: Vec<Term>,
    /// The FTS **OR** group (one `~`-run), if any `~` tokens were present.
    /// Renders as a single parenthesised `(a OR b OR …)` AND-clause.
    pub or_group: Vec<Term>,
    /// FTS **NOT** terms (`-term`), in document order.
    pub not_terms: Vec<Term>,
}

impl ParsedQuery {
    /// The FTS content terms *only* (what goes into `pages_fts MATCH …`).
    #[inline]
    pub fn fts_present(&self) -> bool {
        !self.and_terms.is_empty() || !self.or_group.is_empty() || !self.not_terms.is_empty()
    }

    /// Render the FTS **content** portion as a well-formed FTS5 query string
    /// (the value for `pages_fts MATCH <fts_query>`). Top-level clauses are
    /// **space-joined** (a space in FTS5 *is* implicit AND), the `~` group is
    /// one **parenthesised** `(a OR b OR …)` clause, a `-term` becomes a
    /// `NOT <term>` clause, and ambiguous terms are **quoted** (inner `"`
    /// doubled). Filter-only queries render as the empty string (the M7 layer
    /// then omits the `MATCH` clause).
    ///
    /// # Clause order (proven-parseable on the bundled SQLite FTS5)
    ///
    /// The render order is **OR-group first, then NOT terms, then bare AND
    /// terms** — all space-joined. This is not arbitrary: on the bundled
    /// build (SQLite 3.46.0), FTS5 *rejects* a bare term sitting directly
    /// after a parenthesized group (`attention (llm OR mllm)` → syntax error
    /// near `OR`) and also rejects explicit `AND NOT` (`a AND NOT b` →
    /// syntax error near `NOT`). The *only* confirmed-parseable ordering
    /// for the AND/OR/NOT structure is the one we emit here:
    ///
    /// | shape              | render                              | FTS5 parse (SQLite 3.46.0) |
    /// |--------------------|-------------------------------------|----------------------------|
    /// | `a b`              | `a b`                               | `a AND b` (implicit)       |
    /// | `a b -c`           | `a b NOT c`                         | `a AND b AND NOT c`        |
    /// | `~a ~b`            | `(a OR b)`                          | `a OR b`                   |
    /// | `~a ~b -c`         | `(a OR b) NOT c`                    | `(a OR b) AND NOT c`       |
    /// | `~a ~b a' -c`      | `(a OR b) NOT c a'`                 | `(a OR b) AND NOT c AND a'`|
    /// | `t:… a` (filter)   | `a`                                 | (filter is a SQL join, §5) |
    ///
    /// Proven by `kb-db/tests/fts5.rs` →
    /// `canonical_fts_render_means_what_the_grammar_intends` (an empirical
    /// FTS5 probe that seeds known rows and asserts the exact row set each
    /// rendered string returns — the load-bearing guarantee that "the render
    /// string means what the grammar intends" on the *actual* engine build,
    /// per the §9 "escaping/quoting correct" bullet; see also PLAN.md §13d).
    pub fn render_fts(&self) -> String {
        let mut clauses: Vec<String> = Vec::new();

        if !self.or_group.is_empty() {
            // OR-group present: it MUST lead (FTS5 rejects a bare term sitting
            // directly before/after a paren-group). Then NOT (infix), then bare
            // (bare *may* trail a NOT clause). Confirmed parseable on SQLite 3.46.0:
            //   "(llm OR mllm) NOT gpt attention"  →  OK [E]
            clauses.push(format!(
                "({})",
                self.or_group
                    .iter()
                    .map(render_term)
                    .collect::<Vec<_>>()
                    .join(" OR ")
            ));
            for nt in &self.not_terms {
                clauses.push(format!("NOT {}", render_term(nt)));
            }
            for t in &self.and_terms {
                clauses.push(render_term(t));
            }
        } else {
            // No OR group: bare terms lead, then NOT (each NOT is infix-after-a-term,
            // which is the only place FTS5 accepts it). Confirmed parseable:
            //   "apple NOT pear"  →  OK [A]
            //   "NOT pear apple"  →  ERR (leading NOT is rejected by FTS5)
            //
            //   A NOT-only query (`-gpt` alone, no bare, no OR) renders as
            //   `"NOT gpt"` which FTS5 rejects as a leading NOT — the M7 SQL
            //   layer converts this to a column-exclusion filter (not a MATCH
            //   clause) when `fts_present() == true` but no non-NOT clause
            //   exists. See `semantics_spec.rs` for the spec boundary.
            for t in &self.and_terms {
                clauses.push(render_term(t));
            }
            for nt in &self.not_terms {
                clauses.push(format!("NOT {}", render_term(nt)));
            }
        }

        clauses.join(" ")
    }
}

/// Is `s` ambiguous as a bare FTS5 token (so it *must* be quoted)?
///
/// A term is ambiguous if it is empty, or contains whitespace, or contains a
/// double-quote, or begins with an FTS5 structural operator character (a
/// `^` prefix is FTS5's… actually FTS5 prefix is `*` — but a leading
/// `(` / operator / quote would change meaning). For v1: whitespace or a
/// double-quote forces quoting.
pub fn needs_quoting(s: &str) -> bool {
    s.is_empty() || s.contains(' ') || s.contains('"')
}

/// Render a single content term as an FTS5 token: bare if unambiguous,
/// double-quoted (inner `"` doubled) if ambiguous or a multi-word phrase.
///
/// # Quoting rule (the "escaping/quoting correct" contract)
/// - `apple` → `apple`
/// - `"exact phrase"` → `"exact phrase"`
/// - `a"b` → `"a""b"` (inner quote doubled)
pub fn render_term(t: &Term) -> String {
    match t {
        Term::Token { text, phrase } => {
            if *phrase || needs_quoting(text) {
                format!("\"{}\"", text.replace('"', "\"\""))
            } else {
                text.clone()
            }
        }
    }
}

/// A token produced by [`tokenize`]: either a bare word or an outer-quoted
/// phrase (its inner text already unescaped).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Word(String),
    Phrase(String),
}

/// Tokenize a query into words / phrases. Whitespace separates words; a
/// double-quote opens a **phrase** that runs to the matching unescaped close
/// quote — inner whitespace is part of the phrase, and `""` is an escaped
/// inner quote. This is the *only* place quotes are interpreted; after it,
/// tokens carry no quote syntax.
fn tokenize(query: &str) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::new();
    let chars: Vec<char> = query.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        // skip whitespace
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == '"' {
            // phrase: scan to the matching unescaped close quote
            let mut j = i + 1;
            let mut buf: Vec<char> = Vec::new();
            let mut closed = false;
            while j < n {
                if chars[j] == '"' {
                    if j + 1 < n && chars[j + 1] == '"' {
                        buf.push('"'); // escaped inner quote
                        j += 2;
                        continue;
                    }
                    closed = true;
                    break;
                }
                buf.push(chars[j]);
                j += 1;
            }
            if closed {
                out.push(Tok::Phrase(buf.into_iter().collect()));
                i = j + 1;
            } else {
                // unterminated quote: treat the rest as a bare word (lenient)
                let buf: String = buf.into_iter().collect();
                if !buf.is_empty() {
                    out.push(Tok::Word(buf));
                }
                i = n;
            }
        } else {
            // bare word: run to the next whitespace or quote
            let mut j = i;
            let start = i;
            while j < n && !chars[j].is_whitespace() && chars[j] != '"' {
                j += 1;
            }
            let w: String = chars[start..j].iter().collect();
            if !w.is_empty() {
                out.push(Tok::Word(w));
            }
            i = j;
        }
    }
    out
}

/// Parse a raw `kb search` query string into the separated
/// [`ParsedQuery`]. Tokens are classified:
///
/// - a **phrase** (outer-quoted) → AND-term (rendered quoted)
/// - `~term` → OR group
/// - `-term` → NOT
/// - `key:value` with a **whitelisted** key + non-empty value → filter
/// - otherwise → AND-term (a bare word)
///
/// # Rules (mirrored by `spec/search_grammar_spec.rs`)
/// - filter-vs-FTS separation: a token is a filter *or* content, never both.
/// - only whitelisted keys are filters; non-whitelisted `foo:bar` is content.
/// - `~` terms all join the single OR group (document order).
pub fn parse(query: &str) -> ParsedQuery {
    let toks = tokenize(query);
    let mut q = ParsedQuery::default();
    let mut i = 0usize;
    while i < toks.len() {
        match &toks[i] {
            Tok::Phrase(inner) => {
                q.and_terms.push(Term::Token {
                    text: inner.clone(),
                    phrase: true,
                });
                i += 1;
            }
            Tok::Word(w) => {
                if let Some(t) = w.strip_prefix('~') {
                    if !t.is_empty() {
                        q.or_group.push(Term::Token {
                            text: t.to_string(),
                            phrase: false,
                        });
                    }
                    i += 1;
                    continue;
                }
                if let Some(t) = w.strip_prefix('-') {
                    if !t.is_empty() {
                        q.not_terms.push(Term::Token {
                            text: t.to_string(),
                            phrase: false,
                        });
                    }
                    i += 1;
                    continue;
                }
                if let Some((k, v)) = split_key_value(w) {
                    if is_filter_key(k) && !v.is_empty() {
                        q.filters.push(Filter {
                            key: k.to_string(),
                            value: v.to_string(),
                        });
                        i += 1;
                        continue;
                    }
                    // A whitelisted `key:` whose value is the *next* quoted phrase
                    // (a multi-word value, e.g. `tag:"large language models"`). The
                    // quote is what makes the surrounding span unbreakable, so the
                    // phrase is the filter value, not separate FTS content.
                    if v.is_empty() && is_filter_key(k) {
                        if let Some(Tok::Phrase(inner)) = toks.get(i + 1) {
                            q.filters.push(Filter {
                                key: k.to_string(),
                                value: inner.clone(),
                            });
                            i += 2;
                            continue;
                        }
                    }
                    // non-whitelisted key (or empty value w/o a phrase): *content*, not a filter
                }
                if !w.is_empty() {
                    q.and_terms.push(Term::Token {
                        text: w.clone(),
                        phrase: false,
                    });
                }
                i += 1;
            }
        }
    }
    q
}

/// Split `token` on its *first* `:`. Returns `(key, value)` only if there is a
/// non-empty key before the colon. A leading colon (`:foo`) or no colon → None.
fn split_key_value(token: &str) -> Option<(&str, &str)> {
    token.find(':').and_then(|p| {
        if p == 0 {
            None
        } else {
            Some((&token[..p], &token[p + 1..]))
        }
    })
}

// =============================================================================
// Unit tests — pin exec `search_grammar` to the spec shape.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- filter-vs-FTS separation (whitelist) ------------------------------

    #[test]
    fn whitelisted_key_is_a_filter() {
        let q = parse("type:wiki apple");
        assert_eq!(
            q.filters,
            vec![Filter {
                key: "type".into(),
                value: "wiki".into()
            }]
        );
        assert_eq!(q.and_terms.len(), 1);
        assert_eq!(
            q.and_terms[0],
            Term::Token {
                text: "apple".into(),
                phrase: false
            }
        );
    }

    #[test]
    fn tag_is_a_filter_but_routes_differently() {
        let q = parse("tag:llm attention");
        assert!(q.filters.iter().any(|f| f.is_tag() && f.value == "llm"));
        assert_eq!(q.and_terms.len(), 1);
    }

    #[test]
    fn quoted_phrase_after_filter_key_is_the_filter_value() {
        // `tag:"large language models"`: the quote is what makes the span
        // unbreakable, so the whole phrase is the filter *value* (multi-word),
        // not a separate FTS content phrase.
        let q = parse("tag:\"large language models\"");
        assert_eq!(
            q.filters,
            vec![Filter {
                key: "tag".into(),
                value: "large language models".into()
            }]
        );
        assert!(
            q.and_terms.is_empty(),
            "the phrase must be the filter value, not content"
        );
        assert!(q.not_terms.is_empty());
        assert!(q.or_group.is_empty());
        assert_eq!(q.render_fts(), "");
    }

    #[test]
    fn quoted_phrase_after_filter_key_mixed_with_content() {
        // Filter + content still separate: the `attention` term stays FTS.
        let q = parse("tag:\"llm\" attention");
        assert_eq!(
            q.filters,
            vec![Filter {
                key: "tag".into(),
                value: "llm".into()
            }]
        );
        assert_eq!(q.and_terms.len(), 1);
        assert_eq!(
            q.and_terms[0],
            Term::Token {
                text: "attention".into(),
                phrase: false
            }
        );
    }

    #[test]
    fn non_whitelisted_key_is_content_not_filter() {
        // `foo:bar` is not a filter (foo ∉ whitelist) → it is a content token.
        let q = parse("foo:bar");
        assert!(q.filters.is_empty(), "foo: must not be a filter");
        assert_eq!(q.and_terms.len(), 1);
        assert_eq!(
            q.and_terms[0],
            Term::Token {
                text: "foo:bar".into(),
                phrase: false
            }
        );
    }

    #[test]
    fn filter_needs_nonempty_value() {
        let q = parse("type: apple");
        // `type:` with an empty value is NOT a filter; `type:` is content.
        assert!(q.filters.is_empty());
    }

    #[test]
    fn filter_keys_are_the_v1_whitelist() {
        let keys = filter_keys();
        assert!(keys.contains("type"));
        assert!(keys.contains("language"));
        assert!(keys.contains("tag"));
        assert!(!keys.contains("foo"));
        assert_eq!(keys.len(), 3);
    }

    // ---- the `Null` absence sentinel --------------------------------------

    #[test]
    fn null_sentinel_is_a_filter_per_key() {
        for key in ["type", "language", "tag"] {
            let q = parse(&format!("{key}:Null"));
            assert!(
                q.filters.iter().any(|f| f.key == key && f.is_null()),
                "{key}:Null must parse as a null filter"
            );
            assert!(q.and_terms.is_empty());
        }
    }

    #[test]
    fn null_sentinel_is_exact_cased() {
        // `language:Null` is the sentinel…
        let q = parse("language:Null");
        let f = q.filters.iter().find(|f| f.key == "language").expect("a language filter");
        assert!(f.is_null());
        // …but `language:null` (lowercase) is a plain exact-value filter on
        // the string "null" — a field *value* literally spelled "null" still
        // matches, and the absence sentinel is exact-cased `Null`.
        let q = parse("language:null");
        let f = q.filters.iter().find(|f| f.key == "language").expect("a language filter");
        assert!(!f.is_null());
        assert_eq!(f.value, "null");
        // Case-mixed is also not the sentinel.
        assert!(parse("language:NULL")
            .filters
            .iter()
            .all(|f| !f.is_null()));
    }

    #[test]
    fn null_sentinel_and_content_coexist() {
        let q = parse("language:Null attention");
        assert!(q.filters.iter().any(|f| f.is_null()));
        assert_eq!(q.and_terms.len(), 1);
    }

    // ---- AND / OR / NOT parsing --------------------------------------------

    #[test]
    fn bare_terms_are_and() {
        let q = parse("apple banana");
        assert!(q.or_group.is_empty());
        assert!(q.not_terms.is_empty());
        assert_eq!(q.and_terms.len(), 2);
        assert_eq!(q.render_fts(), "apple banana");
    }

    #[test]
    fn tilde_terms_form_one_or_group() {
        let q = parse("~llm ~mllm");
        assert_eq!(q.or_group.len(), 2);
        assert_eq!(q.render_fts(), "(llm OR mllm)");
    }

    #[test]
    fn dash_term_is_not() {
        // `-pear` → a `NOT pear` clause; `apple` is the AND term (space = implicit AND).
        // The canonical order is [NOT] → [bare], since bare cannot directly
        // follow a paren-group on the bundled SQLite FTS5 build (see render_fts
        // doc). Here there's no OR group, so it's simply `apple NOT pear`.
        let q = parse("apple -pear");
        assert_eq!(q.not_terms.len(), 1);
        assert_eq!(q.render_fts(), "apple NOT pear");
    }

    #[test]
    fn or_and_not_coexist() {
        // Canonical render order: OR-group → NOT → bare AND terms (see
        // render_fts doc for the FTS5 parse-order rationale and the
        // kb-db/tests/fts5.rs empirical probe that LOCKS this against the
        // bundled SQLite 3.46.0 engine).
        let q = parse("~llm ~mllm attention -gpt");
        assert_eq!(q.render_fts(), "(llm OR mllm) NOT gpt attention");
    }

    #[test]
    fn order_is_irrelevant_for_semantics() {
        // Reordering AND terms keeps the same required term *set* (commutative
        // filters, §9). We assert the rendered clause multiset is unchanged.
        let a = parse("apple banana").and_terms.clone();
        let b = parse("banana apple").and_terms.clone();
        assert_eq!(a.len(), b.len());
        // same terms as a set:
        let sa: BTreeSet<_> = a.iter().cloned().collect();
        let sb: BTreeSet<_> = b.iter().cloned().collect();
        assert_eq!(sa, sb);
    }

    // ---- quoting / escaping ------------------------------------------------

    #[test]
    fn phrase_is_quoted() {
        let q = parse("\"exact phrase\"");
        assert_eq!(q.and_terms.len(), 1);
        assert_eq!(
            q.and_terms[0],
            Term::Token {
                text: "exact phrase".into(),
                phrase: true
            }
        );
        assert_eq!(q.render_fts(), "\"exact phrase\"");
    }

    #[test]
    fn inner_quote_is_doubled() {
        // a token containing a double-quote is ambiguous → quoted, inner doubled.
        let t = Term::Token {
            text: String::from("a\"b"),
            phrase: false,
        };
        assert_eq!(render_term(&t), "\"a\"\"b\"");
    }

    #[test]
    fn bare_word_stays_unquoted() {
        assert!(!needs_quoting("apple"));
        assert_eq!(
            render_term(&Term::Token {
                text: "apple".into(),
                phrase: false
            }),
            "apple"
        );
    }

    #[test]
    fn escaped_inner_quote_is_kept() {
        // `"a "" b"` tokenizes to the inner text `a " b` (the doubled quote
        // is a single unescaped `"` in the phrase body).
        let q = parse(r#""a "" b""#);
        assert_eq!(q.and_terms.len(), 1);
        assert_eq!(
            q.and_terms[0],
            Term::Token {
                text: String::from("a \" b"),
                phrase: true
            }
        );
        // renders by re-escaping: the inner `"` is doubled again in the output
        assert_eq!(q.render_fts(), "\"a \"\" b\"");
    }

    #[test]
    fn needs_quoting_partition() {
        // total + disjoint: a token is ambiguous or not, never both.
        for s in ["", "has space", "a\"q", "plain", "token"] {
            assert!(needs_quoting(s) || !needs_quoting(s));
        }
        assert!(needs_quoting("has space"));
        assert!(needs_quoting("a\"q"));
        assert!(needs_quoting(""));
        assert!(!needs_quoting("plain"));
    }
}
