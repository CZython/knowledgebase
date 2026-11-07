# Links rework + toc metrics — four tasks (T1–T4)

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
| T4 | `kb toc`: per-section line/char counts (section total + body-only) | pending |

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
highlight uses `raw_to_rendered_span` — a string search for the link's visible
text (label or target) in the rendered line — rather than re-walking the
markdown state machine. `LinksEntry.end` is now carried (CLI-facing, not yet
painted by the TUI). Added `render::tests::span_*` unit tests. Gate 7/7, no
warnings, `cargo test --workspace` green.

**Files touched:** `kb/tui/mod.rs` (`LinksEntry.end`, `LinksState.source_body`,
`open_links`), `kb/tui/render.rs` (`draw_links` two-pane,
`raw_to_rendered_span`, `draw_link_target_info`, `draw` dispatch),
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

**After each task:** CHANGELOG `[Unreleased]`, `PLAN.md` §12 milestone row
+ §13x results, `DESIGN-links-rewire.md` status if link-view-related.

## Conventions (carry-over)

- JSON: `JObject` keys are `&'static str`; `block_array` for raw-object
  arrays, `inline_array` for string arrays.
- CHANGELOG `[Unreleased]` sections: `### Added` / `### Changed` /
  `### Fixed` / `### Removed`.
- MCP work (no `kb verify` tool, links graph-vs-list gap, etag ceremony)
  is **deferred — job for another agent**.
