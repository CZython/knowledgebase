//! identity — slug resolution + `type:slug` display-id assembly (M1).
//!
//! This module is the *identity half* that `frontmatter` (metadata) feeds
//! into. It sits between `frontmatter::parse()` and the DB indexer (M5):
//!
//!   ```text
//!   .md file  →  frontmatter::parse()   (page_type, title, …)
//!                 + body scan for H1
//!                 ↓
//!   identity::resolve_slug(kind, title, h1, filename)
//!                 ↓
//!   identity::display_id(kind, slug)   =  type:slug
//!   ```
//!
//! ## Slug priority (PLAN.md §4)
//!
//! 1. Frontmatter `title` — if present and non-empty, it wins. (A page with
//!    a `title:` needs no H1.)
//! 2. The body's **H1** — the first `# heading` line, if any.
//! 3. The **filename** (sans extension).
//!
//! The chosen slug is trimmed, and (per the spec) *never empty* in a valid
//! page. The exec returns `Option<String>` so the caller can distinguish
//! "no slug found at all" (→ the page is malformed and the indexer warns)
//! from "found".
//!
//! ## Display id
//!
//! `display_id(kind, slug)` = `kind.prefix() ++ slug` — re-exposes the
//! `meta::NoteMeta::as_display_id()` contract in a *pure* form, with no
//! `NoteMeta` struct required. This is the string that goes into the
//! `pages.id` column and into `[[wikilink]]` targets.
//!
//! Verified: `spec/identity_spec.rs` proves the shape (each branch's output
//! equals the input it was chosen from, and the display id is exactly
//! `prefix ++ slug` — the same 3-branch proof pattern as `meta_spec.rs`).

use super::meta::NoteType;

/// Resolve the slug from the three candidate sources.
///
/// # Priority (PLAN.md §4)
///
/// 1. `title` — the frontmatter `title:` field (already the canonical value
///    if the caller used `frontmatter::Frontmatter::title`).
/// 2. `h1` — the first H1 heading in the body, if the body has one.
/// 3. `filename` — the base name *sans extension* (caller extracts it from
///    the `Path`; the extension is stripped before calling, per §4 "the
///    filename (minus extension)").
///
/// All three are trimmed; leading/trailing whitespace is dropped. An empty
/// value (or all-empty) returns `None` — the caller either warns (indexer)
/// or treats the page as malformed.
///
/// # Invariants (mirrored by `spec/identity_spec.rs`)
///
/// - If `title` trims to non-empty, the result is `Some(title.trim())`.
/// - Else, if `h1` trims to non-empty, the result is `Some(h1.trim())`.
/// - Else, if `filename` trims to non-empty, the result is `Some(filename.trim())`.
/// - Else, the result is `None`.
pub fn resolve_slug<'a>(
    title: Option<&'a str>,
    h1: Option<&'a str>,
    filename: Option<&'a str>,
) -> Option<&'a str> {
    if let Some(t) = title {
        let t = t.trim();
        if !t.is_empty() {
            return Some(t);
        }
    }
    if let Some(h) = h1 {
        let h = h.trim();
        if !h.is_empty() {
            return Some(h);
        }
    }
    if let Some(f) = filename {
        let f = f.trim();
        if !f.is_empty() {
            return Some(f);
        }
    }
    None
}

/// Assemble the display id `type:slug`. The caller is responsible for
/// having resolved the slug (see `resolve_slug`). If the slug is empty
/// (e.g. `Some("")`) the display id still assembles — a caller that needs
/// non-empty must enforce it themselves. (The exec parser already drops
/// empty slugs; this is a pure string function.)
pub fn display_id(kind: NoteType, slug: &str) -> String {
    format!("{}{}", kind.prefix(), slug)
}

/// True if `slug` is a well-formed display-name part (non-empty after trim).
/// Re-exported from `meta::NoteMeta::slug_is_well_formed` for symmetry with
/// the pure-function API surface of this module.
#[inline]
pub fn slug_is_well_formed(slug: &str) -> bool {
    !slug.trim().is_empty()
}

/// Extract a candidate H1 from a page's body. A *valid* H1 is the first
/// `# heading` line (exactly one `#`, not `##`, not `###`, …) *before* any
/// other content. The heading's text is returned trimmed. The function is
/// deliberately lenient: a body that starts with a blank line or a
/// blockquote followed by `# Foo` still finds the H1 (a real page may have
/// a frontmatter-adjacent separator, a leading blank, or a comment).
///
/// # Returns
///
/// `Some(text)` if the body's first *heading* is an H1 (level 1);
/// `None` otherwise (the body has no headings at all, or the first heading
/// is H2+).
///
/// Why "first heading, and it must be level 1"? Markdown spec: a page has
/// exactly *one* H1 (the title); anything else is a subsection. This
/// matches the `mdtool.py` behaviour the tool is porting from.
pub fn find_h1(body: &str) -> Option<&str> {
    // Trim leading blank / frontmatter-adjacent lines.
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        // A heading is a line that starts with `#`. An H1 is *exactly*
        // `# ` (one `#`, then a space). Deeper headings start with `##`.
        if t.starts_with('#') {
            if let Some(rest) = t.strip_prefix('#') {
                // `rest` is what follows the first `#`. If rest starts with
                // a space → H1; if it starts with another `#` → deeper.
                if rest.starts_with(' ') {
                    let heading = rest.trim_start();
                    if !heading.is_empty() {
                        return Some(heading);
                    }
                }
            }
            // A heading line that's not H1 → no H1 exists (first heading
            // already spoken). Stop.
            return None;
        }
        // A non-heading line before any heading — ignore (the H1 could come
        // later, though that's unusual; still, we only count the *first*
        // heading).
        // Actually, per the contract above, the H1 must be the FIRST heading
        // in the body. Content before it is allowed (lead-in paragraphs);
        // we keep scanning until we hit the first heading line.
    }
    None
}

/// Derive the "filename (minus extension)" candidate from a full path.
/// Accepts any path with a final component that has no extension (e.g.
/// `notes/transformer` → `transformer`) or *one* extension
/// (`notes/transformer.md` → `transformer`). Multiple dots (`v1.2.tar.gz`)
/// keep everything but the last extension (POSIX convention).
///
/// Returns the base component's name *sans* extension, or `None` if the
/// base component itself is just an extension (e.g. `.hidden` → `""` →
/// `None`).
pub fn filename_minus_extension(path: &str) -> Option<String> {
    // Find the final path component.
    let base = path.rsplit('/').next().unwrap_or("");
    let base = base.trim();
    if base.is_empty() {
        return None;
    }
    // Strip one trailing extension at the *last* '.'.
    let dot = match base.rfind('.') {
        Some(d) => d,
        None => return Some(base.to_string()),
    };
    // A leading '.' (dotfile) isn't an extension to strip; it's the
    // filename itself.
    if dot == 0 {
        return Some(base.to_string());
    }
    let name = &base[..dot];
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

// =============================================================================
// Unit tests — the "drift-check" that pins exec `identity` to the spec.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- slug priority (PLAN.md §4): title → H1 → filename -------------

    #[test]
    fn title_wins_over_h1_and_filename() {
        let s = resolve_slug(Some("My Title"), Some("H1 Heading"), Some("file.md")).unwrap();
        assert_eq!(s, "My Title");
    }

    #[test]
    fn h1_wins_over_filename_when_no_title() {
        let s = resolve_slug(None, Some("H1 Heading"), Some("file.md")).unwrap();
        assert_eq!(s, "H1 Heading");
    }

    #[test]
    fn filename_when_no_title_no_h1() {
        let s = resolve_slug(None, None, Some("transformer")).unwrap();
        assert_eq!(s, "transformer");
    }

    #[test]
    fn empty_title_falls_through_to_h1() {
        let s = resolve_slug(Some("   "), Some("H1"), Some("f")).unwrap();
        assert_eq!(s, "H1");
    }

    #[test]
    fn empty_title_and_h1_fall_through_to_filename() {
        let s = resolve_slug(Some(""), None, Some("f")).unwrap();
        assert_eq!(s, "f");
    }

    #[test]
    fn all_empty_returns_none() {
        assert_eq!(resolve_slug(None, None, None), None);
        assert_eq!(resolve_slug(Some(""), Some(" "), Some("")), None);
    }

    // ---- filename (minus extension) ------------------------------------

    #[test]
    fn strip_single_extension() {
        let s = filename_minus_extension("notes/transformer.md").unwrap();
        assert_eq!(s, "transformer");
    }

    #[test]
    fn dotfile_keeps_leading_dot_after_stripping_final_ext() {
        // `.hidden.md` → strip the *last* extension only → `.hidden`.
        // (Only the trailing extension is removed; a leading dot at
        // position 0 is part of the name, not an extension.)
        let s = filename_minus_extension(".hidden.md").unwrap();
        assert_eq!(s, ".hidden");
    }

    #[test]
    fn multiple_extensions_strip_only_last() {
        // v1.2.tar.gz → v1.2.tar (strip `.gz` only).
        let s = filename_minus_extension("pkg/v1.2.tar.gz").unwrap();
        assert_eq!(s, "v1.2.tar");
    }

    #[test]
    fn no_extension_returns_name() {
        let s = filename_minus_extension("notes/transformer").unwrap();
        assert_eq!(s, "transformer");
    }

    // ---- find_h1 ---------------------------------------------------------

    #[test]
    fn first_h1_is_used() {
        let body = "# The Title\n\nintro text\n\n## Subsection\n\nmore";
        assert_eq!(find_h1(body), Some("The Title"));
    }

    #[test]
    fn leading_blank_lines_tolerated() {
        let body = "\n\n# The Title\n\ntext";
        assert_eq!(find_h1(body), Some("The Title"));
    }

    #[test]
    fn no_h1_when_first_heading_is_h2() {
        let body = "intro\n\n## First Subsection\n\n## Second";
        assert_eq!(find_h1(body), None);
    }

    #[test]
    fn no_heading_at_all() {
        let body = "just some text\n\nand more";
        assert_eq!(find_h1(body), None);
    }

    // ---- display_id -------------------------------------------------------

    #[test]
    fn display_id_is_prefix_plus_slug() {
        assert_eq!(display_id(NoteType::Label, "LLM"), "label:LLM");
        assert_eq!(
            display_id(NoteType::Wiki, "Transformer (architecture)"),
            "wiki:Transformer (architecture)"
        );
        assert_eq!(
            display_id(NoteType::Note, "API Gateway — notes"),
            "note:API Gateway — notes"
        );
    }

    #[test]
    fn slug_is_well_formed_matches_meta() {
        assert!(!slug_is_well_formed(""));
        assert!(!slug_is_well_formed("   "));
        assert!(slug_is_well_formed("LLM"));
    }

    // ---- full pipeline: parse → slug → display id ------------------------

    #[test]
    fn pipeline_a1() {
        // A.1 (wiki: Transformer (architecture)): title in frontmatter wins.
        use crate::frontmatter::parse as fm_parse;
        let page = concat!(
            "---\n",
            "type: wiki\n",
            "title: Transformer (architecture)\n",
            "description: How the Transformer works.\n",
            "tags:\n  - transformer\n  - llm\n",
            "---\n",
            "\n# Transformer (architecture)\n\nThe Transformer is…\n",
        );
        let fm = fm_parse(page);
        assert!(fm.is_indexable());
        assert_eq!(fm.page_type, Some(NoteType::Wiki));
        let h1 = find_h1(&fm.body);
        let file = filename_minus_extension("notes/transformer.md").unwrap();
        let slug = resolve_slug(fm.title.as_deref(), h1, Some(&file)).unwrap();
        assert_eq!(slug, "Transformer (architecture)"); // title won
        let id = display_id(NoteType::Wiki, slug);
        assert_eq!(id, "wiki:Transformer (architecture)");
    }

    #[test]
    fn pipeline_a1b() {
        // A.1b (label: Transformer (papers)): title wins; slug has a space +
        // parens — the display id must survive it exactly.
        use crate::frontmatter::parse as fm_parse;
        let page = concat!(
            "---\n",
            "type: label\n",
            "title: Transformer (papers)\n",
            "tags:\n  - transformer\n",
            "---\n",
            "\n# Transformer (papers)\n\npapers/attention-is-all-you-need.pdf\n",
        );
        let fm = fm_parse(page);
        assert_eq!(fm.page_type, Some(NoteType::Label));
        let h1 = find_h1(&fm.body);
        let file = filename_minus_extension("library/transformer.md").unwrap();
        assert_eq!(file, "transformer");
        let slug = resolve_slug(fm.title.as_deref(), h1, Some(&file)).unwrap();
        assert_eq!(slug, "Transformer (papers)");
        let id = display_id(NoteType::Label, slug);
        assert_eq!(id, "label:Transformer (papers)");
    }
}
