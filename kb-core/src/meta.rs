//! NoteType / NoteMeta — the *exec* data model for a note's metadata.
//!
//! This file is **plain Rust**, deliberately containing **no `verus!` block
//! and no `Seq`/`@` spec syntax**. Why: this Verus fork rejects
//! user-defined types declared outside a `verus!` block. The *verified*
//! invariants about the display-id model live in `spec/meta_spec.rs`
//! (a standalone file verified with the `verus` binary — see
//! `./verify.sh`).
//!
//! The two layers are tied together the same way as the reference project
//! `git-restricted_version_B`:
//!
//!   ```text
//!     src/policy.rs  (exec Rust: three const lists + classify())
//!          │
//!   gen_spec.py  (drift-check: re-projects the const lists into the spec)
//!          │
//!   verus/policy_spec.rs  (spec: in_allow/in_deny/in_cond + lemmas I1..I5)
//!   ```
//!
//! For kb-core the "drift-check" is the *unit tests below*. They take the
//! concrete Appendix-A examples and assert the exec `as_display_id()`
//! produces strings matching the spec's `display_id_*` shape
//! (prefix ++ slug, prefix is a real prefix, slug is a suffix). The
//! *invariants* themselves (decomposition, prefix distinctness, slug
//! recoverability) are what Z3 actually proves.
//!
//! If someone refactors the exec `as_display_id()` and breaks the "prefix
//! ++ slug" shape, the tests below fail. If someone edits the spec and
//! drops a lemma or breaks an invariant, `verus` fails. The model and
//! the code cannot drift silently.
//!
//! ## Frontmatter fields (PLAN.md §3, §4)
//!
//! - `type:` — **required** to index a page. Missing → file skipped +
//!   warned. Matched case-insensitively; canonical lowercase stored.
//! - `title:` — optional. If present, used as the **slug** (display
//!   name). A page with a `title:` needs no H1.
//! - `description:` — optional, **≤ 255 chars** (PLAN.md §3), FTS-
//!   indexed, shown in `search` and `browse`.
//! - `tags:` — optional, trimmed, deduplicated, non-empty entries only.
//! - `search_terms:` — optional; whole-entry, case-insensitive substring
//!   match against the FTS portion of a query (PLAN.md §7).
//!
//! ## The display-id
//!
//! `as_display_id()` returns `<prefix> <slug>` where:
//!
//! | kind     | prefix   |
//! | -------- | -------- |
//! | Label    | `label:` |
//! | Wiki     | `wiki:`  |
//! | Note     | `note:`  |
//!
//! This value goes into `pages.id` and into `[[wikilink]]` references.
//! The prefix is not part of the *display* — it's the *kind* tag for
//! `kb resolve`.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoteType {
    /// `type: label` — a proxy note pointing at files/folders on disk.
    Label,
    /// `type: wiki` — an explanatory page about a concept.
    Wiki,
    /// `type: note` — a note scoped to its own folder.
    Note,
}

impl NoteType {
    /// The display-id prefix for this kind.
    #[inline]
    pub fn prefix(&self) -> &'static str {
        match self {
            NoteType::Label => "label:",
            NoteType::Wiki => "wiki:",
            NoteType::Note => "note:",
        }
    }

    /// Parse a `type:` field value (case-insensitive, trimmed).
    /// Returns `None` for values that aren't one of the three known kinds.
    ///
    /// The parser in `frontmatter.rs` (M1) is responsible for *warnings*
    /// when this returns `None`; this helper just does the match.
    #[inline]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "label" => Some(NoteType::Label),
            "wiki" => Some(NoteType::Wiki),
            "note" => Some(NoteType::Note),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NoteMeta {
    pub kind: NoteType,
    /// The **slug** — display-name part after the prefix.
    /// Resolved per PLAN.md §4: frontmatter `title` → H1 → filename.
    /// Whitespace-trimmed. Never empty (parser rejects empty/whitespace).
    pub slug: String,
    /// ≤ 255 chars if present. (Parser enforces the length bound.)
    pub description: Option<String>,
    /// Trimmed, deduped, non-empty. (Parser enforces the rules.)
    pub tags: Vec<String>,
    /// Non-empty entries as written (multi-word entries keep quotes).
    pub search_terms: Vec<String>,
}

impl NoteMeta {
    /// The **display id** — what goes into `pages.id` and into
    /// `[[wikilink]]` references.
    ///
    /// The shape `prefix ++ slug` is what the spec (in `spec/meta_spec.rs`)
    /// proves and what the unit tests below assert on concrete inputs.
    #[inline]
    pub fn as_display_id(&self) -> String {
        format!("{}{}", self.kind.prefix(), self.slug)
    }

    /// True if the slug is well-formed per the spec's definition
    /// (non-empty after trim). Used by callers as a *runtime* check in
    /// addition to what the spec *proves* about parser outputs.
    #[inline]
    pub fn slug_is_well_formed(&self) -> bool {
        !self.slug.trim().is_empty()
    }
}

// =============================================================================
// Unit tests — the "drift-check" that ties exec behaviour to the spec
// shape. These run under `cargo test` (not `verus`).
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // A helper to build meta without the struct-literal boilerplate.
    fn meta(kind: NoteType, slug: &str) -> NoteMeta {
        NoteMeta {
            kind,
            slug: slug.to_string(),
            description: None,
            tags: vec![],
            search_terms: vec![],
        }
    }

    // ---- display-id: concrete, *executable* cases from Appendix A -------

    #[test]
    fn label_display_id_a1() {
        let m = meta(NoteType::Label, "Transformers (architecture)");
        assert_eq!(m.as_display_id(), "label:Transformers (architecture)");
        assert!(m.as_display_id().starts_with("label:"));
        assert!(m.as_display_id().ends_with("Transformers (architecture)"));
    }

    #[test]
    fn wiki_display_id_a2() {
        let m = meta(NoteType::Wiki, "Transformer (architecture)");
        assert_eq!(m.as_display_id(), "wiki:Transformer (architecture)");
        assert!(m.as_display_id().starts_with("wiki:"));
        assert!(m.as_display_id().ends_with("Transformer (architecture)"));
    }

    #[test]
    fn note_display_id_a3() {
        let m = meta(NoteType::Note, "Transformer architecture (papers)");
        assert_eq!(m.as_display_id(), "note:Transformer architecture (papers)");
        assert!(m.as_display_id().starts_with("note:"));
        assert!(m
            .as_display_id()
            .ends_with("Transformer architecture (papers)"));
    }

    // ---- prefix correctness (every kind) ---------------------------------

    #[test]
    fn prefixes_are_correct() {
        assert_eq!(NoteType::Label.prefix(), "label:");
        assert_eq!(NoteType::Wiki.prefix(), "wiki:");
        assert_eq!(NoteType::Note.prefix(), "note:");
    }

    // The three prefixes are pairwise distinct (mirrors spec I2/I3).
    #[test]
    fn prefixes_are_pairwise_distinct() {
        let ps = [
            NoteType::Label.prefix(),
            NoteType::Wiki.prefix(),
            NoteType::Note.prefix(),
        ];
        assert_ne!(ps[0], ps[1]);
        assert_ne!(ps[0], ps[2]);
        assert_ne!(ps[1], ps[2]);
    }

    // ---- `type:` parse: the three known kinds, case-insensitive ----------

    #[test]
    fn parse_known_kinds() {
        assert_eq!(NoteType::parse("label"), Some(NoteType::Label));
        assert_eq!(NoteType::parse("Label"), Some(NoteType::Label));
        assert_eq!(NoteType::parse("LABEL"), Some(NoteType::Label));
        assert_eq!(NoteType::parse(" wiki"), Some(NoteType::Wiki));
        assert_eq!(NoteType::parse("Note"), Some(NoteType::Note));
        assert_eq!(NoteType::parse("note"), Some(NoteType::Note));
    }

    #[test]
    fn parse_rejects_unknown() {
        assert_eq!(NoteType::parse("file"), None);
        assert_eq!(NoteType::parse("blog"), None);
        assert_eq!(NoteType::parse(""), None);
        assert_eq!(NoteType::parse("   "), None);
    }

    // ---- slug well-formedness: the parser contract -----------------------

    #[test]
    fn whitespace_only_slug_is_rejected() {
        let m = meta(NoteType::Label, "   ");
        assert!(!m.slug_is_well_formed());
        // But a slug with inner spaces IS fine (`Transformers (architecture)`).
        let ok = meta(NoteType::Label, "Transformers (architecture)");
        assert!(ok.slug_is_well_formed());
    }

    // ---- resolve: split on the first ':' recovers the kind ---------------

    #[test]
    fn resolve_label_slug() {
        // If we had only `label:<slug>`, the slug is everything AFTER
        // the first ':'. (No ':' in the prefix itself, so the first ':'
        // is unambiguous. This is `kb resolve`'s core operation.)
        let id = meta(NoteType::Label, "a:b").as_display_id();
        assert_eq!(id, "label:a:b");
        let (kind, slug) = id.split_once(':').unwrap();
        assert_eq!(kind, "label");
        assert_eq!(slug, "a:b");
    }

    // ---- full Appendix A.1 example: all 5 frontmatter fields present -----

    #[test]
    fn full_a1_note() {
        // A.1: label about the Transformer paper (full frontmatter).
        let m = NoteMeta {
            kind: NoteType::Label,
            slug: "Transformers (architecture)".to_string(),
            description: Some(
                "A proxy note pointing at /src/papers/acl_2017/transformer.pdf".to_string(),
            ),
            tags: vec!["rust".to_string(), "architecture".to_string()],
            search_terms: vec!["rust async".to_string(), "grpc".to_string()],
        };
        assert_eq!(m.as_display_id(), "label:Transformers (architecture)");
        assert!(m.slug_is_well_formed());
        assert!(m.tags.len() == 2);
        assert!(m.search_terms.len() == 2);
        let d = m.description.as_ref().unwrap();
        assert!(d.len() <= 255); // PLAN.md §3
    }
}
