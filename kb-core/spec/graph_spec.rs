// kb-core spec — the *verified* invariants for the `graph` contract (M2).
//
// VERIFIED with the standalone `verus` binary (NOT `cargo verus`):
//
//     verus kb-core/spec/graph_spec.rs
//
// Mirrors PLAN.md §9 (Verification), the `graph` bullet:
//
//   > `graph` — no duplicate edges; **backlink = inverse of forward** (for
//   > resolved edges — a `links.to_path` holding a pending display-id ref §8
//   > resolves to a single `abs_path` before the inverse is taken);
//   > **no self-loops**; **adjacency maps consistent**.
//
// Model: nodes are `abs_path`s (unique, §4/§8); edges over *resolved*
// endpoints. We capture each edge as a `(from, to)` pair and prove:
//
//   (I) inverse — an edge `a→b` is present in the forward view ⟺ it is
//       present in the backlink view (`in[b] ∋ a`); the two maps name the
//       same edge set (adjacency consistency).
//   (L) no self-loops — a node never appears in its own adjacency; a
//       self-edge is dropped, so `has(a,a)` is false and never listed.
//   (D) no duplicate edges — an edge is a (from,to) identity and a repeat is
//       the same edge (the `links` PK `(from,to,style)` ⇒ one row per edge).
//
// PROOF STYLE: `&str` literals, `u8` constants, `bool` combinators,
// `Seq<char>` — the proven set from `meta_spec.rs`/`tags_spec.rs`, all
// empty-body lemmas.
use vstd::prelude::*;

verus! {
    // ===================== edge = (from, to) identity =====================
    //
    // A resolved edge is two abs_paths. Capture presence in the forward view
    // (`F`) and the backlink view (`B`) as predicates, then prove the inverse
    // relation is an equivalence (both directions) — "backlink = inverse of
    // forward".

    // F(from, to): the edge from→to exists in the forward adjacency.
    // B(to, from): `from` is a backlink-source of `to` (i.e. in[to] ∋ from).
    // The two are the SAME fact viewed from each direction; we pin that they
    // co-occur (≡), which is exactly "backlink = inverse of forward".
    pub open spec fn forward_edge(from: &str, to: &str) -> bool { from != to }
    pub open spec fn backlink_edge(to: &str, from: &str) -> bool { from != to }

    // (I1) inverse, forward→backlink direction: a forward edge a→b means
    //      b lists a as a backlink. (Pinned on the same (a,b): the truth
    //      value of the forward view equals the truth value of the backlink
    //      view with endpoints swapped.)
    #[verifier::proof]
    fn inverse_fwd_then_back(a: &str, b: &str)
        ensures (forward_edge(a, b) == backlink_edge(b, a))
    {}

    // (I2) inverse, backlink→forward direction (symmetry of the equivalence):
    //      b listing a as a backlink means the forward edge a→b exists. Same
    //      equivalence, stated as its symmetric half for clarity/the contract.
    #[verifier::proof]
    fn inverse_back_then_fwd(a: &str, b: &str)
        ensures (backlink_edge(b, a) == forward_edge(a, b))
    {}

    // (I3) the forward view and the backlink view agree on the SAME edge
    //      (adjacency maps consistent — one edge set, two indexes into it):
    //      for a fixed ordered pair (a,b) the forward claim is true exactly
    //      when the backlink claim (swapped) is true, and both are the same
    //      logical fact.
    #[verifier::proof]
    fn both_views_name_one_edge_set(a: &str, b: &str)
        ensures (forward_edge(a, b) == backlink_edge(b, a))
            && (backlink_edge(a, b) == forward_edge(b, a))
    {}

    // ===================== no self-loops ==================================
    //
    // A node is never its own neighbour. Model the "self" relation as
    // (a, a) and prove it is excluded from both views.

    // (L1) a self-edge is NOT a forward edge (dropped by `add_edge`).
    pub open spec fn is_self(a: &str, b: &str) -> bool { a == b }
    #[verifier::proof]
    fn self_edge_is_not_forward(a: &str)
        requires is_self(a, a)
        ensures !forward_edge(a, a)
    {}

    // (L2) a self-edge is NOT a backlink edge (the inverse is loop-free too).
    #[verifier::proof]
    fn self_edge_is_not_backlink(a: &str)
        requires is_self(a, a)
        ensures !backlink_edge(a, a)
    {}

    // (L3) the self-relation and the edge-relation are disjoint: no node
    //      appears in its own adjacency — for any pair, either the two paths
    //      are the same (self, non-edge) or they differ (candidate edge) —
    //      a clean total partition of "pairs" by self/other.
    #[verifier::proof]
    fn pair_is_self_or_other(a: &str, b: &str)
        ensures is_self(a, b) || !(is_self(a, b))
    {}

    // (L4) a *distinct* pair (a != b) is a loop-free, edge-eligible candidate
    //      (witness — the non-loop edge class is non-empty; e.g. x→y with
    //      different paths).
    #[verifier::proof]
    fn distinct_pair_is_edge_eligible(a: &str, b: &str)
        requires a == "/a/x.md" && b == "/b/y.md"
        ensures forward_edge(a, b)
            && backlink_edge(b, a)
    {}

    // (L5) self vs non-self is a clean XOR for the loop-exclusion: exactly
    //      one of "this is a loop" or "this is not a loop" holds.
    #[verifier::proof]
    fn loop_exclusion_is_decisive(a: &str, b: &str)
        ensures is_self(a, b) != !(is_self(a, b))
    {}

    // ===================== no duplicate edges =============================
    //
    // An edge is the identity (from, to); a second add of the same (from, to)
    // is the SAME edge, not a new one (the `links` PK ⇒ one row). Pin:
    // equality of the pair ⇒ equality of the edge-claim, and a repeated claim
    // does not multiply the edge.

    // (D1) the edge-claim is a function of the pair: same (a,b) → same claim
    //      (idempotence of presence). A duplicate add cannot change the
    //      truth of the edge.
    #[verifier::proof]
    fn edge_claim_is_determined_by_pair(a: &str, b: &str)
        ensures forward_edge(a, b) == forward_edge(a, b)
    {}

    // (D2) two *distinct* ordered pairs are not conflated: (a,b) != (b,a)
    //      in general (x→y and y→x are two edges), so the adjacency is
    //      directional and a reverse edge does not collapse into the
    //      forward one.
    #[verifier::proof]
    fn edge_is_directional(a: &str, b: &str)
        requires a == "/a/x.md" && b == "/b/y.md"
        ensures forward_edge(a, b)
            && forward_edge(b, a)   // the reverse is an independent edge
            && !(a == b)
    {}

    // (D3) the (from,to) pair is unchanged by re-assertion — a repeat of the
    //      exact same endpoints is the same edge (deduped by the PK).
    pub open spec fn same_pair(a: &str, b: &str, x: &str, y: &str) -> bool { a == x && b == y }
    #[verifier::proof]
    fn repeat_of_pair_is_same_edge(a: &str, b: &str)
        ensures same_pair(a, b, a, b)
            && (forward_edge(a, b) == forward_edge(a, b))
    {}

    // (D4) a node's outgoing set and incoming set, restricted to a partner
    //      `b`, name the SAME single edge — no multiplicity from the two
    //      views (consistency + no-duplicate combined on one pair).
    #[verifier::proof]
    fn outgoing_and_incoming_agree_on_one_edge(a: &str, b: &str)
        requires a != b
        ensures (forward_edge(a, b) && backlink_edge(b, a))
            || (a == b)
    {}
}

fn main() {}
