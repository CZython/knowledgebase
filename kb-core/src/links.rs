//! links — wikilink / path reference extraction + style classification (M2).
//!
//! §6/§8 (`links` table) + §9 (Verification):
//!
//! > `links` — **no duplicate edges**; **style classification correct**;
//! > style is `'wikilink' | 'relative' | 'absolute'`.
//!
//! ## What gets extracted (v1, deliberate and conservative)
//!
//! - **Wikilinks** — `[[ … ]]` (a target between a double-bracket pair, no
//!   newline inside the target). Target = the inner text, trimmed. This is
//!   the *display-id / slug* form used by `[[LLM]]` and
//!   `[[Transformer (architecture)]]`.
//! - **Path refs** — a whitespace-delimited token that looks like a path:
//!   contains a `/`, or ends with a known file extension. Classified as
//!   **`absolute`** if it starts with `/`, else **`relative`**. (A bare word
//!   like `token` is *not* a path and is not extracted.)
//!
//! Wikilinks and path refs are the two ref kinds that carry meaning in a
//! page's body. (Inline-code spans are scanned as plain text in v1; the
//! classification rule above still applies to the token inside.)
//!
//! The *code* is here; the spec (`spec/links_spec.rs`) proves the style is
//! a clean 3-way exclusive classification and that duplicate edge rows are
//! dropped. Graph invariants (inverse/backlink, no self-loops) live in
//! `graph.rs` + `spec/graph_spec.rs`.

use std::collections::BTreeSet;

/// How a reference was written (mirrors the `links.style` column).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LinkStyle {
    /// `[[ … ]]` — a display-id / slug reference.
    Wikilink,
    /// A path not starting with `/` (e.g. `papers/foo.pdf`, `../rust/gateway`).
    Relative,
    /// A path starting with `/` (e.g. `/home/u/notes/foo.md`).
    Absolute,
}

impl LinkStyle {
    /// The `links.style` column value for this style (exactly the enum's
    /// variant name, all-lowercase, matching the §8 SQL comment).
    #[inline]
    pub fn as_str(&self) -> &'static str {
        match self {
            LinkStyle::Wikilink => "wikilink",
            LinkStyle::Relative => "relative",
            LinkStyle::Absolute => "absolute",
        }
    }

    #[inline]
    pub fn parse(s: &str) -> Option<LinkStyle> {
        match s {
            "wikilink" => Some(LinkStyle::Wikilink),
            "relative" => Some(LinkStyle::Relative),
            "absolute" => Some(LinkStyle::Absolute),
            _ => None,
        }
    }
}

/// A single reference extracted from a page body: the target text and the
/// style it was written as. (The *resolved* endpoint — `abs_path` — is filled
/// in by the M5 indexer via `kb resolve`; here it is the raw written form.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The written target (wikilink inner text, or the path token).
    pub target: String,
    pub style: LinkStyle,
}

/// A known file extension that makes a bare token "look like a path" (the
/// path-ref heuristic). Kept small and deliberate.
const PATH_EXTS: &[&str] = &[
    "md", "mdx", "markdown", "txt", "pdf", "epub", "csv", "json", "yaml", "yml", "toml",
    "rs", "py", "js", "ts", "go", "c", "h", "cpp", "hpp", "sql", "sh", "html", "css",
];

/// True if `token` looks like a filesystem path: contains a `/`, or ends
/// with a known extension (case-insensitive). A single word like `token`
/// or `attention` is *not* a path. (Public: reused by `labels` for its file
/// list.)
pub fn is_pathlike(token: &str) -> bool {
    if token.contains('/') {
        return true;
    }
    for ext in PATH_EXTS {
        let e = format!(".{ext}");
        if token.len() > e.len() && token.to_lowercase().ends_with(e.as_str()) {
            return true;
        }
    }
    false
}

/// Classify a bare path token into Relative/Absolute (absolute iff it starts
/// with `/`). Returns `None` when the token is not path-like (callers skip
/// it).
fn classify_token(token: &str) -> Option<LinkStyle> {
    if is_pathlike(token) {
        if token.starts_with('/') {
            Some(LinkStyle::Absolute)
        } else {
            Some(LinkStyle::Relative)
        }
    } else {
        None
    }
}

/// Extract all reference links from a page body.
///
/// Scans left-to-right. For each `[[ … ]]` span: emit a `Wikilink` with the
/// trimmed inner text (if non-empty). Otherwise, walk the text token by
/// token (split on whitespace); for each path-like token, emit a
/// `Relative`/`Absolute` ref. Order = document order. No dedupe here — the
/// caller (`graph::dedup`) collapses repeated `(from,to,style)` rows per the
/// `links` PK (§9 no-duplicate-edges).
///
/// # Invariants (mirrored by `spec/links_spec.rs`)
///
/// - Every emitted style is one of the three and is *consistent with the
///   target*: a wikilink target is never classified as a path, and an
///   absolute target starts with `/` while a relative one does not.
/// - An absolute style implies the target begins with `/`; a relative style
///   implies it does not. (style ⟺ target shape — the "classification
///   correct" contract.)
pub fn extract_links(body: &str) -> Vec<Link> {
    let mut out: Vec<Link> = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'[' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // found `[[` at i; scan for the closing `]]` (no newline inside)
            i += 2;
            let start = i;
            while i < bytes.len() && !(bytes[i] == b']' && i + 1 < bytes.len() && bytes[i + 1] == b']') {
                if bytes[i] == b'\n' {
                    break; // wikilink does not span a newline
                }
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b']' && i + 1 < bytes.len() && bytes[i + 1] == b']' {
                let inner = body[start..i].trim().to_string();
                if !inner.is_empty() {
                    out.push(Link { target: inner, style: LinkStyle::Wikilink });
                }
                i += 2; // skip `]]`
            } else {
                // no closing ]] before a newline/end-of-text: the `[[` was
                // decorative. We've already consumed the `[[` (i is at the
                // newline or end); continue scanning after it as plain text.
            }
        } else {
            // gather a whitespace-delimited token
            let token_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if token_start < i {
                let token = &body[token_start..i];
                if let Some(style) = classify_token(token) {
                    out.push(Link { target: token.to_string(), style });
                }
            }
            // advance past trailing whitespace so we don't loop forever
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
        }
    }
    out
}

/// Dedupe a set of links so that no `(target, style)` pair repeats — the
/// `links` PK (`from_path, to_path, style`) means a repeated edge is a single
/// row. `from_path` is supplied by the caller (it's the source page) and is
/// constant here; we dedupe on the (target, style) axis. First occurrence
/// order is preserved.
pub fn dedup(links: Vec<Link>) -> Vec<Link> {
    // SECURITY_REPORT 3.16: `seen` was a `Vec` with `seen.iter().any(…)` —
    // O(L²) for hub/index pages with many outgoing links. A `BTreeSet` keyed
    // on (target, style) makes each lookup O(log L). First-occurrence order
    // is preserved by only pushing to `out` on a fresh key.
    let mut out: Vec<Link> = Vec::new();
    let mut seen: BTreeSet<(String, LinkStyle)> = BTreeSet::new();
    for l in links {
        let key = (l.target.clone(), l.style);
        if seen.insert(key) {
            out.push(l);
        }
    }
    out
}

// =============================================================================
// Unit tests — pin exec `links` to the spec shape.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- style classification (style ⟺ target shape) ----------------------

    #[test]
    fn absolute_style_implies_leading_slash() {
        let links = extract_links("see /home/u/notes/foo.md for detail");
        let l = &links[0];
        assert_eq!(l.style, LinkStyle::Absolute);
        assert!(l.target.starts_with('/'));
    }

    #[test]
    fn relative_style_implies_no_leading_slash() {
        let links = extract_links("the paper is at papers/attention-is-all-you-need.pdf");
        let l = &links[0];
        assert_eq!(l.style, LinkStyle::Relative);
        assert!(!l.target.starts_with('/'));
    }

    #[test]
    fn extension_token_is_relative() {
        let links = extract_links("book: deep-learning-goodfellow-ch3.epub");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].style, LinkStyle::Relative);
        assert_eq!(links[0].target, "deep-learning-goodfellow-ch3.epub");
    }

    #[test]
    fn slash_token_is_relative_even_without_ext() {
        let links = extract_links("impl lives in ../rust/gateway/");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].style, LinkStyle::Relative);
    }

    #[test]
    fn styles_are_exhaustive_and_named() {
        assert_eq!(LinkStyle::Wikilink.as_str(), "wikilink");
        assert_eq!(LinkStyle::Relative.as_str(), "relative");
        assert_eq!(LinkStyle::Absolute.as_str(), "absolute");
        // round-trips
        assert_eq!(LinkStyle::parse("wikilink"), Some(LinkStyle::Wikilink));
        assert_eq!(LinkStyle::parse("bogus"), None);
    }

    // ---- wikilink extraction ----------------------------------------------

    #[test]
    fn wikilink_bare_slug() {
        let links = extract_links("> tracked by the [[LLM]] label");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0], Link { target: "LLM".to_string(), style: LinkStyle::Wikilink });
    }

    #[test]
    fn wikilink_with_spaces_and_parens() {
        let links = extract_links("see [[Transformer (architecture)]] for detail");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Transformer (architecture)");
        assert_eq!(links[0].style, LinkStyle::Wikilink);
    }

    #[test]
    fn wikilink_and_path_coexist_in_order() {
        let body = "see [[LLM]] and the PDF at papers/a.pdf now";
        let links = extract_links(body);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].style, LinkStyle::Wikilink);
        assert_eq!(links[0].target, "LLM");
        assert_eq!(links[1].style, LinkStyle::Relative);
        assert_eq!(links[1].target, "papers/a.pdf");
    }

    #[test]
    fn wikilink_does_not_tokenize_inner_spans_as_paths() {
        // Even if the wikilink inner text contained a `/`, it stays a wikilink
        // (the target is the whole inner string), not a relative path.
        let links = extract_links("refer to [[a/b]] here");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0], Link { target: "a/b".to_string(), style: LinkStyle::Wikilink });
    }

    #[test]
    fn multiline_wikilink_is_not_extracted() {
        // A `[[` whose inner text spans a newline is not a wikilink.
        let body = "broken [[\nLLM]] label";
        let links = extract_links(body);
        assert!(
            !links.iter().any(|l| l.style == LinkStyle::Wikilink),
            "multiline [[ must not be a wikilink"
        );
    }

    #[test]
    fn bare_words_are_not_paths() {
        let links = extract_links("token bucket vs sliding window");
        assert!(links.is_empty(), "prose words must not be extracted as links");
    }

    // ---- dedupe (no duplicate (target,style) edges) ------------------------

    #[test]
    fn dedup_collapses_repeated_edges() {
        let mk = |t: &str, s: LinkStyle| Link { target: t.to_string(), style: s };
        let links = vec![
            mk("LLM", LinkStyle::Wikilink),
            mk("LLM", LinkStyle::Wikilink),
            mk("papers/a.pdf", LinkStyle::Relative),
            mk("papers/a.pdf", LinkStyle::Relative),
            mk("LLM", LinkStyle::Relative), // same target, DIFFERENT style → kept
        ];
        let d = dedup(links);
        assert_eq!(d.len(), 3);
        // first-occurrence order
        assert_eq!(d[0], mk("LLM", LinkStyle::Wikilink));
        assert_eq!(d[1], mk("papers/a.pdf", LinkStyle::Relative));
        assert_eq!(d[2], mk("LLM", LinkStyle::Relative));
    }

    #[test]
    fn dedup_is_idempotent() {
        let links = vec![
            Link { target: "x.pdf".into(), style: LinkStyle::Relative },
            Link { target: "x.pdf".into(), style: LinkStyle::Relative },
        ];
        let once = dedup(links);
        let twice = dedup(once.clone());
        assert_eq!(once, twice);
    }
}
