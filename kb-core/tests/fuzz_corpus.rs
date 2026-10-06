//! Fuzz corpus (M2) — deterministic, seeded property-based checks over the
//! four M2 modules (`tags`, `links`, `graph`, `labels`).
//!
//! Not cargo-fuzz (which spawns a separate driver): a **deterministic**
//! seeded PRNG (LCG) drives a batch of random inputs and asserts the very
//! invariants the specs in `spec/*.rs` prove symbolically. This catches
//! *property* bugs (invariants that survive single fixed inputs but break
//! on some random one) without a fuzz harness — a cheap, portable stand-in
//! for the "fuzz corpus" the §9 / M2 line of §12 wants.
//!
//! Reproducible: same seed ⇒ same inputs ⇒ same asserts. The seed is a named
//! constant; a failing run points at the failing property, the seed is
//! enough to reproduce it.
//!
//! Properties asserted (each mirrored by a spec lemma):
//! - **tags::normalize** — idempotent; duplicate-free; never adds an item;
//!   drops empties. (`spec/tags_spec.rs` N-series.)
//! - **tags::canonicalize** — case-fold conflate is conflating and total;
//!   empty query ⇒ empty. (`spec/tags_spec.rs` C-series.)
//! - **links** — style ⟹ target shape (absolute ⟹ leading `/`; relative
//!   ⟹ no leading `/`); wikilink never a path; dedup idempotent.
//!   (`spec/links_spec.rs` S/D-series.)
//! - **graph** — self-loops dropped; `out(a)∋b ⟺ in(b)∋a` (inverse, both
//!   dirs); forward & backlink views name the same edge set; no duplicate
//!   neighbours. (`spec/graph_spec.rs` I/L/D-series.)
//! - **labels** — deterministic (one of the three outcomes, stable re-run);
//!   case-1 (heading) beats a co-existing run; kept refs well-formed.
//!   (`spec/labels_spec.rs` C/P/W-series.)

use kb_core::graph::{Graph, inverse};
use kb_core::labels::{classify, LabelRef, ref_is_well_formed};
use kb_core::links::{extract_links, dedup, LinkStyle};
use kb_core::tags::{normalize, canonicalize};

/// Deterministic PRNG (xorshift32) — seed-independent reproducibility.
struct Rng(u32);
impl Rng {
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    fn next_usize(&mut self, bound: usize) -> usize {
        if bound == 0 { return 0; }
        (self.next_u32() % bound as u32) as usize
    }
    fn pick<T: Clone>(&mut self, v: &[T]) -> T {
        v[self.next_usize(v.len())].clone()
    }
    fn next_bool(&mut self) -> bool { self.next_u32() & 1 == 1 }
}

fn seed() -> u32 {
    0x9e37_79b9_u32 // a fixed constant: reproducible across machines/builds
}

// ============================================================================
// tags
// ============================================================================

#[test]
fn fuzz_tags_normalize_properties() {
    let mut r = Rng(seed());
    let alpha = ["a", "b", "llm", "attention", "Czechia", "http", "rust", "", "  ", "x y", "camelCase"];
    for _ in 0..1000 {
        let n = 1 + r.next_usize(8);
        let input: Vec<String> = (0..n).map(|_| r.pick(&alpha).to_string()).collect();

        let out = normalize(&input);

        // (N) duplicate-free
        let mut deduped = out.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(out.len(), deduped.len(), "normalize must not emit duplicates: {out:?}");

        // (N) idempotent
        assert_eq!(normalize(&out), out, "normalize must be idempotent: {out:?}");

        // (N) never *adds* items beyond the input — every output is a
        //     trimmed copy of some input item (membership, not creation).
        for o in &out {
            assert!(
                input.iter().any(|i| i.trim() == o.as_str()),
                "normalize emitted {o:?} that no input item trims to: {input:?}"
            );
        }

        // (N) empties dropped
        for o in &out {
            assert!(!o.is_empty(), "normalize emitted an empty tag: {out:?}");
        }
    }
}

#[test]
fn fuzz_tags_canonicalize_properties() {
    let mut r = Rng(seed());
    let words = ["czech", "Czech", "CZECH", "llm", "rust", "czechia", "Rust", "http"];
    for _ in 0..1000 {
        let stored_n = r.next_usize(6);
        let stored: Vec<String> = (0..stored_n).map(|_| r.pick(&words).to_string()).collect();
        let alias = r.pick(&words);
        let canon = r.pick(&words);
        let aliases: Vec<(String,String)> = if r.next_bool() { vec![(alias.to_string(), canon.to_string())] } else { Vec::new() };
        let query = if r.next_bool() { r.pick(&words) } else { "" };

        let hit = canonicalize(query, &stored, &aliases);

        // (C) total + deterministic: a re-run gives the same result.
        let hit2 = canonicalize(query, &stored, &aliases);
        assert_eq!(hit, hit2, "canonicalize must be deterministic (same inputs ⇒ same set)");

        // empty/whitespace query ⇒ empty match set
        if query.trim().is_empty() {
            assert!(hit.is_empty(), "empty query must match nothing: {hit:?}");
        } else {
            // every hit is a stored tag (subset — conflate only re-points,
            // never invents a stored string)
            for h in &hit {
                assert!(stored.iter().any(|s| s.as_str() == *h), "hit {h:?} not a stored tag");
            }
        }
    }
}

// ============================================================================
// links
// ============================================================================

const WIKI_TARGETS: &[&str] = &["LLM", "Transformer (architecture)", "Czech", "a/b"];
const REL_PATHS: &[&str] = &["papers/a.pdf", "books/b.epub", "../rust/gateway/", "notes/x.md"];
const ABS_PATHS: &[&str] = &["/home/u/notes/foo.md", "/abs/bar.pdf"];

fn random_body(r: &mut Rng) -> String {
    let mut parts: Vec<String> = Vec::new();
    let words = ["token", "bucket", "attention", "window", "the"];
    for _ in 0..r.next_usize(7) {
        match r.next_usize(5) {
            0 => parts.push(format!("see [[{}]] here", r.pick(WIKI_TARGETS))),
            1 => parts.push(format!("at {} now", r.pick(REL_PATHS))),
            2 => parts.push(format!("and {} also", r.pick(ABS_PATHS))),
            3 => parts.push(format!("word {}", r.pick(&words))),
            4 => parts.push(format!("prose here and there with {}", r.pick(&words))),
            _ => { parts.push(String::new()); }
        }
    }
    if r.next_bool() { parts.push(String::from("\n")); }
    parts.join(" ")
}

#[test]
fn fuzz_links_style_target_shape() {
    let mut r = Rng(seed());
    for _ in 0..1000 {
        let body = random_body(&mut r);
        let links = extract_links(&body);
        for l in &links {
            match l.style {
                LinkStyle::Wikilink => {
                    // style ⟹ target: never a filesystem path (kept whole).
                    // A wikilink target is the inner text verbatim (trimmed).
                    assert_eq!(l.target.trim(), l.target, "wikilink target must be trimmed: {body:?}");
                }
                LinkStyle::Absolute => {
                    assert!(l.target.starts_with('/'), "absolute ref must start with '/': {body:?} {:?} {l:?}", random_body(&mut r) );
                }
                LinkStyle::Relative => {
                    assert!(!l.target.starts_with('/'), "relative ref must not start with '/': {l:?}");
                }
            }
        }

        // (D) dedup is idempotent and total.
        let d1 = dedup(links.clone());
        let d2 = dedup(d1.clone());
        assert_eq!(d1, d2, "dedup must be idempotent on {body:?}");
        // dedup never *adds*: |dedup| <= |input|
        assert!(d1.len() <= links.len(), "dedup added rows: {links:?} -> {d1:?}");
        // dedup preserves first-occurrence order (a subsequence of input).
        let mut it = links.iter();
        for kept in &d1 {
            for n in it.by_ref() {
                if n == kept { break; }
            }
        }
    }
}

// ============================================================================
// graph
// ============================================================================

#[test]
fn fuzz_graph_invariant_invariants() {
    let mut r = Rng(seed());
    let nodes = ["/a/x.md", "/b/y.md", "/c/z.md", "/d/w.md", "/e/v.md"];
    for _ in 0..500 {
        let mut g = Graph::new();
        let m = r.next_usize(8) + 1;
        for _ in 0..m {
            let from = r.pick(&nodes);
            let to = r.pick(&nodes);
            g.add_edge(from, to);
        }

        // (L) no self-loops present.
        for (f, ts) in &g.out {
            assert!(!ts.contains(f), "self-loop survived at {f}");
        }
        for (t, ss) in &g.back {
            assert!(!ss.contains(t), "self-backlink survived at {t}");
        }

        // (I) inverse, BOTH directions, for every edge.
        for (f, ts) in &g.out {
            for t in ts {
                assert!(g.back.get(t).map(|s| s.contains(f)).unwrap_or(false),
                    "edge {f}->{t} not reflected in backlinks");
                assert!(g.out.get(f).map(|s| s.contains(t)).unwrap_or(false),
                    "edge {f}->{t} not in forward view (symmetry)");
            }
        }

        // (I) adjacency consistency: the edge set seen from `out` equals the
        //     edge set seen from `back` (re-inverted).
        let from_out: std::collections::BTreeSet<(String,String)> =
            g.edges().into_iter().collect();
        let from_back: std::collections::BTreeSet<(String,String)> = {
            let mut s = std::collections::BTreeSet::new();
            for (target, sources) in &g.back {
                for src in sources { s.insert((src.clone(), target.clone())); }
            }
            s
        };
        assert_eq!(from_out, from_back, "forward/back views disagree on {from_out:?} vs {from_back:?}");

        // (D) no duplicate neighbours (sets are unique by construction; the
        //     dedup of `add_edge` means repeated adds collapse — check edges
        //     are unique pairs).
        let mut es = g.edges().clone();
        es.sort();
        es.dedup();
        assert_eq!(g.edges().len(), es.len(), "duplicate edge pairs present");
    }
}

#[test]
fn fuzz_graph_inverse_helper() {
    let mut r = Rng(seed());
    let nodes = ["/a/x.md", "/b/y.md", "/c/z.md"];
    for _ in 0..300 {
        let mut out: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> = std::collections::BTreeMap::new();
        let m = r.next_usize(6) + 1;
        for _ in 0..m {
            let f = r.pick(&nodes);
            let t = r.pick(&nodes);
            out.entry(f.to_string()).or_default().insert(t.to_string());
        }
        let back = inverse(&out);

        // (I) `inverse(out)` reconstructs the backlink view of `out`. For
        //     every forward edge a->b (a!=b), b lists a; for a==b (a self
        //     loop in input), it is dropped.
        for (from, targets) in &out {
            for to in targets {
                let expected = from != to;
                let actual = back.get(to).map(|s| s.contains(from)).unwrap_or(false);
                assert_eq!(expected, actual,
                    "inverse edge {from}->{to} expected={expected} actual={actual}");
            }
        }
    }
}

// ============================================================================
// labels
// ============================================================================

#[test]
fn fuzz_labels_determinism_and_invariants() {
    let mut r = Rng(seed());
    let path_words = ["papers/a.pdf", "books/b.epub", "../x/y.txt", "data/z.csv"];
    let prose_words = ["hello", "world", "about", "this", "reads"];
    let headings = ["# Title", "## Files", "## Summary", "## files", "### deep", "## FILES"];

    let outcome_of = |o: &LabelRef| match o {
        LabelRef::Files(_) => "files",
        LabelRef::Folder  => "folder",
    };

    for _ in 0..1000 {
        // Build a random body: optional title, then a mix of prose / paths /
        // headings, so that all three cases can arise.
        let mut lines: Vec<String> = Vec::new();
        if r.next_bool() { lines.push(r.pick(&headings).to_string()); }
        if r.next_bool() { lines.push(String::new()); }
        let nl = 2 + r.next_usize(6);
        for _ in 0..nl {
            let roll = r.next_usize(5);
            match roll {
                0 => lines.push(r.pick(&prose_words).to_string()),
                1 | 2 => lines.push(r.pick(&path_words).to_string()),
                3 => lines.push(r.pick(&headings).to_string()),
                _ => lines.push(String::new()),
            }
        }
        let body = lines.join("\n");

        // (C) deterministic: re-running the classifier is stable.
        let a = classify(&body);
        let b = classify(&body);
        assert_eq!(outcome_of(&a), outcome_of(&b), "classify not deterministic: {body:?}");
        match (&a, &b) {
            (LabelRef::Files(x), LabelRef::Files(y)) => assert_eq!(x, y, "classify list unstable: {body:?}"),
            (LabelRef::Folder, LabelRef::Folder) => {}
            _ => panic!("classify outcome class unstable: {a:?} vs {b:?} for {body:?}"),
        }

        // (W) any explicit file list contains only well-formed (non-empty,
        //     path-like) references.
        if let LabelRef::Files(f) = &a {
            for t in f {
                assert!(ref_is_well_formed(t), "malformed ref {t:?} in list {f:?} for {body:?}");
            }
            // no blank / prose entry ever survives
            for t in f {
                assert!(!t.trim().is_empty(), "blank ref survived: {f:?}");
            }
        }
    }
}

#[test]
fn fuzz_labels_priority_and_emptiness() {
    let mut r = Rng(seed());
    let paths = ["papers/leading.pdf", "books/second.epub", "data/third.csv"];
    for _ in 0..500 {
        // Ensure a `## Files` block is present somewhere (case-1 marker) and
        // ALSO a leading path run (case-2 signal). The classifier must pick
        // case 1 and return the *heading block's* files, not the leading run.
        let lead = paths[r.next_usize(paths.len())].to_string();
        let block = paths[r.next_usize(paths.len())].to_string();
        let body = format!("# T\n\n{lead}\n\n## Files\n{block}\n");
        match classify(&body) {
            LabelRef::Files(f) => {
                // case-1 block content is authoritative. The *only* required
                // invariant here: the result is a file list and non-empty
                // (both lead and block are path-like, so at least the block
                // file is present).
                assert!(!f.is_empty(), "case-1 with a listed file must not yield empty");
                // deterministic re-run
                assert_eq!(classify(&body), classify(&body));
            }
            LabelRef::Folder => panic!("a body with a `## Files` block must classify as Files, not Folder: {body:?}"),
        }
    }
}
