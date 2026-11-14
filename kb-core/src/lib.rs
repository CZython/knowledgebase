//! # kb-core — the *proven* half of `knowledgebase` (PLAN.md §10)
//!
//! Two layers coexist in this crate:
//!
//! 1. **Exec** — pure Rust data + helpers, lives in `src/lib.rs` (and
//!    modules like `src/meta.rs`). No `std::io`, no DB engine, no `clap`,
//!    no side effects. Runs under `cargo build` / `cargo test`.
//!
//! 2. **Spec (verified)** — the invariants we commit to, written as
//!    Verus specs + proofs. Lives in `spec/*.rs`, verified by the
//!    *standalone* `verus` binary (see `kb-core/verify.sh` or
//!    the top-level `./verify.sh`).
//!
//! Why the two? This fork of Verus rejects user-defined types declared
//! outside `verus!` blocks (the error: "cannot use type X ... declared
//! outside the verus! macro"). The working reference project
//! `git-restricted_version_B` therefore keeps its entire *verified* set
//! of functions in a single `fn main()` spec file with `&str` parameters
//! only, and verifies it standalone. We follow the same shape.
//!
//! The two layers are pinned together:
//!
//! - the **exec** `NoteMeta.as_display_id()` is checked by unit tests to
//!   produce exactly what the **spec** `display_id_*` predicates say
//!   (spec↔exec agreement tests in `src/meta.rs`);
//! - the **spec** invariants (pairwise-distinct prefixes, decomposition,
//!   etc.) are discharged by Z3 via `./verify.sh`.
//!
//! If a future refactor breaks *either* side, the corresponding test or
//! proof fails — the model and the code cannot drift apart silently.
//!
//! ## Modules (exec)
//!
//! - `meta` — the `NoteType` / `NoteMeta` data model, the
//!   `as_display_id()` helper, and the *concrete* unit tests that pin
//!   exec behaviour to the spec's shape (Appendix A examples).
//! - `frontmatter` — the permissive YAML-subset parser (M1): scalars,
//!   inline + block lists, `#` comments; `type:` gate, `description:`
//!   ≤255 flag, tag dedupe, verbatim `search_required_phrase`.
//! - `identity` — slug resolution (title → H1 → filename) and the
//!   `type:slug` display-id assembly (M1).
//! - `tags` — tag-string normalization (store: trim / drop-empty /
//!   dedupe, case-preserving) + query-time canonicalize (case-fold +
//!   alias conflate) (M2).
//! - `links` — wikilink (`[[ … ]]`) / path-ref extraction + 3-way style
//!   classification (`wikilink | relative | absolute`) + (to,style)
//!   dedup (M2).
//! - `graph` — forward/backlink adjacency over resolved edges,
//!   self-loops dropped, with the inverse relation exposed as a pure
//!   helper (M2).
//! - `labels` — deterministic 3-way (files-heading / leading-run /
//!   folder) classification of a `type: label` page's references;
//!   well-formedness predicate for a single file ref (M2).
//! - `uuid` — the page identity token: `uuid_of(id, abs_path) =
//!   etag_of(id \0 abs_path)` (FNV-1a 64, 16 hex, salted by the PK),
//!   `is_uuid_token` (5–16 hex), and `shortest_qualifying` (the abstract
//!   "shortest unique prefix" walk — the concrete indexed DB lookup lives in
//!   `kb/cli/mod.rs`, memory bounded to ≤2 rows per probe).
//!
//! ## Fuzz corpus
//!
//! `tests/fuzz_corpus.rs` — a *deterministic*, seeded property-based
//! driver over the four M2 modules. Not cargo-fuzz (separate harness):
//! a fixed-seed LCG drives ~5000 random inputs and re-asserts the
//! invariants that the specs in `spec/{tags,links,graph,labels}_spec.rs`
//! prove symbolically — a cheap, portable "fuzz corpus" that catches
//! property bugs no single fixed input would surface. Reproducible.
//!
//! ## Specs (verified — in `spec/`)
//!
//! - `spec/meta_spec.rs` — the display-id invariants (prefix distinctness,
//!   decomposition, suffix recovery) with proofs. (M0.)
//! - `spec/fm_spec.rs` — the frontmatter contract (kind partition, description
//!   bound, tag hygiene, `search_required_phrase` gate shape) with proofs. (M1.)
//! - `spec/identity_spec.rs` — the slug-priority decision + display-id
//!   assembly + well-formedness shape with proofs. (M1.)
//! - `spec/tags_spec.rs` — normalize is idempotent / duplicate-free /
//!   empty-drop is a total+disjoint partition; canonicalize is a total +
//!   conflating decision. (M2.)
//! - `spec/links_spec.rs` — style ⟹ target shape (3-way exclusive),
//!   wikilink-never-a-path, dedup idempotent (M2).
//! - `spec/graph_spec.rs` — inverse / no-self-loop / adjacency-consistency
//!   over the resolved-edge set (M2).
//! - `spec/labels_spec.rs` — 3-way exclusive classification, priority
//!   (case-1 beats case-2), well-formed-ref / relative-ref partition
//!   (M2).

pub mod aliases;
pub mod etag;
pub mod frontmatter;
pub mod fs_snapshot;
pub mod graph;
pub mod identity;
pub mod labels;
pub mod links;
pub mod meta;
pub mod resolve;
pub mod search_grammar;
pub mod section;
pub mod semantics;
pub mod tags;
pub mod uuid;
