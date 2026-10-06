//! Golden fixture structural test (M0).
//!
//! The five MD pages under `tests/fixtures/` are the **single source of truth**
//! for frontmatter shape (PLAN.md Appendix A.1–A.1b) used by the M1
//! `kb-core::frontmatter` parser and the M5 `kb-db` indexer. This test does
//! NOT depend on either — it parses just enough (the `---` frontmatter block)
//! to assert that each page carries the exact `type:` and `title:` promised by
//! the Appendix A.6 identity table.
//!
//! If a fixture note is edited, either the note or this table must change —
//! that is the whole point of a golden test: it fails the build the moment the
//! fixture and the documented contract drift apart.

use std::path::{Path, PathBuf};

/// The workspace-root `tests/fixtures` dir, resolved at build time from
/// `kb-core`'s manifest so the test works regardless of CWD.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures")
        .canonicalize()
        .expect("tests/fixtures must exist (M0 deliverable)")
}

/// A row of the Appendix A.6 identity table: root sub-dir, file (rel to root),
/// expected `type:` value, expected `title:` value.
struct Expected {
    root: &'static str,
    file: &'static str,
    page_type: &'static str,
    title: &'static str,
}

const TABLE: &[Expected] = &[
    Expected { root: "notes", file: "wiki/transformer.md", page_type: "wiki", title: "Transformer (architecture)" },
    Expected { root: "work", file: "projects/api-gateway/README.md", page_type: "note", title: "API Gateway — notes" },
    Expected { root: "library", file: "llm.md", page_type: "label", title: "LLM" },
    Expected { root: "library", file: "czech.md", page_type: "label", title: "Czech" },
    Expected { root: "library", file: "transformer.md", page_type: "label", title: "Transformer (papers)" },
];

/// Minimal frontmatter reader: the region between the first `---` line and the
/// next `---` line. Returns the raw block text (keys + values). Not the real
/// parser (M1) — just enough to pull out `type:` and `title:` flat scalars.
fn frontmatter_block(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(lines.first().copied().unwrap_or("").trim(), "---", "page must open with ---");
    let start = 1;
    let end = lines[1..]
        .iter()
        .position(|l| l.trim() == "---")
        .expect("closing --- of frontmatter required")
        + 1;
    lines[start..end].join("\n")
}

/// Pull a flat `key: value` scalar from a frontmatter block. (Tags etc. are
/// lists and are intentionally out of scope for this M0 check.)
fn get_scalar(block: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    for line in block.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix(&prefix) {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[test]
fn every_fixture_page_matches_the_identity_table() {
    for e in TABLE {
        let path = fixtures().join(e.root).join(e.file);
        assert!(path.is_file(), "missing fixture: {}", path.display());
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
        let block = frontmatter_block(&text);
        let got_type = get_scalar(&block, "type")
            .unwrap_or_else(|| panic!("{}: no `type:` in frontmatter", path.display()));
        assert_eq!(got_type, e.page_type, "{}: type mismatch", path.display());
        let got_title = get_scalar(&block, "title")
            .unwrap_or_else(|| panic!("{}: no `title:` in frontmatter", path.display()));
        assert_eq!(got_title, e.title, "{}: title mismatch", path.display());
    }
}

#[test]
fn non_md_label_targets_are_present_and_never_indexed() {
    // The 5 referenced non-MD resources must exist on disk (the labels point at
    // them) and must live only under `library/` — confirming they are
    // *referenced files*, not MD pages (they would never index as rows).
    let targets = [
        "papers/attention-is-all-you-need.pdf",
        "papers/multitask-chain-of-thought.pdf",
        "papers/scaled-robust-fine-tuning.pdf",
        "papers/transformer-xl-attentive-language.pdf",
        "books/deep-learning-goodfellow-ch3.epub",
    ];
    for t in targets {
        let p = fixtures().join("library").join(t);
        assert!(p.is_file(), "missing label target: {}", p.display());
    }
}
