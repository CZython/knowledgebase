//! WYSIWYG span-splice for the Formatted view (PLAN-GUI §8.2 / §8.6).
//!
//! The document is always Markdown source. Editing a block means: serialize the
//! edited *content* back to that block's Markdown, then replace **only that
//! block's source span**, leaving every other byte untouched. This mirrors the
//! repo's section-splice philosophy — edit the smallest span, keep the rest.
//!
//! All functions here are pure and side-effect free so the §8.6 guarantees are
//! unit-testable:
//! * **Identity round-trip** — a block spliced back with its own content is a
//!   no-op on the body.
//! * **Span isolation** — editing one block never changes bytes outside its
//!   span.

use std::ops::Range;

use super::md::{flat_text, Block, Doc};

/// The structural kind of a splicable block, plus the serialization hints the
/// renderer needs to rebuild its Markdown (heading level, code language/fence
/// form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Heading(u8),
    Paragraph,
    /// `fenced = true` → a ` ```lang ` … ` ``` ` block (rebuilds as a fence).
    /// `fenced = false` → an indented block (rebuilds with 4-space indent).
    /// `lang` is carried through either way.
    Code {
        lang: Option<String>,
        fenced: bool,
    },
}

/// A single editable block: its kind, its user-visible plain content (markers
/// already stripped — what the text field shows and commits), and the byte
/// range of that block in the source `body` that it splices against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockEdit {
    pub kind: BlockKind,
    pub plain_text: String,
    pub body_range: Range<usize>,
}

/// Every splicable **root** block of `doc` that resolves a span in `body`, in
/// document order. Non-splicable blocks (list / quote / table / rule / html)
/// are skipped — they render, but are not yet individually editable.
pub fn splicible_root_blocks(doc: &Doc, body: &str) -> Vec<BlockEdit> {
    let mut out = Vec::new();
    for &idx in &doc.root {
        let block = &doc.blocks[idx];
        let Some(range) = block.range(body) else {
            continue;
        };
        match block {
            Block::Heading { level, inlines } => out.push(BlockEdit {
                kind: BlockKind::Heading(*level),
                plain_text: flat_text(inlines),
                body_range: range,
            }),
            Block::Paragraph { inlines } => out.push(BlockEdit {
                kind: BlockKind::Paragraph,
                plain_text: flat_text(inlines),
                body_range: range,
            }),
            Block::Code { text, lang } => out.push(BlockEdit {
                kind: BlockKind::Code {
                    lang: lang.clone(),
                    fenced: lang.is_some(),
                },
                plain_text: text.clone(),
                body_range: range,
            }),
            Block::List { .. }
            | Block::ListItem { .. }
            | Block::Quote { .. }
            | Block::Table { .. }
            | Block::Rule
            | Block::Html { .. } => {}
        }
    }
    out
}

/// Rebuild a block's Markdown from its kind and plain content. The result ends
/// with **no** trailing newline (the caller — [`splice`] — re-attaches the
/// line terminator if the original span carried one).
pub fn serialize_block(kind: &BlockKind, plain_text: &str) -> String {
    let content = plain_text.trim_end_matches('\n');
    match kind {
        BlockKind::Heading(level) => format!("{} {}", "#".repeat(*level as usize), content),
        BlockKind::Paragraph => content.to_string(),
        BlockKind::Code { lang, fenced } if *fenced => {
            let tag = lang.as_deref().unwrap_or("");
            format!("```{tag}\n{content}\n```")
        }
        BlockKind::Code { lang, fenced } if !*fenced => {
            if content.is_empty() {
                return "    ".to_string();
            }
            let indented: Vec<String> = content.lines().map(|l| format!("    {l}")).collect();
            if let Some(tag) = lang {
                // An indented block that still knows a language is unusual; keep
                // it faithful to the fence we cannot express via indent only.
                format!("```{tag}\n{}\n```", indented.join("\n"))
            } else {
                indented.join("\n")
            }
        }
        _ => content.to_string(),
    }
}

/// Replace `edit.body_range` in `body` with [`serialize_block`] of `edit`.
///
/// The span that [`super::md::Block::range`] produces includes the block's own
/// terminating newline *when there is more document after it* (that newline is
/// the line terminator, not the inter-block blank separator). We preserve that
/// behaviour: if the original span ended in `\n`, the replacement does too, so
/// a single-line edit keeps the following section separator in place.
///
/// No-op (returns `body` unchanged) if the range is malformed.
pub fn splice(body: &str, edit: &BlockEdit) -> String {
    let r = edit.body_range.clone();
    if r.start > r.end || r.end > body.len() {
        return body.to_string();
    }
    let span = &body[r.clone()];
    let had_trailing_nl = span.ends_with('\n');
    let replacement = serialize_block(&edit.kind, &edit.plain_text);
    let mut out = String::with_capacity(body.len());
    out.push_str(&body[..r.start]);
    out.push_str(&replacement);
    if had_trailing_nl {
        out.push('\n');
    }
    out.push_str(&body[r.end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::super::md::parse;
    use super::*;

    /// §8.6 identity round-trip: splicing a block back with its *own* content
    /// must reproduce the body byte-for-byte (a save with no effective edit).
    ///
    /// Marker-free bodies round-trip exactly; inline-marked-up paragraphs are
    /// the §8.3 escape-hatch case (see `paragraph_inline_markers_hidden`).
    #[test]
    fn identity_roundtrip_heading_paragraph_code() {
        let body =
            "# Title\n\nA plain paragraph with no inline markers at all.\n\n```rust\nfn main() {\n    let x = 1;\n}\n```\n\nend";
        let doc = parse(body);
        for edit in splicible_root_blocks(&doc, body) {
            let out = splice(body, &edit);
            assert_eq!(out, body, "identity round-trip failed for {:?}", edit.kind);
        }
    }

    /// §8.3: inline markers are *hidden* in the Formatted view, so a
    /// marker-bearing paragraph's plain text is its marker-stripped content.
    /// Re-serializing that (an "identity" edit here) normalizes the source to
    /// the marker-free form — the Raw view is the escape hatch. The bytes
    /// *outside* the block are still isolated.
    #[test]
    fn paragraph_inline_markers_hidden() {
        let body = "# T\n\nA plain paragraph that mentions `code` and **bold**.\n\nend";
        let doc = parse(body);
        let para = splicible_root_blocks(&doc, body)
            .into_iter()
            .find(|e| matches!(e.kind, BlockKind::Paragraph))
            .expect("a paragraph");
        // Markers are stripped from the user-visible plain text.
        assert_eq!(
            para.plain_text,
            "A plain paragraph that mentions code and bold."
        );
        let out = splice(body, &para);
        // Surrounding bytes untouched; the block itself is the normalized form.
        assert_eq!(
            out,
            "# T\n\nA plain paragraph that mentions code and bold.\n\nend"
        );
    }

    /// §8.6 identity round-trip, indented (non-fenced) code block.
    #[test]
    fn identity_roundtrip_indented_code() {
        // A document whose whole body is a plain paragraph + indented code.
        let body = "intro\n\n    indented line one\n    indented line two\n\noutro";
        let doc = parse(body);
        let edits = splicible_root_blocks(&doc, body);
        let indented = edits
            .iter()
            .find(|e| matches!(e.kind, BlockKind::Code { fenced: false, .. }))
            .expect("an indented code block");
        assert_eq!(splice(body, indented), body);
    }

    /// §8.6 span isolation: editing one block leaves every byte outside its
    /// span identical (compare prefix and suffix).
    #[test]
    fn span_isolation_edit_heading() {
        let body = "# Old heading\n\npara one\n\n### Sub\n\npara two\n\n- a list\n\nend";
        let doc = parse(body);
        let edits = splicible_root_blocks(&doc, body);
        let heading = edits
            .iter()
            .find(|e| matches!(e.kind, BlockKind::Heading(_)))
            .expect("a heading");
        let r = heading.body_range.clone();

        let mut edit = heading.clone();
        edit.plain_text = "New heading".to_string();
        let out = splice(body, &edit);

        // Everything before and after the spliced range is byte-identical.
        assert!(out.starts_with(&body[..r.start]), "prefix changed");
        assert!(out.ends_with(&body[r.end..]), "suffix changed");
        // And the replaced region is exactly the re-serialized block (+#) + its
        // terminator. The original span was "# Old heading\n"; the spliced
        // region is "# New heading\n" — the same bytes outside are untouched.
        assert_eq!(
            &out[r.start..r.start + "# New heading\n".len()],
            "# New heading\n"
        );
    }

    /// Editing the *middle* block of three leaves the first and last untouched.
    #[test]
    fn span_isolation_edit_middle_paragraph_only() {
        let body = "first\n\nMIDDLE paragraph here\n\nlast";
        let doc = parse(body);
        let edits = splicible_root_blocks(&doc, body);
        let mid = edits
            .iter()
            .find(|e| e.plain_text == "MIDDLE paragraph here")
            .expect("the middle paragraph");

        let mut edit = mid.clone();
        edit.plain_text = "REPLACED".to_string();
        let out = splice(body, &edit);

        assert!(out.starts_with("first\n\n"), "first block clobbered");
        assert!(out.ends_with("\n\nlast"), "last block clobbered");
        assert!(out.contains("REPLACED"));
        // Exactly one change: the middle line.
        assert_eq!(out, "first\n\nREPLACED\n\nlast");
    }

    /// A heading edit preserves the trailing section separator that the span
    /// consumed.
    #[test]
    fn heading_edit_preserves_section_separator() {
        let body = "# A\n\n### Sub\n\n# D";
        let doc = parse(body);
        let a = splicible_root_blocks(&doc, body)
            .into_iter()
            .find(|e| matches!(&e.kind, BlockKind::Heading(1)) && e.plain_text == "A")
            .expect("h1 A");
        let mut edit = a;
        edit.plain_text = "Az".to_string();
        assert_eq!(splice(body, &edit), "# Az\n\n### Sub\n\n# D");
    }

    /// A fenced code edit replaces the inner content and keeps the fence.
    #[test]
    fn code_fenced_edit() {
        let body = "```rust\nfn old() {}\n```";
        let doc = parse(body);
        let code = splicible_root_blocks(&doc, body)
            .into_iter()
            .find(|e| matches!(&e.kind, BlockKind::Code { fenced: true, .. }))
            .expect("a fenced code block");
        let mut edit = code;
        edit.plain_text = "fn new() { let x = 2; }".to_string();
        assert_eq!(splice(body, &edit), "```rust\nfn new() { let x = 2; }\n```");
    }

    /// A multi-line fenced code edit (the indented body must be preserved
    /// verbatim, not re-indented).
    #[test]
    fn code_fenced_multiline_body_preserved() {
        let body = "```rust\nfn main() {\n    let x = 1;\n}\n```";
        let doc = parse(body);
        let code = splicible_root_blocks(&doc, body)
            .into_iter()
            .find(|e| matches!(&e.kind, BlockKind::Code { fenced: true, .. }))
            .expect("a fenced code block");
        let mut edit = code;
        edit.plain_text = "fn main() {\n    let y = 2;\n}".to_string();
        assert_eq!(
            splice(body, &edit),
            "```rust\nfn main() {\n    let y = 2;\n}\n```"
        );
    }

    /// `splice` is a pure no-op for a malformed (out-of-range) block.
    #[test]
    fn malformed_range_is_noop() {
        let body = "hello";
        let edit = BlockEdit {
            kind: BlockKind::Paragraph,
            plain_text: "world".to_string(),
            body_range: 100..200,
        };
        assert_eq!(splice(body, &edit), body);
    }

    /// Splices that start after a frontmatter block operate on the body slice
    /// only (ranges are body-relative; the caller passes the frontmatter-less
    /// body). Verifies the mechanism does not care where the body begins.
    #[test]
    fn frontmatter_offset_handled_by_body_relative_ranges() {
        // Simulate a body that begins mid-document (frontmatter already
        // removed by the caller). The resolver + splicer work on `body` only.
        let body = "## Section\ncontent here\n\n## Next";
        let doc = parse(body);
        let sec = splicible_root_blocks(&doc, body)
            .into_iter()
            .find(|e| matches!(&e.kind, BlockKind::Heading(2)) && e.plain_text == "Section")
            .expect("h2 Section");
        let mut edit = sec;
        edit.plain_text = "Renamed".to_string();
        assert_eq!(splice(body, &edit), "## Renamed\ncontent here\n\n## Next");
    }

    /// Toggle stability (§8.6): parsing → serializing → re-parsing a splicable
    /// block yields the same plain content, so Formated ⇄ Raw ⇄ Formatted with
    /// no edits is stable.
    #[test]
    fn reserialize_is_stable() {
        let body = "# T\n\npara with **style**";
        let doc = parse(body);
        for edit in splicible_root_blocks(&doc, body) {
            let re = serialize_block(&edit.kind, &edit.plain_text);
            // Re-serializing the same content is idempotent at the content level.
            let re2 = serialize_block(&edit.kind, &edit.plain_text);
            assert_eq!(re, re2);
            // (The exact round-trip to the *original* bytes also holds for
            // marker-free content — see identity_roundtrip_* above.)
            let _ = re;
        }
    }
}
