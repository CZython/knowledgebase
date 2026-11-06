# Links — shell rewire onto the unified reference grammar (addendum)

**Status addendum to `DESIGN-links.md`.** The locked design (`DESIGN-links.md`)
defines the *grammar* and the three-layer split (scanner / resolver /
renderer). This file records the **implementation status** of rewiring the
shell (`kb`) from the retired v1 extractor (bare-path heuristic + wikilink
only) onto that grammar. It does not restate the design — for the rules, the
table, the one-rule, and the scope split, read `DESIGN-links.md`.

## Scope split — implementation status

`DESIGN-links.md` carves the work into two increments. Both *shell-facing*
halves are now landed and gate-green; the core `#section` *jump* is the one
core-touching piece that is still isolated (it touches `resolve`).

| Increment | State | Commit |
|---|---|---|
| **Phase 1 — core grammar** (`kb-core::links::detect_refs` + `classify_target`, `LinkKind`, `RawLink`, `Ref`) | ✅ shipped | `a9e1add` |
| **Phase 1 — shell rewire** (`annotated_links`, `kb links --list`, `kb verify` check d, TUI `LinksEntry` + render) | ✅ shipped | (this rewire) |
| **Phase 2 — `#section` jumpable anchor** (core: `Resolution::Unique → Anchor{page,section}` via `kb_core::section::resolve`) | ⏳ isolated | — |

## What the Phase 1 shell rewire did (vs. the v1 extractor it replaced)

The retired v1 rule recognised *two* shapes only — a `[[ … ]]` wikilink and a
**bare** whitespace-bounded path-token caught by a heuristic
(`is_pathlike` = `contains('/')` or known extension). Under the new grammar a
bare path token in prose is **not** a reference at all; only the bracket forms
are. The scanner is now a single `detect_refs(line)` + `classify_target(raw)`
pair; the padding around the inner content is trimmed before classification, so
`[# link-test]` / `[lang-test.md]` (no inner space) classify identically to
`[ # link-test ]` / `[ lang-test.md ]`.

The shell surface that consumed the extractor was rewired in lockstep so that
TUI, `kb links --list`, and `kb verify` keep reading **one** source of truth
(`annotated_links`) and therefore cannot disagree:

- **`AnnotatedLink`** (`kb/main.rs`) gained `kind: LinkKind`,
  `label: Option<String>`, `section: Option<String>`; `style` is still present
  but derived from `kind` at render time (`kind.style_str(&target)`).
- **`push_annotated`** classifies each `RawLink`; on `#section` it splits the
  off suffix, strips the inline-`[ … ]` db wrapper, and feeds the page-part to
  `resolver::resolve`. `External` / `Current` arms early-return (no resolution)
  — they are informational, never navigable.
- **`kb links --list`** (`kb/cli/links.rs`): JSON `style` now
  `kind.style_str`, plus new `label` / `section` fields; the human render shows
  the label in front of the target, the section as an annotation, and skips the
  resolution annotation for `External` / `Current`.
- **`kb verify`** check d (`kb/cli/verify.rs`): kind-based dispatch —
  `External`/`Current` are counted, never dead; `Db`/`Path` feed the
  dead/ambiguous/resolved partition. New `links_current` JSON key.
- **TUI** (`kb/tui/mod.rs` + `kb/tui/render.rs`): `LinksEntry` carries
  `kind` / `label` / `section`; `e_style_str` matches on `kind`;
  `draw_links` renders a kind-aware status line
  (`External` → `(external)`, `Current` → `(this page)`, else the candidate /
  dead-link hint); `links_open_selected` prints a kind-aware dead-link message.

## Verification

- `bash gate.sh` **7/7 green** (build, one-pkg, clippy, tests, verus, behavior,
  dep-tree).
- Unit tests in `kb/main.rs` rewritten for the bracket grammar (bare path-
  token now asserts **empty**; new cases: plain-bracket path, inline labeled
  path, inline labeled db, external URI, current section, section-split-
  resolve, no-padding forms).
- Integration pin `kb-user/tests/m9_agent_surface.rs::links_list_json_shape`
  moved the dead-path body from a bare token to `[ notes/missing.md ]`.
- Live corpus demo `/mnt/ARIA/knowledgebase/link-test.md` — 17 links:
  10 resolved, 2 external, 3 current, 2 dead (two intentional stubs),
  0 ambiguous; `verify` verdict `ok-with-warnings` (exit 0). Confirms
  `[# link-test]` and `[lang-test.md]` resolve identically to their padded forms.
