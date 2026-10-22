//! Frontmatter example smoke test (M1).
//!
//! Reads the per-type example pages under `kb-core/examples/frontmatter/` and
//! runs each through the **real** M1 parser (`kb_core::frontmatter::parse`).
//! These pin the *contract* the M5 indexer relies on, end-to-end:
//!
//!   - `wiki.example.md`  → indexable, kind `wiki`, tags deduped, body present.
//!   - `note.example.md`  → indexable, kind `note`, `search_required_phrase` verbatim (the
//!     gate words — `token bucket` / `mTLS` — kept exactly as written).
//!   - `label.example.md` → indexable, kind `label`.
//!   - `no-type.example.md` → NOT indexable (no `type:`) but still parses,
//!     and `unknown_type_present` is `false` (absent, not a typo).
//!
//! If a field the examples promise stops parsing, this fails the build — the
//! example and the parser cannot drift apart.

use std::path::{Path, PathBuf};

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("frontmatter")
        .canonicalize()
        .expect("kb-core/examples/frontmatter must exist (M1 deliverable)")
}

fn read(name: &str) -> String {
    let path = examples().join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()))
}

use kb_core::frontmatter::parse;
use kb_core::identity::{display_id, filename_minus_extension, find_h1, resolve_slug};
use kb_core::meta::NoteType;

#[test]
fn wiki_example_indexes_and_pops_fields() {
    let p = parse(&read("wiki.example.md"));
    assert!(p.is_indexable(), "wiki example must be indexable");
    assert_eq!(p.page_type, Some(NoteType::Wiki));
    assert_eq!(p.title.as_deref(), Some("Transformer (architecture)"));
    // tags: trimmed, deduped, first-occurrence order (all three distinct here).
    assert_eq!(p.tags, vec!["transformer", "llm", "attention"]);
    // description: present, short enough to be in-bound.
    assert!(p.description.is_some());
    assert!(!p.description_too_long);
    // body: text after the closing ---
    assert!(p.body.contains("Transformer"));
    // full pipeline: slug from title → display id.
    let slug = resolve_slug(p.title.as_deref(), find_h1(&p.body), Some("transformer"));
    assert_eq!(slug, Some("Transformer (architecture)"));
    assert_eq!(
        display_id(NoteType::Wiki, slug.unwrap()),
        "wiki:Transformer (architecture)"
    );
}

#[test]
fn note_example_keeps_search_required_phrase_verbatim() {
    let p = parse(&read("note.example.md"));
    assert!(p.is_indexable());
    assert_eq!(p.page_type, Some(NoteType::Note));
    // The gate words must be preserved verbatim (case as written): `mTLS`
    // keeps its mixed case, `token bucket` keeps its space. This is the
    // whole "search_required_phrase hidden gate" contract (§7).
    assert_eq!(p.search_required_phrase, vec!["token bucket", "mTLS"]);
    assert_eq!(p.tags, vec!["gateway", "http"]);
    assert_eq!(p.title.as_deref(), Some("API Gateway — notes"));
}

#[test]
fn label_example_indexes() {
    let p = parse(&read("label.example.md"));
    assert!(p.is_indexable());
    assert_eq!(p.page_type, Some(NoteType::Label));
    assert_eq!(p.title.as_deref(), Some("Transformer (papers)"));
    assert!(p.description.is_some());
    assert_eq!(p.tags, vec!["transformer"]);
}

#[test]
fn no_type_example_parses_but_is_not_indexable() {
    let p = parse(&read("no-type.example.md"));
    assert!(!p.is_indexable(), "a page with no `type:` is not indexed");
    assert_eq!(p.page_type, None);
    // Absent (not a typo) — so the indexer's *warn text* differs.
    assert!(!p.unknown_type_present);
    // Fields still came through — the page is readable, just not indexed.
    assert_eq!(p.title.as_deref(), Some("Scratch — no type"));
    assert_eq!(p.tags, vec!["scratch"]);
    assert!(!p.body.is_empty());
}

#[test]
fn all_examples_resolvable_to_a_display_id_when_indexable() {
    // Every INDEXABLE example resolves to a `type:slug` display id through
    // the real identity pipeline (title → H1 → filename). The no-type page
    // has no kind, so it is excluded.
    for (name, kind, expected) in [
        (
            "wiki.example.md",
            NoteType::Wiki,
            "wiki:Transformer (architecture)",
        ),
        (
            "note.example.md",
            NoteType::Note,
            "note:API Gateway — notes",
        ),
        (
            "label.example.md",
            NoteType::Label,
            "label:Transformer (papers)",
        ),
    ] {
        let p = parse(&read(name));
        let slug = match resolve_slug(p.title.as_deref(), find_h1(&p.body), Some("file")) {
            Some(s) => s,
            None => panic!("{name}: no slug resolved — title/H1/filename all empty?"),
        };
        let id = display_id(kind, slug);
        assert_eq!(id, expected, "{name}: display id mismatch");
        // sanity: the filename-minus-extension helper also works on a md path.
        assert_eq!(
            filename_minus_extension(name),
            Some(name.trim_end_matches(".md").to_string())
        );
    }
}
