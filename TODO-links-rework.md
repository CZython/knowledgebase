# Links rework + toc metrics — tasks (T1–T9)

**Context:** the unified reference grammar (DESIGN-links.md phase 1) landed
(`a9e1add` core, `3329e04` shell). This file tracks the follow-on UX rework
the user requested. **DESIGN-links.md is LOCKED** — addenda go in
`DESIGN-links-rewire.md` (or a sibling), never the original.

Gate: `bash gate.sh` (7 stages, must stay green). Git repo:
`/home/jakub/programming/Rust/knowledgebase`. Demo corpus (outside repo):
`/mnt/ARIA/knowledgebase` — `link-test.md` has 17 links covering all forms
(10 resolved, 2 external, 3 current, 2 dead, 0 ambiguous; verified exit 0).

## Status

| # | Task | State |
|---|------|-------|
| T1 | `kb links`: default = annotated list; `--dedup` = old edge-set+backlinks view | **done** (shipped) |
| T2 | TUI links view rework: find-style single-link focus + right-pane target info | **done** (shipped) |
| T3 | TUI Reader: open at a target body line (so `#section` links land mid-page) | pending |
| T4 | `kb toc`: per-section line/char counts (section total + body-only) | **done** (gate green) |
| T5 | Indexer: switch `extract_links` → `detect_refs` + `classify_target` (v2 grammar) | **done** (gate green) |
| T6 | All edit pathways: auto-reindex the single page after a successful write | **done** (gate green) |
| T7 | `section_lookup(body, ref) → Result<Option<usize>, SectionError>` — line of section heading | **done** (gate green) |
| T8 | Wire the section lookup into the resolver: page resolves but `#section` missing → **ambiguous** | pending |
| T9 | Wire the section lookup into the TUI reader: section links open / recenter at the heading line | pending |

## T1 — `kb links` default flip

- `kb links <target>` → the per-occurrence annotated list (today's `--list`).
- `kb links <target> --dedup` → the old dedup'd edge-set + backlinks view (today's default).
- `--json` applies to whichever view is chosen.

**Files:**
- `kb/main.rs` — CLI enum `Links { target, list: bool, json: bool }` (≈L413–435),
  dispatch call into `cmd_links`, help text.
- `kb/cli/links.rs` — `cmd_links(dbpath, target, list: bool, json: bool)` (L36);
  swap the branches (list path becomes default, edge-set path behind `dedup`).
  `--list` can stay as a deprecated alias or be removed — decide in-pass.
- `kb-user/tests/m9_agent_surface.rs` — `links_list_json_shape` pin may need
  the flag spelled as `--dedup`/none to match the new default.
- `CHANGELOG.md` `[Unreleased]` Changed entry.

**Next concrete step:** add `dedup: bool` to the `Links` arm in `main.rs`,
flip the dispatch, rename the `list` param in `cmd_links`, update help +
tests, run `bash gate.sh`.

## T2 — TUI links view: find-style (DONE)

Rework `Mode::Links` from a flat numbered list to the **reader-find** model
(`f` in Read): `↑/↓` focuses a single link occurrence and the source body is
kept visible with the focused link highlighted (`find_current`), all other
occurrences lighter (`find_match`); a right pane shows the focused link's
*target info* (kind, label, target, section, resolution status, candidates).
`Enter` opens the target (candidate picker sub-state preserved); `Esc` back to
Read.

**Implementation note:** the rework is **self-contained** — `draw_links`
computes its own centering from its *actual* layout-split pane dimensions each
frame (via `find_padding`) rather than reading a shared `FindState::center`,
so it is resize-safe and needs no input-pass recenter hook (`recenter_links`
was tried and dropped: the render pass is the single source of truth). The
left body pane **splices** each *alive* link — one that uniquely resolves, or
an external / `#section` ref — down to just its display text
(`is_alive_link` + `splice_links_on_line`, splicing high-to-low over
`col`/`end`), so `[label](path)` reads as `label` with a recoloured span
(`find_current` focused / `find_match` others) exactly like the reader renders
`**bold**` / `*italic*`. **Dead** (unresolved) and **ambiguous** links are
skipped and render as ordinary prose. Highlighting then uses
`raw_to_rendered_span` — a string search for the link's visible text (label
or target) in the rendered line — rather than re-walking the markdown state
machine. `LinksEntry.end` is now carried (CLI-facing). Added
`render::tests::span_*` (highlight helper) and `splice_*` (splicing, incl.
multi-link-per-line ordering) unit tests. Gate 7/7, no warnings,
`cargo test --workspace` green.

**Files touched:** `kb/tui/mod.rs` (`LinksEntry.end`, `LinksState.source_body`,
`open_links`), `kb/tui/render.rs` (`draw_links` two-pane,
`raw_to_rendered_span`, `is_alive_link`, `splice_links_on_line`,
`draw_link_target_info`, `draw` dispatch),
`kb/main.rs` (`AnnotatedLink.end`, `push_annotated`), `kb/tui/input.rs`
(`handle_links`).

## T3 — TUI Reader: land mid-page on a line

`open_read` always starts at `scroll_top = 0`. Extend so a link target with
a `#section` (or explicit line) opens the target **centered on that section's
heading line**. Compute the line from the target page's body (heading match
via `kb-core` `section` resolve — `kb-core/src/section.rs`).

**Files:** `kb/tui/mod.rs` (`open_read`, `links_open_selected`,
`load_body`, `scroll_body_top`), `kb/tui/render.rs` (`draw_read` if a
"landed at section" marker is wanted).

## T4 — `kb toc` per-section metrics

`kb toc <file>` currently lists the section tree only. Add per-section
**line count** (whole section incl. subsections) and **char count**
(body-only or full — decide in-pass, both in JSON; human view: whole section).

**Files:** wherever `cmd_toc` lives (find via `grep toc kb/main.rs`
`kb/cli/*.rs`), `kb-core/src/section.rs` (spans already carry line bounds —
verify), JSON + human renderers, tests.

**Done:** `cmd_toc` (`kb/cli/show.rs`) now emits per-section metrics from the
existing `Section` bounds (no new fields). JSON gains `lines`/`chars` (full
span incl. subsections) and `body_lines`/`body_chars` (own body only); the
human view appends `[N lines, M chars]` (whole section). Decision: JSON
carries **both** spans as the spec suggested. Pinned in
`kb-user/tests/section_cli.rs::toc_json_is_well_formed_envelope`.

## T5 — Indexer: v1 → v2 reference grammar

**Verified:** `kb-db/src/index.rs:222` still calls
`kb_core::links::extract_links` (v1: wikilink `[[…]]` + bare-token
heuristic), and `kb-db/src/refresh.rs:262` writes the `links` table from
that. The v2 grammar already landed (`detect_refs` + `classify_target`)
and is the **only** path used by the TUI / CLI `links` command via
`annotated_links`. The indexer must follow.

**Goal:** replace `extract_links` with the v2 scanner in `index_page_links`
so that `kb refresh` indexes `[[…]]`, `[label](path)`, `<url>`, `[…](…)`
— the full v2 grammar — and the `links` table matches what `kb links`
reports.

**Sub-tasks:**
1. Decide the `links` schema delta: does the current table
   (`from_path, to_path, style`) need a `label` column? If the user
   wants to *see* the label in a backlinks view, yes — add it.
   If the table is still target-only, leave it.
2. `kb-core/src/links.rs` — `extract_links` is currently pub; check
   whether any other call-site exists before deleting / deprecating.
3. `kb-db/src/index.rs:222` — swap to `detect_refs` + `classify_target`
   and insert the resolved target into `to_path` (using `resolver` if
   needed, or store the raw target and resolve at `kb links` time).
4. Refresh tests (`kb-db/tests/*`, `kb-user/tests/*`) — any pin that
   asserts the old v1 extraction will need updating.
5. `spec/links_spec.rs` — the v1 spec pins wikilink + path-token
   behaviour; the v2 spec (if any) should cover the full grammar.

**Files:** `kb-core/src/links.rs`, `kb-db/src/index.rs`,
`kb-db/src/refresh.rs`, `spec/links_spec.rs`, `kb-db/tests/*`.

## T6 — Auto-refresh after every edit pathway

**Goal:** any CLI write-op that changes a page's body or frontmatter
runs `kb refresh` (or the targeted equivalent) **in the same process**
immediately after the `commit` succeeds, so the index, FTS, and
`links` table are always consistent with disk. The user should never
have to manually run `kb refresh` after `kb section`, `kb create`,
`kb replace`, `kb patch`, etc.

**Why now:** T5 makes the index depend on the v2 grammar; without
auto-refresh a `kb section append` followed by `kb links` would show
stale data. (Before T5 the index was only consulted for backlinks,
which was a rarer user flow; the T7–T9 chain makes `links` + section
lookup the primary consumer.)

**Scope:** every write path in `kb/cli/section.rs`, `kb/cli/editor.rs`,
`kb/cli/create.rs`, `kb/cli/tag.rs`, `kb/cli/writeops.rs`.

**Implementation options (pick one in-pass):**
1. **Post-commit hook** — after each `commit(file, orig, new, …)` (or
   the equivalent), call `kb_refresh::reconcile_one(db, abs_path)`
   (a new or existing single-file reconcile entry-point). This is the
   narrowest change: the index updates immediately, the output can
   still print the edit result.
2. **`commit` wrapper** — add an optional `refresh: bool` param to
   `commit()` in `writeops.rs`; all callers set it `true` except
   `--dry-run` (which skips it).
3. **Global `at_exit` / `Drop` guard** — every process that opens a
   write DB handle registers a flush; runs once on clean exit.
   (Fragile — signal handling; prefer 1 or 2.)

**Sub-tasks:**
1. Check if `kb-db/src/refresh.rs` already exposes a single-file
   reconcile function (it has `refresh` (full) — see if a
   `reconcile_one(abs_path, db) -> Result<..>` helper exists or is
   easy to extract).
2. Wire the call into each write-path post-`commit` (or in `commit`
   itself — option 2).
3. `--dry-run` must NOT trigger refresh (the write didn't happen).
4. `--json` output: should the refresh summary be included? If yes,
   add it as a `"refresh": {…}` key in the existing envelope. If no,
   keep the existing shape.
5. Tests: `kb-user/tests/*` — the existing section/edit tests should
   still pass; add a test that a `section set` followed by a fresh
   `kb links` call (in the same test harness, same DB) shows the new
   link.

**Files:** `kb/cli/writeops.rs` (`commit`), `kb/cli/section.rs`,
`kb/cli/editor.rs`, `kb/cli/create.rs`, `kb/cli/tag.rs`,
`kb-db/src/refresh.rs` (single-file helper), tests.

**Implemented (decision — option 2, commit wrapper).** `commit()` now takes
`dbpath: Option<&Path>` as its first arg and, after a successful write, calls
`reindex_after_write(dbpath, path, json)` (`kb/main.rs`). That helper opens
the DB, finds the winning root via `roots_with_seq` + `winning_root`, then
`build_page` + `upsert_page` the single written file (the same pair
`create.rs::index_created` and the TUI `commit_edit` already use). Best-effort:
a re-index miss (no roots / not indexable / no slug) is a stderr note, not an
error — disk stays authoritative and `kb refresh` still repairs. `--dry-run` and
the no-change path return before the call, so they never re-index, and under
`--json` notes go to stderr to keep stdout a single clean JSON doc (no
`"refresh"` key added — envelope shape unchanged). `commit()` is the single
choke point (all 10 call sites are `section.rs`), so `--db` is threaded through
`cmd_section` / `cmd_replace` / `cmd_patch` from the dispatch arms. `create.rs`
already indexes via `index_created`; `editor.rs` writes through the same
`commit()`; `tag.rs` only rewrites `tag_aliases` rows (no page body), so it
needs no hook. Tests: `kb-user/tests/section_cli.rs` —
`t6_replace_adds_link_visible_without_manual_refresh` +
`t6_section_set_adds_link_visible_without_manual_refresh` (a write that adds a
`[[B]]` link is immediately visible to a fresh `kb links` process).

## T7 — Section lookup: `section_lookup(body, ref) → Option<usize>`

**Goal:** a pure function in `kb-core/src/section.rs` that takes a
body (`&str`) and a section reference (dotted number `1.2`, title
`Watchmaking`, or title path `Guide > Watchmaking`) and returns the
**0-based line index of the heading** that matches. `None` if no
section matches, `Some(line)` if unique, a `SectionLookupError` if
ambiguous (or simply `None` + a separate `ambiguous` flag — decide
in-pass).

**Why in kb-core** (not kb-db or CLI): it's a pure parse over text;
the T7 test can be a `#[test]` in `section.rs` with no DB; T8 wires it
into the resolver in `kb-core::resolver`; T9 wires it into the TUI
in `kb-user`.

**Sub-tasks:**
1. Reuse `Doc::resolve` (which returns a `Section` with `start_line`)
   as the implementation. `Doc` is already in `kb-core::section`.
   `section_lookup(body, ref_) -> Result<Option<usize>, SectionError>`
   where `Err(SectionError::Ambiguous{…})` is the caller's signal.
2. Add a convenience `section_lookup_line(body, ref_) -> Option<usize>`
   that maps `Ambiguous` → `None` (for callers that don't care).
3. Unit tests in `section.rs`: dotted number, case-insensitive title,
   title path, ambiguous, not-found, empty body, heading at line 0,
   heading at last line. ≥10 cases.

**Files:** `kb-core/src/section.rs` (function + tests),
`kb-core/src/lib.rs` (re-export if needed).

**Implemented (decision — `Err` = the ambiguity signal, not `None`).**
`section_lookup(body, ref_) -> Result<Option<usize>, SectionError>` lives in
`kb-core/src/section.rs` (free fns, re-exported via the existing
`pub mod section`). It builds a `Doc` and delegates identity to the same
4-stage `Doc::resolve` cascade, then maps the winning section to
`Section::start_line` (0-based heading line). Contract: `Ok(Some(line))` = a
unique match; `Ok(None)` = nothing matched (`NoSection` / `EmptyRef` /
`NotFound`) — the page resolved but the `#section` names a section that doesn't
exist there; `Err(SectionError::Ambiguous { .. })` = 2+ sections matched. A
`section_lookup_line(..) -> Option<usize>` convenience folds `Ambiguous` → `None`
(`unwrap_or_default()`) for callers that only jump on a real hit. Chose
`Err(Ambiguous)` (not a `found + ambiguous` tuple) over the spec's "or None +
a flag" alternative so the existing `SectionError::Ambiguous { ref_, candidates }`
carries the full candidate list verbatim — T8's resolver can surface it directly
to the "please disambiguate" message. 8 unit tests cover dotted numbers, case-
insensitive titles + title paths, not-found (dotted/title/empty ref/empty body),
ambiguity (title + dotted disambiguation), single-H1 → line 0, heading at last
line, and the `section_lookup_line` fold. No new `kb-core` exports needed
(`section` was already `pub mod`). Reuses no new deps.

## T8 — Resolver: `#section`-missing → ambiguous (not dead)

**Goal:** in `kb-core/src/resolver.rs` (or wherever `resolver::resolve`
lives), when a link target is `<something>#<section>` and the page
resolves uniquely but the section is **not found** in the page's body,
the result should be `Resolution::Candidates` (ambiguous) — **not**
`Resolution::Unique` (current behaviour) and **not**
`Resolution::Empty` (dead).

**Rationale:** the page exists so the link is not "dead" (the user can
still open the page); but the `#section` is wrong so the link is not
"uniquely resolved to a target" either — it needs disambiguation
(like a page-name collision). The TUI should show this in the
candidate picker sub-state ("page opened, section not found — go to
top?").

**Sub-tasks:**
1. `resolver` currently takes `&[resolver::Page]` where `Page` has
   `abs_path`, `typ`, `slug`. Does `Page` carry `body`? If not, the
   resolver needs the body for section lookup. Options:
   a. Extend `Page` struct with `body: String` (or `body: Option<String>`).
      Cost: the corpus load in `kb::load_corpus` already reads the
      body (for FTS), so it's available at load time.
   b. Add a separate `body_lookup: HashMap<String, String>` param.
   (a) is cleaner — the resolver is the single place that needs
   both the page and its body.
2. In `resolve()`, after `Resolution::Unique(abs)`: if the target
   carried a `#section` suffix, call `section_lookup(body, section)`.
   If `None` → treat as `Candidates([abs])` with a `"section_missing"`
   marker (the TUI / CLI can render it as "page exists, section not
   found" rather than an actual candidate list).
3. `AnnotatedLink.resolved_id` — when `section_missing`, is the link
   still "resolved" (the page is known)? `AnnotatedLink` already has a
   `section: Option<String>` field; the TUI / CLI can check
   `resolved_id.is_some() && section.is_some() && !sections_present`
   to detect this case. Or add a `resolution: Resolution` enum to
   `AnnotatedLink`.
4. Tests: `kb-core/src/resolver.rs` — a page with body `# H1\nbody\n# H2\nbody`
   and target `H1` → unique; target `H3` → ambiguous/section-missing;
   target `H2` → unique at line 3.
5. Wire into the TUI's `link_state()`: a resolved page with a missing
   section is `Resolved` (navigable) — the TUI opens the page (T9
   will handle the no-scroll case: scroll to top with a "section not
   found" hint in the status bar).

**Files:** `kb-core/src/resolver.rs`, `kb-core/src/section.rs`
(export `section_lookup`), `kb/main.rs` (`AnnotatedLink` if a
`resolution` field is added), `kb-core/tests/*`.

**Blocked on:** T7 (needs `section_lookup`).

## T9 — TUI: open / recenter at section heading

**Goal:** when the user presses `Enter` on a link that carries a
`#section` (or the current page has a `[[…#section]]` self-ref), the
reader view should **scroll to the line of that section's heading**
and centre it (the existing `scroll_body_top` / `find_padding_for_cursor`
machinery already supports centring — it's just never called with a
non-zero target line).

**Sub-tasks (T3 was already scoped for this, T9 adds the section
lookup on top):**
1. `kb/tui/mod.rs` — `open_read` / `links_open_selected` /
   `open_linked`: accept an optional `target_line: Option<usize>`.
   If `Some(line)`, set `scroll_top` to centre on that line
   (`find_padding_for_cursor` / the existing centre math).
   If `None`, `scroll_top = 0` (current behaviour).
2. `AnnotatedLink` / `LinksEntry` — carry the resolved section's
   `start_line` (or compute it at open time via `section_lookup`
   T7, which is a pure function on the body — no DB needed).
3. TUI status bar hint: when landing on a section, show
   `"→ § <section title>"` in the find-style header. When the section
   was **not found** (T8's ambiguous case), show
   `"! section '<sec>' not found — at top"`.
4. `kb/tui/render.rs` — `draw_read` may need a one-line "landed at
   section" marker if the user wants it (T3 optional).
5. `kb/tui/mod.rs` — `ReadState.scroll_top` initialisation: if a
   `target_line` is given and `body.lines().count() > target_line`,
   set it; if the target line is past the end (body changed since
   resolve), fall back to 0.
6. Tests: mock a page with 3 headings; link to `#section-2` →
   `scroll_top > 0` after `open_read`; link to `#missing` → `scroll_top
   = 0` + the "section not found" hint is in the rendered frame.

**Files:** `kb/tui/mod.rs` (`open_read`, `links_open_selected`,
`open_linked`, `ReadState.scroll_top`), `kb/tui/render.rs`
(`draw_read` optional marker), `kb/main.rs` (carry `start_line` on
`AnnotatedLink` if not already), tests.

**Blocked on:** T7 (`section_lookup`) and T8 (resolver knows the
section is missing).

**After all T5–T9:** CHANGELOG `[Unreleased]`, `PLAN.md` §12 milestone
row + §13x results, `DESIGN-links-rewire.md` status.

## Conventions (carry-over)

- JSON: `JObject` keys are `&'static str`; `block_array` for raw-object
  arrays, `inline_array` for string arrays.
- CHANGELOG `[Unreleased]` sections: `### Added` / `### Changed` /
  `### Fixed` / `### Removed`.
- MCP work (no `kb verify` tool, links graph-vs-list gap, etag ceremony)
  is **deferred — job for another agent**.
