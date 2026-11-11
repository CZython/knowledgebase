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
    "md", "mdx", "markdown", "txt", "pdf", "epub", "csv", "json", "yaml", "yml", "toml", "rs",
    "py", "js", "ts", "go", "c", "h", "cpp", "hpp", "sql", "sh", "html", "css",
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
            while i < bytes.len()
                && !(bytes[i] == b']' && i + 1 < bytes.len() && bytes[i + 1] == b']')
            {
                if bytes[i] == b'\n' {
                    break; // wikilink does not span a newline
                }
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b']' && i + 1 < bytes.len() && bytes[i + 1] == b']' {
                let inner = body[start..i].trim().to_string();
                if !inner.is_empty() {
                    out.push(Link {
                        target: inner,
                        style: LinkStyle::Wikilink,
                    });
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
                    out.push(Link {
                        target: token.to_string(),
                        style,
                    });
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

// =============================================================================
// Extended link detection (DESIGN-links.md Phase 1).
//
// Three new reference *forms* are recognized on top of the v1 set:
//
//   `[ label ]( /path/to/page.md )` — a *path* link with a display text
//   `[ label ]( [ title ] )`        — a *db* link with a display text
//   `[ path ]`                      — a *path* link, no label
//   `[[ target ]]`                  — a *db* link (the v1 form, unchanged)
//   ``` `< https://… >`             — an *external* URI (never resolved)
//
// The detection is split into two pure functions, per the user's directive:
//
//   1. `detect_refs`       — **syntax only**: find a reference span, extract
//      the raw target string + an optional label, and *hint* at the kind
//      based on the *form* it was written as (wikilink / inline / plain /
//      external). No shape classification, no `#section` split, no corpus.
//      This is the part that *cannot* depend on `resolve.rs` (the corpus
//      does not matter for syntax), so it stays pure and independently
//      testable.
//
//   2. `classify_target`   — **path resolution, step 1**: take the raw target
//      string and decide its *kind* — Db vs Path vs Current (the `#` form).
//      Splits `#section` off the page-part. This is the part that *may*
//      want to grow more heuristics as the grammar evolves, so it is its
//      own function.
//
// (The actual page lookup — stage 1–4 against the corpus — stays in the
// shell's `annotated_links`; it depends on `resolver::Page`, which is a
// *separate* concern from these two primitives.)
//
// Why two functions? Each has a *different* failure mode and a different
// extension axis: `detect_refs` cares about *where and how long* the span
// is (the byte-offset arithmetic); `classify_target` cares about *what the
// target means* (the shape heuristics). Keeping them separate means a
// future grammar extension (e.g. a new link form, or a new kind in the
// classifier) only touches one function at a time, and each is pure and
// independently testable without the other.
// =============================================================================

/// A single reference occurrence, as *detected* (not yet resolved).
///
/// - `start` / `end` — byte indices into the enclosing line; the span
///   `[start..end)` is the **whole written reference** including the
///   wrappers (e.g. for `[[X]]`, `start` is the first `[` and `end` is
///   just after the last `]`). The *content* of the target — the
///   `target` field — is the inner (unwrapped) string.
/// - `target` — the raw target string (the part the resolver consumes).
/// - `label` — the optional display text, if the form carried one
///   (`[ label ]( target )`). `None` for `[[X]]`, `[ X ]`, `<url>`.
/// - `kind` — the *hinted* kind, derived from the written form (not from
///   the shape of the target). The shell's classifier (`classify_target`)
///   may override the `Inline` hint based on the inner shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawLink {
    /// Byte offset (into the enclosing line) where the reference starts.
    pub start: usize,
    /// Byte offset (into the enclosing line) where the reference ends
    /// (exclusive).
    pub end: usize,
    /// The raw target string (the inner part, unwrapped of its form).
    pub target: String,
    /// The optional display text (the inline label). `None` when the
    /// written form carried no label.
    pub label: Option<String>,
    /// The *hinted* kind, from the written form.
    pub form: TargetKind,
}

/// The written form of a reference — *how* it was spelled, before any
/// shape-based classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    /// `[[ target ]]` — a wikilink (v1 form).
    Wikilink,
    /// `[ label ]( target )` — an inline link; the *target* may be a path
    /// or a db title (decided by `classify_target`).
    Inline,
    /// `[ target ]` — a plain path (the *new* form, v1+), no label.
    Plain,
    /// `< scheme:… >` — an external URI (a `scheme:` prefix is required —
    /// see [`is_uri_like`]).
    External,
}

/// True iff `s` looks like an external URI: starts with `scheme:` where
/// the scheme is 2+ ASCII chars, the **first** of which must be a letter
/// (RFC 3986 rule — `123:456` is not a URI), followed by a non-whitespace
/// remainder.  Deliberate: `a < b` (math) and `<foo>` (bare word) are not
/// URIs and are not extracted.
pub fn is_uri_like(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0usize;
    // scheme: first char must be a letter (RFC 3986), the remainder is
    // alphanumeric or `-`/`+`/`.`; minimum 2 chars to rule out `a:b`-style
    // fragments.  We accept a wider range than RFC's 64-char max for
    // forward-compatibility.
    while i < b.len() && !b[i].is_ascii_whitespace() {
        i += 1;
    }
    let Some(colon) = b.iter().position(|&c| c == b':') else {
        return false;
    };
    if colon < 2 {
        return false;
    }
    let scheme = &b[..colon];
    let ok_scheme = scheme.iter().enumerate().all(|(i, &c)| {
        if i == 0 {
            c.is_ascii_alphabetic()
        } else {
            c.is_ascii_alphanumeric() || c == b'-' || c == b'+' || c == b'.'
        }
    });
    if !ok_scheme {
        return false;
    }
    // require at least one more non-whitespace char after the colon
    colon + 1 < i && !b[colon + 1..i].iter().any(|c| c.is_ascii_whitespace())
}

/// Extract every reference span in `line` (in document order). This is the
/// *syntax-only* half of the two-function split; it is corpus-free and can
/// be tested in isolation.
///
/// Precedence (left-to-right):
/// 1. `[[ target ]]` — `[[` wins (v1 behaviour, unchanged).
/// 2. `[ … ]` — read to the matching `]`; peek: if the next non-whitespace
///    is `(`, it is the inline form `[ label ]( target )` and the text
///    inside `()` is the target (and the bracketed text is the label);
///    otherwise the bracketed text is the target (a *path* — the new form).
/// 3. `< … >` — read to the first `>`; if the content is a well-formed URI
///    (per [`is_uri_like`]) → external; otherwise *not a link* (skip).
///
/// A lone `[` with no closing `]` before a newline is decorative (skip),
/// mirroring the v1 `[[` fallthrough.
pub fn detect_refs(line: &str) -> Vec<RawLink> {
    let mut out: Vec<RawLink> = Vec::new();
    let bytes = line.as_bytes();
    let n = bytes.len();
    let mut i = 0usize;
    while i < n {
        let c = bytes[i];
        if c == b'[' && i + 1 < n && bytes[i + 1] == b'[' {
            // ---- `[[ … ]]` (wikilink) -------------------------------------
            i += 2;
            let content_start = i;
            let mut found_close = false;
            while i < n {
                if bytes[i] == b']' && i + 1 < n && bytes[i + 1] == b']' {
                    found_close = true;
                    break;
                }
                if bytes[i] == b'\n' {
                    break; // wikilinks do not span a newline
                }
                i += 1;
            }
            if found_close {
                let target = line[content_start..i].trim().to_string();
                if !target.is_empty() {
                    out.push(RawLink {
                        start: content_start - 2, // the opening `[` of `[[`
                        end: i + 2,               // just past the closing `]]`
                        target,
                        label: None,
                        form: TargetKind::Wikilink,
                    });
                }
                i += 2;
            }
            // no close → decorative; i is at the newline or end — continue.
        } else if c == b'[' {
            // ---- `[ … ]` (plain or inline) --------------------------------
            // read to the matching `]`
            let open = i;
            i += 1;
            let content_start = i;
            let mut found_close = false;
            while i < n {
                if bytes[i] == b']' {
                    found_close = true;
                    break;
                }
                if bytes[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if !found_close {
                continue; // decorative — keep scanning
            }
            let label = line[content_start..i].trim().to_string();
            let close_bracket = i;
            i += 1; // past `]`
                    // peek past whitespace for `(`
            let mut j = i;
            while j < n && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < n && bytes[j] == b'(' {
                // inline form: `[ label ]( target )`
                let paren_start = j + 1;
                let mut close = false;
                let mut k = paren_start;
                while k < n {
                    if bytes[k] == b')' {
                        close = true;
                        break;
                    }
                    if bytes[k] == b'\n' {
                        break;
                    }
                    k += 1;
                }
                if close {
                    let target = line[paren_start..k].trim().to_string();
                    if !target.is_empty() {
                        out.push(RawLink {
                            start: open, // the opening `[`
                            end: k + 1,  // just past `)`
                            target,
                            label: if label.is_empty() { None } else { Some(label) },
                            form: TargetKind::Inline,
                        });
                    }
                    i = k + 1;
                }
            } else {
                // plain form: `[ target ]`
                let target = label; // the bracketed text is the target
                if !target.is_empty() {
                    out.push(RawLink {
                        start: open,
                        end: close_bracket + 1,
                        target,
                        label: None,
                        form: TargetKind::Plain,
                    });
                }
            }
        } else if c == b'<' {
            // ---- `< … >` (external URI) ----------------------------------
            // Delimiter style: `< https://… >` — padding spaces between the
            // brackets and the URI are trimmed off; the *target* itself must
            // have no whitespace (`is_uri_like`). `a < b` (math) trims to
            // `a` / `b` — not a URI — so it is not a link (stays prose).
            let open = i;
            i += 1;
            let mut found_close = false;
            while i < n {
                if bytes[i] == b'>' {
                    found_close = true;
                    break;
                }
                if bytes[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if found_close {
                let inner = line[open + 1..i].trim().to_string();
                if !inner.is_empty() && is_uri_like(&inner) {
                    out.push(RawLink {
                        start: open,
                        end: i + 1,
                        target: inner,
                        label: None,
                        form: TargetKind::External,
                    });
                }
                i += 1; // past `>`
            }
        } else {
            // plain character — advance one
            i += 1;
        }
    }
    out
}

/// A *classified* target — the output of the second half of the split
/// (`classify_target`). This is what the shell's resolver consumes when it
/// looks the target up in the corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ref {
    /// The raw written target (unchanged from `RawLink.target`).
    pub target: String,
    /// The optional display text (the inline label).
    pub label: Option<String>,
    /// The *kind* the target is to be resolved as:
    /// - `Db` — look up the corpus by display-id / slug / path / substring
    ///   (the `resolver::resolve` waterfall). Used for `[[X]]`, `[X]`,
    ///   `[label]( [X] )`.
    /// - `Path` — a path-shaped target; resolved by `resolver::resolve` but
    ///   *classified* as a path link (the `links.style` column = `relative`
    ///   or `absolute`). Used for `[[X]]` when `X` is path-shaped? No — see
    ///   below: a path target *always* classifies as `Path`.
    /// - `External` — an external URI (never looked up; rendered as the
    ///   bare URI).
    ///
    /// NOTE: `#section` is a *target-level* annotation, carried in `section`,
    /// and is *orthogonal* to the kind — a db target can have a section, a
    /// path target can have a section, etc. (DESIGN-links.md §grammar:
    /// "`#section` layers on top of either").
    pub kind: LinkKind,
    /// The optional `#section` (if the target carried one).
    pub section: Option<String>,
}

/// The four kinds a *classified* target resolves to (the "mode" column of
/// DESIGN-links.md).
///
/// `Wikilink` is the v1 form (`[[ … ]]`) — it is classified as a *db*
/// reference *or* a *path* reference depending on the target's shape.
/// (v1 `extract_links` emitted it as `LinkStyle::Wikilink` because it did
/// not split the shape; the new classification *does* split it.)
///
/// `Inline` is `[ label ]( target )` — the *shape* of the inner target
/// decides Path vs Db.
///
/// `Plain` is `[ target ]` (no label) — a path link, *always* (the form
/// itself says "this is a path").
///
/// `External` is `< scheme:… >` — an external URI (not a page reference at
/// all, but a link the reader can copy).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LinkKind {
    /// `[[ target ]]` — a db reference (the v1 form).
    Db,
    /// `[ label ]( path )` or `[ target ]` — a path link (resolved via
    /// `resolver::resolve` at stage 3, but *classified* as a path for the
    /// `links.style` column).
    Path,
    /// `< https://… >` — an external URI (never resolved).
    External,
    /// `[ # section ]` — a *current-page* section reference (no page to look
    /// up; the section is the page's own section). The `kind` is *always*
    /// `Path` for this (it is the *page's own* path, with a section).
    Current,
}

impl LinkKind {
    /// The render-style string (for the TUI palette's `link` role, the
    /// CLI's `style` column, and the JSON's `style` key). Derived from the
    /// kind + (for `Db`/`Path`) the target's shape (a leading `/` → absolute).
    pub fn style_str(&self, target: &str) -> &'static str {
        match self {
            LinkKind::Db => "wikilink", // the v1 `links.style` value for `[[…]]`
            LinkKind::Path => {
                if target.starts_with('/') {
                    "absolute"
                } else {
                    "relative"
                }
            }
            LinkKind::External => "external", // a new value — never a page
            LinkKind::Current => "current",   // a new value — the page itself
        }
    }
}

/// Classify a raw target (the output of `detect_refs`) into a `Ref` —
/// the *kind* to resolve it under, plus the `#section` split.
///
/// # Rules
///
/// - Target wrapped *exactly* as `[ … ]` (a single pair of brackets, no
///   inner whitespace-only trim ambiguity) → `Db` (the *db* form). The
///   target's *inner* text (brackets stripped) is the db lookup key.
/// - Target starts with `#` (after trim) → `Current` + section = the
///   `#`-stripped remainder. (The page is the one being read; only the
///   section is looked up, against the page's own sections — the TUI's
///   `open_linked` jumps to that heading's rendered row.)
/// - Target is path-shaped (per [`is_pathlike`]) → `Path`.
/// - Otherwise → `Db` (a *word* — the db lookup; this is the v1 bare-word
///   rule, unchanged).
///
/// The *label* passes through unchanged (it is the display text the
/// renderer uses when the target resolves).
pub fn classify_target(raw: &RawLink) -> Ref {
    // 0. `External` form — the scanner already decided this is a URI. A URI
    //    contains `/`, so shape-based classification would mis-fire as `Path`;
    //    the *written form* is authoritative here (a `<…>` that isn't a URI
    //    was never extracted, so an `External` raw is always a real link).
    if raw.form == TargetKind::External {
        return Ref {
            target: raw.target.clone(),
            label: raw.label.clone(),
            kind: LinkKind::External,
            section: None,
        };
    }
    let target = raw.target.trim();
    // 1. `#section` (a *current-page* anchor — the page is the one being
    //    read; the section is the page's own section).
    if let Some(section_str) = target.strip_prefix('#') {
        let section = section_str.trim().to_string();
        return Ref {
            target: raw.target.clone(),
            label: raw.label.clone(),
            kind: LinkKind::Current,
            section: if section.is_empty() {
                None
            } else {
                Some(section)
            },
        };
    }
    // 2. `[ … ]` (a single bracket pair around the target) → `Db` (the *db*
    //    lookup key is the inner text). The *inner* brackets are stripped —
    //    the resolver consumes the slug, not the wrapper.
    let is_bracketed = target.len() >= 2 && target.starts_with('[') && target.ends_with(']');
    if is_bracketed {
        let inner = target[1..target.len() - 1].trim().to_string();
        return Ref {
            target: raw.target.clone(),
            label: raw.label.clone(),
            kind: if inner.starts_with('/') || inner.contains('/') || is_pathlike(&inner) {
                LinkKind::Path
            } else {
                LinkKind::Db
            },
            section: None,
        };
    }
    // 3. Split off a `#section` (if any) — the page-part is the part *before*
    //    the `#`, the section is the part *after*. The section is kept
    //    verbatim (it may contain spaces — `Cost / result`).
    let (page_part, section) = match target.find('#') {
        Some(p) => (&target[..p], Some(target[p + 1..].trim().to_string())),
        None => (target, None),
    };
    let page_part = page_part.trim();
    // 4. path-shaped → `Path`; otherwise → `Db` (a word; the v1 bare-word
    //    rule, unchanged).
    let kind = if !page_part.is_empty()
        && (page_part.starts_with('/') || page_part.contains('/') || is_pathlike(page_part))
    {
        LinkKind::Path
    } else {
        LinkKind::Db
    };
    Ref {
        target: raw.target.clone(),
        label: raw.label.clone(),
        kind,
        section,
    }
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

/// Extract page-targeting reference *rows* from a body — the v2 scanner
/// (`detect_refs` + `classify_target`). Used by the M5 indexer to populate
/// the `links` table (page→page edges).
///
/// - `External` URIs and `Current`-page refs are **skipped** (they are not
///   page→page edges).
/// - Any trailing `#section` is **stripped** — the `links` table stores the
///   *page* target; the section is orthogonal (`Doc::resolve` handles it at
///   query time, not index time).
/// - Dedupes on `(target, style_str)`; first-occurrence order preserved.
///
/// Returns `(target, style)` pairs in document order.
pub fn extract_ref_rows(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for line in body.lines() {
        for raw in detect_refs(line) {
            let r = classify_target(&raw);
            match r.kind {
                LinkKind::External | LinkKind::Current => continue,
                LinkKind::Db | LinkKind::Path => {}
            }
            // Strip `#section` if present (the target is the page, not the
            // page+anchor).
            let t = r.target.trim();
            let page_part = match t.find('#') {
                Some(p) => t[..p].trim().to_string(),
                None => t.to_string(),
            };
            if page_part.is_empty() {
                continue;
            }
            let style = r.kind.style_str(&page_part);
            let key = (page_part, style.to_string());
            if seen.insert(key.clone()) {
                out.push(key);
            }
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
        assert_eq!(
            links[0],
            Link {
                target: "LLM".to_string(),
                style: LinkStyle::Wikilink
            }
        );
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
        assert_eq!(
            links[0],
            Link {
                target: "a/b".to_string(),
                style: LinkStyle::Wikilink
            }
        );
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
        assert!(
            links.is_empty(),
            "prose words must not be extracted as links"
        );
    }

    // ---- dedupe (no duplicate (target,style) edges) ------------------------

    #[test]
    fn dedup_collapses_repeated_edges() {
        let mk = |t: &str, s: LinkStyle| Link {
            target: t.to_string(),
            style: s,
        };
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
            Link {
                target: "x.pdf".into(),
                style: LinkStyle::Relative,
            },
            Link {
                target: "x.pdf".into(),
                style: LinkStyle::Relative,
            },
        ];
        let once = dedup(links);
        let twice = dedup(once.clone());
        assert_eq!(once, twice);
    }

    // ---- is_uri_like (DESIGN-links.md: `<…>` = external URI) --------------

    #[test]
    fn uri_like_accepts_scheme_colon_rest() {
        assert!(is_uri_like("https://arxiv.org/abs/1706.03762"));
        assert!(is_uri_like("http://a.b"));
        assert!(is_uri_like("mailto:x@y.z"));
        assert!(is_uri_like("ftp+ssh://host"));
    }

    #[test]
    fn uri_like_rejects_math_and_bare_words() {
        assert!(!is_uri_like("a < b")); // inner whitespace
        assert!(!is_uri_like("foo")); // no colon
        assert!(!is_uri_like("a:b")); // 1-char scheme
        assert!(!is_uri_like("123:456")); // numeric scheme head
        assert!(!is_uri_like("http:")); // nothing after the colon
        assert!(!is_uri_like("-ab:cd")); // `-` must not lead
    }

    // ---- detect_refs (syntax only, no corpus) -----------------------------

    #[test]
    fn refs_wikilink_span_is_the_full_brackets() {
        let l = "see [[ watchmaking guide ]] ok";
        let r = &detect_refs(l)[0];
        assert_eq!(r.form, TargetKind::Wikilink);
        assert_eq!(r.target, "watchmaking guide");
        assert_eq!(&l[r.start..r.end], "[[ watchmaking guide ]]");
    }

    #[test]
    fn refs_inline_carries_label_and_unwrapped_target() {
        let l = "a [watches cost](/mnt/notes/guide.md) b";
        let r = &detect_refs(l)[0];
        assert_eq!(r.form, TargetKind::Inline);
        assert_eq!(r.label.as_deref(), Some("watches cost"));
        assert_eq!(r.target, "/mnt/notes/guide.md");
        assert_eq!(&l[r.start..r.end], "[watches cost](/mnt/notes/guide.md)");
    }

    #[test]
    fn refs_plain_brackets_target_is_the_bracketed_text() {
        let l = "use [ /mnt/notes/guide.md ] for it";
        let r = &detect_refs(l)[0];
        assert_eq!(r.form, TargetKind::Plain);
        assert_eq!(r.label, None);
        assert_eq!(r.target, "/mnt/notes/guide.md");
        assert_eq!(&l[r.start..r.end], "[ /mnt/notes/guide.md ]");
    }

    #[test]
    fn refs_uri_only_when_well_formed() {
        assert_eq!(detect_refs("paper < https://x.io/y > here").len(), 1);
        assert!(detect_refs("a < b math stays prose").is_empty());
        assert!(detect_refs("<foo> is not a link").is_empty());
    }

    #[test]
    fn refs_decorative_brackets_are_skipped() {
        assert!(detect_refs("a [b never closed").is_empty());
        assert!(detect_refs("a [[b never closed").is_empty());
        assert!(detect_refs("plain list, no refs").is_empty());
    }

    #[test]
    fn refs_multiple_links_keep_document_order() {
        let l = "[w](/a.md) then [[ t ]] and < https://x.io >";
        let r = detect_refs(l);
        assert_eq!(
            r.iter().map(|x| x.form).collect::<Vec<_>>(),
            vec![
                TargetKind::Inline,
                TargetKind::Wikilink,
                TargetKind::External
            ]
        );
        assert!(r.windows(2).all(|w| w[0].end <= w[1].start));
    }

    // ---- classify_target (form + shape → kind, `#` split) -----------------

    fn cls(s: &str) -> Ref {
        classify_target(&detect_refs(s)[0])
    }

    #[test]
    fn classify_db_plain_and_wikilink() {
        assert_eq!(
            cls("[[watchmaking guide]]"),
            classify_target(&detect_refs("[[watchmaking guide]]")[0])
        );
        let w = cls("[[watchmaking guide]]");
        assert_eq!(w.kind, LinkKind::Db);
        assert_eq!(w.kind.style_str(&w.target), "wikilink");
        let bare = cls("[watchmaking]"); // bare word in brackets → db lookup
        assert_eq!(bare.kind, LinkKind::Db);
        let path = cls("[/watchmaking]"); // slash shape → path, not db
        assert_eq!(path.kind, LinkKind::Path);
    }

    #[test]
    fn classify_path_bare_and_inline() {
        let p = cls("[w](/mnt/x.md)");
        assert_eq!(p.kind, LinkKind::Path);
        assert_eq!(p.kind.style_str(&p.target), "absolute");
        let r = cls("[notes/a.md]");
        assert_eq!(r.kind, LinkKind::Path);
        assert_eq!(r.kind.style_str(&r.target), "relative");
    }

    #[test]
    fn classify_inline_bracketed_inner_is_db() {
        // `[ label ]( [ title ] )` — inner brackets make it a db lookup.
        let r = cls("[watches cost]( [ watchmaking guide ] )");
        assert_eq!(r.kind, LinkKind::Db);
        assert_eq!(r.label.as_deref(), Some("watches cost"));
    }

    #[test]
    fn classify_current_section_forms() {
        let c = cls("[# Cost / result]");
        assert_eq!(c.kind, LinkKind::Current);
        assert_eq!(c.section.as_deref(), Some("Cost / result"));
        assert_eq!(c.kind.style_str(&c.target), "current");
        let w = cls("[[watchmaking guide#Cost / result]]");
        assert_eq!(w.kind, LinkKind::Db);
        assert_eq!(w.section.as_deref(), Some("Cost / result"));
        let p = cls("[w](/mnt/x.md#Sec)");
        assert_eq!(p.kind, LinkKind::Path);
        assert_eq!(p.section.as_deref(), Some("Sec"));
    }

    #[test]
    fn classify_external_never_resolved() {
        let r = cls("< https://arxiv.org/abs/1706.03762 >");
        assert_eq!(r.kind, LinkKind::External);
        assert_eq!(r.section, None);
        assert_eq!(r.kind.style_str(&r.target), "external");
    }

    #[test]
    fn classify_is_idempotent_per_occurrence() {
        for s in [
            "[[a]]",
            "[x](/y.md)",
            "[x]([a b])",
            "[#sec]",
            "< https://x.io >",
        ] {
            let first = cls(s);
            let second = cls(s);
            assert_eq!(first, second);
        }
    }

    // ---- extract_ref_rows (v2 page→page edges for the `links` table) ------

    #[test]
    fn ref_rows_wikilink_is_page_edge() {
        let rows = extract_ref_rows("see [[watchmaking guide]] here");
        assert_eq!(rows, vec![("watchmaking guide".into(), "wikilink".into())]);
    }

    #[test]
    fn ref_rows_path_link_is_relative_or_absolute() {
        let rows = extract_ref_rows("a [notes/a.md] b [x](/home/u/x.md)");
        // both are Path — style is relative then absolute (document order).
        assert_eq!(
            rows,
            vec![
                ("notes/a.md".into(), "relative".into()),
                ("/home/u/x.md".into(), "absolute".into())
            ]
        );
    }

    #[test]
    fn ref_rows_external_and_current_are_skipped() {
        // External URI: skipped (not a page edge).
        let rows = extract_ref_rows("paper < https://arxiv.org > here");
        assert!(rows.is_empty(), "external URIs are not page→page edges");
        // Current-page ref `[ # sec ]`: skipped (resolves to the page itself).
        let rows = extract_ref_rows("see [ # Cost / result ] and nothing else");
        assert!(rows.is_empty(), "current-page refs are not page→page edges");
    }

    #[test]
    fn ref_rows_section_anchor_is_stripped() {
        // `#section` is an orthogonal target-level annotation — the `links`
        // table stores the *page* target (the section is resolved at query
        // time, not index time).
        let rows = extract_ref_rows("see [[watchmaking guide#Cost / result]]");
        assert_eq!(rows, vec![("watchmaking guide".into(), "wikilink".into())]);
        let rows = extract_ref_rows("a [notes/a.md#Sec] b");
        assert_eq!(rows, vec![("notes/a.md".into(), "relative".into())]);
    }

    #[test]
    fn ref_rows_dedupes_repeated_edges() {
        // Two `[[LLM]]` in the body → one row.
        let rows = extract_ref_rows("see [[LLM]] and also [[LLM]] again");
        assert_eq!(rows, vec![("LLM".into(), "wikilink".into())]);
        // `[[a.md]]` + `[a.md]` — both classify as `Path` with style
        // `relative` in v2 (a path-shaped inner string is a *path* ref; the
        // form does not override the shape classification here), so the two
        // edge rows are identical and dedupe into one.
        let rows = extract_ref_rows("[[a.md]] and [a.md]");
        assert_eq!(rows, vec![("a.md".into(), "relative".into())]);
    }

    #[test]
    fn ref_rows_bare_words_in_prose_are_not_rows() {
        // Under v2, a bare token like `papers/foo.pdf` (no brackets) is
        // *prose*, not a reference — only spelled-out link forms are rows.
        let rows = extract_ref_rows("the paper is at papers/foo.pdf");
        assert!(rows.is_empty(), "v2 retired bare-token paths: {rows:?}");
    }

    #[test]
    fn ref_rows_multiline_is_scanned_per_line() {
        // `detect_refs` is per-line; a `[[` spanning a newline is not a
        // wikilink, but a second line's link still extracts.
        let body = "line one [[not a link\nline two [[real]] here";
        let rows = extract_ref_rows(body);
        assert_eq!(rows, vec![("real".into(), "wikilink".into())]);
    }
}
