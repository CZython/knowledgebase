//! labels — file-reference classification for `type: label` pages (M2).
//!
//! §6 (Tags and Labels, "How a label names its referenced file(s)/folder")
//! + §9 (Verification):
//!
//! > `labels` — label file-reference classification **deterministic**:
//! > `## files` / leading contiguous path-list run / folder-wide, **exactly
//! > one wins**; referenced paths well-formed.
//!
//! A `type: label` page is a *small proxy note* for something bigger. When a
//! tag search or `kb tag` matches a label, the tool does **not** show the
//! label page — it reads the label note from disk and shows the **referenced
//! files/folder**. This module is the pure classifier that decides *what* the
//! label refers to. (Resolving the relative paths against the note's
//! directory, and enumerating the folder, is IO and lives in the M6/M7 CLI /
//! M5 indexer — out of scope for the pure core.)
//!
//! ## The three cases (exactly one wins, in priority order)
//!
//! 1. **`## files`** ATX heading present (**case-insensitive**; `## Files`
//!    works) → the block under it is the file list.
//! 2. A **leading contiguous run** of path-like lines immediately following
//!    the title (the H1; or, when the page has no H1, immediately after the
//!    frontmatter) with **no prose before them** → that run is the file list.
//! 3. Otherwise (prose first, or no list) → **folder-wide** — the label's
//!    own directory is the reference (non-recursive upstream; the classifier
//!    just reports "folder").
//!
//! Referenced paths are kept **well-formed**: trimmed, non-empty, and only
//! path-like tokens (prose lines inside a `## files` block are skipped).
//!
//! Verified: `spec/labels_spec.rs` proves the classification is a clean
//! 3-way exclusive decision (exactly one of the three can win) and that the
//! "no prose before the run" guard partitions case-2 vs case-3.

use super::links::is_pathlike;

/// What a label page refers to (the deterministic classification, §6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LabelRef {
    /// Case 1 or 2 — an explicit list of (relative) file references, trimmed,
    /// path-like only, in document order.
    Files(Vec<String>),
    /// Case 3 — folder-wide: the label references its own directory (the
    /// non-recursive enumeration happens upstream when the label is expanded).
    Folder,
}

impl LabelRef {
    /// `true` when this is an explicit file list (cases 1/2) and that list is
    /// non-empty. A folder-wide or empty-list label reports `false`.
    #[inline]
    pub fn is_explicit_files(&self) -> bool {
        matches!(self, LabelRef::Files(f) if !f.is_empty())
    }
}

/// A Markdown ATX heading of *any* level: a run of one or more `#`, then
/// whitespace (or end), i.e. `# Title`, `## Files`, `### Sub`. A `#` glued to
/// text (`#Title`) is *not* a heading (CommonMark requires the space).
fn is_heading(line: &str) -> bool {
    let t = line.trim_start();
    let hash_count = t.bytes().take_while(|b| *b == b'#').count();
    if hash_count == 0 {
        return false;
    }
    // the char right after the `#` run must be whitespace or end-of-line
    let after = t.get(hash_count..);
    after.is_none() || after.unwrap_or("").starts_with(' ')
}

/// A **level-2** heading (`## …`) whose text, lower-cased and trimmed, is
/// `files` — the case-1 marker (case-insensitive: `## Files`, `## FILES`,
/// `## Files ` all match).
fn is_files_heading(line: &str) -> bool {
    let t = line.trim_start();
    let t = match t.strip_prefix("##") {
        Some(r) => r,
        None => return false,
    };
    // require the level to be exactly 2 (a `###` would strip to "#…")
    if t.starts_with('#') {
        return false;
    }
    t.trim().to_lowercase() == "files"
}

/// The first heading in the body (any level), or `None`. Used to find the
/// "title" boundary for the leading-run (case 2).
fn first_heading_idx(lines: &[&str]) -> Option<usize> {
    lines.iter().position(|l| is_heading(l))
}

/// Classify a label page's body (the text *after* the frontmatter delimiters)
/// into one of the three cases. Deterministic: exactly one of the three
/// branches is taken, in priority order (files-heading > leading-run > folder).
///
/// # Rules (mirrored by `spec/labels_spec.rs`)
/// - **Case 1:** if any `## files` heading exists, the block under it (up to
///   the next heading) is the file list — path-like lines only, trimmed. This
///   *wins* over case 2 even if the page also has a leading run.
/// - **Case 2:** else, look at the lines after the leading title (first
///   heading, or the top when there is none). If the **first non-blank** line
///   in that region is path-like, collect the contiguous run of path-like
///   lines (blank lines don't break the run; a prose line ends it) → the file
///   list.
/// - **Case 3:** otherwise → folder-wide.
///
/// A blank/empty body → folder-wide (no list, no prose).
pub fn classify(body: &str) -> LabelRef {
    let lines: Vec<&str> = body.lines().collect();

    // ---- Case 1: an explicit `## files` heading, if any --------------------
    if let Some(idx) = lines.iter().position(|l| is_files_heading(l)) {
        let mut files: Vec<String> = Vec::new();
        for l in &lines[idx + 1..] {
            let t = l.trim();
            if t.is_empty() {
                continue; // blanks don't matter inside the block
            }
            if is_heading(t) {
                break; // the next heading ends the `## files` block
            }
            if is_pathlike(t) {
                files.push(t.to_string());
            }
            // prose lines inside the block are skipped (not a file ref)
        }
        return LabelRef::Files(files);
    }

    // ---- Case 2: leading contiguous path run (no prose before them) --------
    // Skip optional leading blanks and the title (first heading), then
    // examine the region that follows.
    let start = match first_heading_idx(&lines) {
        Some(h) => h + 1,
        None => 0,
    };

    // Find the first non-blank line after the title region. If it's prose
    // (not path-like) → case 3. If it's path-like → collect the run.
    let mut i = start;
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    if i < lines.len() {
        let first = lines[i].trim();
        if is_pathlike(first) {
            let mut files: Vec<String> = Vec::new();
            while i < lines.len() {
                let t = lines[i].trim();
                if t.is_empty() {
                    i += 1;
                    continue; // blank lines inside the run are allowed
                }
                if is_heading(t) {
                    break; // a section heading ends the run
                }
                if is_pathlike(t) {
                    files.push(t.to_string());
                    i += 1;
                } else {
                    break; // prose ends the run
                }
            }
            return LabelRef::Files(files);
        }
        // first non-blank line was prose → case 3
    }

    // ---- Case 3: folder-wide ----------------------------------------------
    LabelRef::Folder
}

/// Well-formedness of a single file reference: non-empty and path-like (the
/// §9 "referenced paths well-formed" half). A pure predicate for the spec.
#[inline]
pub fn ref_is_well_formed(r: &str) -> bool {
    !r.trim().is_empty() && is_pathlike(r.trim())
}

/// True if `ref_text` is a *relative* reference (starts with neither `/` nor
/// `~`). Referenced files resolve against the label's directory, so a
/// well-formed label ref in v1 is always relative.
#[inline]
pub fn ref_is_relative(r: &str) -> bool {
    let t = r.trim();
    !t.is_empty() && !t.starts_with('/') && !t.starts_with('~')
}

// =============================================================================
// Unit tests — pin exec `labels` to the spec shape (§6 cases + §9).
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- case 1: `## files` heading (case-insensitive) --------------------

    #[test]
    fn case1_files_heading_collects_block() {
        let body = concat!(
            "# LLM\n",
            "\n",
            "Large Language Model papers we read.\n",
            "\n",
            "## Files\n",
            "papers/attention-is-all-you-need.pdf\n",
            "books/deep-learning-goodfellow-ch3.epub\n",
            "\n",
            "## Summary\n",
            "Mixed reading.\n",
        );
        match classify(body) {
            LabelRef::Files(f) => assert_eq!(
                f,
                vec!["papers/attention-is-all-you-need.pdf", "books/deep-learning-goodfellow-ch3.epub"]
            ),
            other => panic!("expected Files, got {other:?}"),
        }
    }

    #[test]
    fn case1_is_case_insensitive_on_heading() {
        let body = "# X\n\n## FILES\na.pdf\nb.epub\n";
        assert_eq!(
            classify(body),
            LabelRef::Files(vec!["a.pdf".to_string(), "b.epub".to_string()])
        );
        let body2 = "## files\na.pdf\n";
        assert_eq!(
            classify(body),
            LabelRef::Files(vec!["a.pdf".to_string(), "b.epub".to_string()])
        );
        assert_eq!(classify(body2), LabelRef::Files(vec!["a.pdf".to_string()]));
    }

    #[test]
    fn case1_stops_at_next_heading_and_skips_prose() {
        let body = "## Files\ncollect-me.pdf\nsome prose line here\nme-too.epub\n## Next\nafter.pdf\n";
        // prose skipped, block ends at `## Next`; `after.pdf` (after the next
        // heading) is NOT in this label's list.
        assert_eq!(
            classify(body),
            LabelRef::Files(vec!["collect-me.pdf".to_string(), "me-too.epub".to_string()])
        );
    }

    // ---- case 2: leading contiguous path run ------------------------------

    #[test]
    fn case2_leading_run_after_title() {
        let body = "# Transformer (papers)\n\npapers/a.pdf\npapers/b.pdf\n";
        assert_eq!(
            classify(body),
            LabelRef::Files(vec!["papers/a.pdf".to_string(), "papers/b.pdf".to_string()])
        );
    }

    #[test]
    fn case2_no_frontmatter_title_run_from_top() {
        // A label with no H1 and no frontmatter title: the leading run starts
        // at the top (the classifier only sees the body text).
        let body = "papers/a.pdf\nbooks/b.epub\n";
        assert_eq!(
            classify(body),
            LabelRef::Files(vec!["papers/a.pdf".to_string(), "books/b.epub".to_string()])
        );
    }

    #[test]
    fn case2_prose_breaks_the_run() {
        // paths, prose, more paths → the run is only the first contiguous run
        // (paths BEFORE the prose); the trailing ones are past the prose.
        let body = "# X\n\npapers/a.pdf\ninterruption prose\npapers/b.pdf\n";
        assert_eq!(classify(body), LabelRef::Files(vec!["papers/a.pdf".to_string()]));
    }

    // ---- case 3: folder-wide ----------------------------------------------

    #[test]
    fn case3_prose_first_is_folder() {
        let body = "# Czech\n\nEverything about the Czech language we track.\n";
        assert_eq!(classify(body), LabelRef::Folder);
    }

    #[test]
    fn case3_empty_body_is_folder() {
        assert_eq!(classify(""), LabelRef::Folder);
        assert_eq!(classify("\n\n\n"), LabelRef::Folder);
    }

    // ---- priority: case 1 wins over case 2 --------------------------------

    #[test]
    fn case1_beats_existing_leading_run() {
        // The page has BOTH a leading-ish file and a `## files` block. The
        // heading present ⇒ case 1 wins and its (explicit) list is used.
        let body = "# X\npapers/early.pdf\n\n## Files\npapers/really-listed.pdf\n";
        // `## files` present → case 1: collect its block → only the listed one.
        assert_eq!(classify(body), LabelRef::Files(vec!["papers/really-listed.pdf".to_string()]));
    }

    // ---- well-formedness (referenced paths) -------------------------------

    #[test]
    fn only_pathlike_tokens_are_kept() {
        // A bare word mixed into a `## files` block is prose → skipped.
        let body = "## Files\npapers/a.pdf\nattention\nbooks/b.epub\n";
        assert_eq!(
            classify(body),
            LabelRef::Files(vec!["papers/a.pdf".to_string(), "books/b.epub".to_string()])
        );
    }

    #[test]
    fn ref_well_formed_requires_pathlike() {
        assert!(ref_is_well_formed("papers/a.pdf"));
        assert!(ref_is_well_formed("books/b.epub"));
        assert!(!ref_is_well_formed("attention")); // bare word is not path-like
        assert!(!ref_is_well_formed("   "));
        assert!(!ref_is_well_formed(""));
    }

    #[test]
    fn referenced_refs_are_relative() {
        assert!(ref_is_relative("papers/a.pdf"));
        assert!(ref_is_relative("../rust/gateway/foo.txt"));
        assert!(!ref_is_relative("/abs/foo.pdf"));
        assert!(!ref_is_relative("~/foo.pdf"));
        assert!(!ref_is_relative(""));
    }
}
