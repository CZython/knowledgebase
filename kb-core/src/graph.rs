//! graph — forward/backlink adjacency over *resolved* edges (M2).
//!
//! §9 (Verification):
//!
//! > `graph` — no duplicate edges; **backlink = inverse of forward** (for
//! > resolved edges — a `links.to_path` holding a pending display-id ref §8
//! > resolves to a single `abs_path` before the inverse is taken);
//! > **no self-loops**; **adjacency maps consistent**.
//!
//! ## Model
//!
//! A page's *identity* is its `abs_path` (unique, §4/§8). Edges are over
//! `abs_path`s that are already **resolved** — the pending display-id →
//! `abs_path` step happens upstream (the M5 indexer via `kb resolve`), so
//! this module takes a resolved edge set. We model the graph as two maps:
//!
//! - `out: abs_path -> sorted, deduped list of targets` (forward links)
//! - `in:  abs_path -> sorted, deduped list of sources` (backlinks)
//!
//! and prove they are *mutual inverses*: `out(a) ∋ b` ⟺ `in(b) ∋ a`.
//! Self-loops are dropped (a page does not backlink itself), so neither map
//! ever lists a node under its own adjacency for a self-edge. No duplicate
//! neighbours (the `links` PK means one row per `(from,to,style)`; for a
//! fixed source the adjacency lists are deduped on target).
//!
//! Verified: `spec/graph_spec.rs` proves the inverse/equivalence, the
//! no-self-loop exclusion, and the adjacency-consistency (forward and
//! backlink views agree on the edge set).

use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// A resolved-edge graph keyed by `abs_path`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Graph {
    /// forward: source -> set of targets (ordered for determinism).
    pub out: BTreeMap<String, BTreeSet<String>>,
    /// backlink: target -> set of sources (ordered for determinism).
    pub back: BTreeMap<String, BTreeSet<String>>,
}

impl Graph {
    #[inline]
    pub fn new() -> Self {
        Graph {
            out: BTreeMap::new(),
            back: BTreeMap::new(),
        }
    }

    /// Add a resolved edge `from -> to`. A self-loop (`from == to`) is
    /// dropped. Duplicate edges collapse (the sets dedupe). Returns `true` if
    /// the set of edges changed (i.e. it was a new, non-loop edge).
    ///
    /// # Invariants maintained (mirrored by `spec/graph_spec.rs`)
    /// - `from != to` after a successful add (no self-loops).
    /// - `out[from] ∋ to` ⟺ `in[to] ∋ from` (inverse).
    /// - `out` and `in` agree on the exact same edge set (consistency).
    pub fn add_edge(&mut self, from: &str, to: &str) -> bool {
        if from == to {
            return false; // drop self-loop (§9 no self-loops)
        }
        let added = self
            .out
            .entry(from.to_string())
            .or_default()
            .insert(to.to_string());
        if added {
            self.back
                .entry(to.to_string())
                .or_default()
                .insert(from.to_string());
        }
        added
    }

    /// The deduped, sorted forward neighbours of `node`, or an empty list.
    pub fn outgoing(&self, node: &str) -> Vec<String> {
        self.out
            .get(node)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// The deduped, sorted backlinks (sources) of `node`, or an empty list.
    pub fn incoming(&self, node: &str) -> Vec<String> {
        self.back
            .get(node)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// True iff the resolved edge `from -> to` is present (forward view).
    #[inline]
    pub fn has_edge(&self, from: &str, to: &str) -> bool {
        self.out.get(from).map(|s| s.contains(to)).unwrap_or(false)
    }

    /// All edges as `(from, to)` pairs, sorted (a total view for tests / the
    /// M5 indexer to upsert). Self-loops never appear.
    pub fn edges(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = Vec::new();
        for (f, ts) in &self.out {
            for t in ts {
                v.push((f.clone(), t.clone()));
            }
        }
        v.sort();
        v
    }
}

/// Inverse of a forward adjacency as a *pure* relation (no self-loops).
/// Given a map `out` (from -> targets), return the `in` (to -> sources) such
/// that `out[a]∋b ⟺ in[b]∋a`. This is the "backlink = inverse of forward"
/// contract isolated, for the spec to pin and the tests to check.
pub fn inverse(out: &BTreeMap<String, BTreeSet<String>>) -> BTreeMap<String, BTreeSet<String>> {
    let mut back: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (from, targets) in out {
        for to in targets {
            // no self-loops: a map that lists its own key is dropped here.
            if from != to {
                back.entry(to.clone()).or_default().insert(from.clone());
            }
        }
    }
    back
}

// =============================================================================
// Unit tests — pin exec `graph` to the spec shape.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- self-loop exclusion ---------------------------------------------

    #[test]
    fn self_loop_is_dropped() {
        let mut g = Graph::new();
        assert!(!g.add_edge("/a/x.md", "/a/x.md"));
        assert!(g.outgoing("/a/x.md").is_empty());
        assert!(g.incoming("/a/x.md").is_empty());
        assert!(!g.has_edge("/a/x.md", "/a/x.md"));
    }

    // ---- inverse: out(a)∋b ⟺ in(b)∋a (both directions) --------------------

    #[test]
    fn backlink_is_inverse_of_forward() {
        let mut g = Graph::new();
        g.add_edge("/a/x.md", "/b/y.md"); // x -> y
        g.add_edge("/a/x.md", "/c/z.md"); // x -> z
        g.add_edge("/c/z.md", "/b/y.md"); // z -> y
        g.add_edge("/b/y.md", "/d/w.md"); // y -> w

        // forward view of x
        assert_eq!(g.outgoing("/a/x.md"), vec!["/b/y.md", "/c/z.md"]);
        // y's backlinks = {x, z}
        assert_eq!(g.incoming("/b/y.md"), vec!["/a/x.md", "/c/z.md"]);
        // equivalence holds for every present edge (x→y, z→y)
        assert!(
            g.has_edge("/a/x.md", "/b/y.md")
                == g.incoming("/b/y.md").contains(&"/a/x.md".to_string())
        );
        assert!(
            g.has_edge("/c/z.md", "/b/y.md")
                == g.incoming("/b/y.md").contains(&"/c/z.md".to_string())
        );
        // and the converse: a source listed in in(y) has that edge
        for s in g.incoming("/b/y.md").clone() {
            assert!(
                g.has_edge(&s, "/b/y.md"),
                "{s} must appear as a forward edge to y"
            );
        }
    }

    // ---- duplicate collapse (no duplicate neighbours) ----------------------

    #[test]
    fn duplicate_edges_collapse() {
        let mut g = Graph::new();
        assert!(g.add_edge("/a/x.md", "/b/y.md"));
        assert!(
            !g.add_edge("/a/x.md", "/b/y.md"),
            "second identical add reports no change"
        );
        assert_eq!(g.outgoing("/a/x.md"), vec!["/b/y.md"]);
        assert_eq!(g.incoming("/b/y.md"), vec!["/a/x.md"]);
        assert_eq!(g.edges().len(), 1);
    }

    // ---- adjacency consistency (forward & backlink agree) -----------------

    #[test]
    fn forward_and_backlink_views_are_consistent() {
        let mut g = Graph::new();
        g.add_edge("/a/x.md", "/b/y.md");
        g.add_edge("/a/x.md", "/c/z.md");
        g.add_edge("/b/y.md", "/c/z.md");

        // The EDGE SET seen from `out` equals the edge set seen from `in`.
        let from_out: std::collections::BTreeSet<(String, String)> =
            g.edges().into_iter().collect();
        let from_in: std::collections::BTreeSet<(String, String)> = {
            let mut s = std::collections::BTreeSet::new();
            for (target, sources) in &g.back {
                for src in sources {
                    s.insert((src.clone(), target.clone()));
                }
            }
            s
        };
        assert_eq!(
            from_out, from_in,
            "forward and backlink views must name the same edges"
        );
    }

    // ---- pure `inverse` round-trips over a non-loop map --------------------

    #[test]
    fn inverse_of_forward_recovers_backlinks() {
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        out.entry("/a/x.md".to_string())
            .or_default()
            .insert("/b/y.md".to_string());
        out.entry("/a/x.md".to_string())
            .or_default()
            .insert("/c/z.md".to_string());
        out.entry("/b/y.md".to_string())
            .or_default()
            .insert("/c/z.md".to_string());
        let inb = inverse(&out);
        assert_eq!(
            inb.get("/b/y.md")
                .map(|s| s.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["/a/x.md".to_string()])
        );
        assert_eq!(
            inb.get("/c/z.md")
                .map(|s| s.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["/a/x.md".to_string(), "/b/y.md".to_string()])
        );
    }

    #[test]
    fn inverse_drops_self_loops_even_if_present_in_input() {
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        out.entry("/a/x.md".to_string())
            .or_default()
            .insert("/a/x.md".to_string()); // self loop
        out.entry("/a/x.md".to_string())
            .or_default()
            .insert("/b/y.md".to_string());
        let inb = inverse(&out);
        // x→x is dropped, so x is not a backlink of itself
        assert!(!inb.contains_key("/a/x.md"));
        assert!(inb
            .get("/b/y.md")
            .map(|s| s.contains("/a/x.md"))
            .unwrap_or(false));
    }
}
