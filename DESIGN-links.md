# Links — unified reference grammar, resolution, and render contract

**Design doc.** Goal: one coherent rule that lets a page reference another
reference (a page *or* a section of a page *or* an external URI) in standard,
recognizable Markdown-ish syntax, resolved through the **single**
`kb-core::resolve` waterfall, and rendered consistently by the TUI palette
(resolved = styled, no brackets; unresolved = verbatim prose). Closes the gap
where `kb` recognized only `[[…]]` wikilinks + bare path tokens and silently
missed everything else — including standard inline links.

## Why

- Today the extractor (`kb-core::links::extract_links`, `kb/main.rs::annotated_links`)
  only knows two shapes: `[[ target ]]` (wikilink) and a *bare* whitespace-bounded
  path-like token (relative/absolute). There is no "no display text, but
  unambiguous" form for a path — and a bare path in prose is detected by a
  *heuristic* (`is_pathlike` = `contains('/')` **or** known ext) with a
  whitespace-only token boundary, so punctuation glues to it (`foo.pdf)` is
  *not* extracted) and only *markdown* pages in the corpus ever resolve.
- Nothing that looks like standard Markdown `[text](target)` is a link at all —
  the whole §4 collision / candidate-picker machinery fires only on wikilinks
  and bare paths.
- The renderer has no stable contract: there is no rule for what a link's *text*
  is once resolved, so the palette can't distinguish "a working link" from
  "prose I mistook for a link".

## The one rule

> **The brackets are syntax the resolver consumes. Resolved → paint it as a link
> (styled, no brackets). Unresolved → the brackets were never meaning; render
> the whole raw token as plain prose.**

Three layers, cleanly separated:

1. **Scanner** (shell, pure) — turn source text into `Link` rows: a *mode*
   (path / db / external / current-section), a *target*, and an optional
   **display text** (the inline label).
2. **Resolver** (`kb-core`, pure, **already built**) — `resolver::resolve` runs
   the §4 waterfall (type:slug → slug → path → substring). Every mode funnels
   through this *one* waterfall. Mode is a **lookup strategy**, not identity —
   a path that resolves and a db-title that resolves to the *same* page are the
   same target.
3. **Renderer** (shell, pure) — `render(link)`, a pure function of the resolved
   outcome. **No new proof obligations**: layers 1 and 3 are shell over the
   already-verified `kb-core`; only the `#section` *jump* (Phase 2) touches core.

## The grammar

Bare token = **path** search. A token wrapped in its *own* `[ ]` pair = **DB**
search. The **outermost** `[ ]` (or `[label]( )`) is just "this is a link".
`#section` layers on top of *either*.

| Wrapper | Syntax | Display text (what the user sees) | Mode |
|---|---|---|---|
| **DB page, no label** | `[[ target ]]` | **title of the resolved page** | **db** |
| **Path, no label** | `[ path ]` | **title of the resolved page** | **path** |
| **Path, with label** | `[ label ]( path )` | `label`, verbatim | **path** |
| **DB, with label** | `[ label ]( [ target ] )` | `label`, verbatim | **db** |
| **Current-page section** | `[ # section ]` | `# section` (no page title — page is self-evident) | **current** |
| **Any + section** | `<above>` with `#section` appended to the target | display text (per above) + `  #  section` | as above |
| **External URI** | `< https://… >` | the bare URI | **external** |

### Precedence / scanning (left-to-right)

- Scan `[[ … ]]` first (existing check wins; inner may itself be a bare or
  bracketed target).
- Else `[ … ]`: read to the matching `]`; peek — if the *next*
  non-whitespace is `(`, it is the inline form `[ label ]( path )` and the text
  inside `()` is the target; otherwise the bracketed text is the target.
- Else `< … >`: read to the first `>` (**no inner whitespace** — CommonMark
  autolink rule, so `a < b` math is *not* a link); a well-formed URI → external,
  anything else → **not a link** (stay plain, no phantom edge).
- A lone `[` with no closing `]` is *decorative* (skip), mirroring today's
  `[[` fallthrough.
- `#` splits the **inner** target on the **first** `#` (keeps inner spaces:
  `Cost / result`). Empty page-part before the `#` = current page.

## Render contract

The renderer is a **pure function of (mode, resolved, section, display)**:

```
resolved =
    current-section, empty page-part          → "#" + section
    other-page, inline label present          → label        (+ "  # " + section if a section was also given)
    other-page, no label                      → page.title   (+ "  # " + section if a section was also given)
unresolved →
    the whole raw token verbatim (plain prose; the brackets were never a link)
external →
    the bare URI (no scheme decoration, palette.link_external)
```

**Reference table** (3rd column = what the *user sees* after resolution). The
title `Watchmaking guide` is the canonical title of **one** page
(`/mnt/…/watchmaking-guide.md`, slug `watchmaking guide`); both row 1 (path)
and row 2 (db) resolve to it, so both render to the **same title** — that
*invariance across mode* is the point (mode = lookup strategy, not identity).

| # | Source in the file | Innermost | **Rendered (styled link text)** | Mode | Section |
|---|---|---|---|---|---|
| 1 | `[ /mnt/…/watchmaking-guide.md ]` | bare path | `Watchmaking guide` | path | — |
| 2 | `[[ watchmaking guide ]]` | `[ title ]` | `Watchmaking guide` | db | — |
| 3 | `[ watches cost ]( /mnt/…/watchmaking-guide.md )` | bare path | `watches cost` | path | — |
| 4 | `[ watches cost ]( [ watchmaking guide ] )` | `[ title ]` | `watches cost` | db | — |
| 5 | `[# Cost / result]` | `#…` (empty page) | **`# Cost / result`** | current | Cost / result |
| 6 | `[[ watchmaking guide#Cost / result]]` | `[ title#… ]` | `Watchmaking guide  #  Cost / result` | db | Cost / result |
| 7 | `[ watches cost ]( [ watchmaking guide#Cost / result ] )` | `[ title#… ]` | `watches cost  #  Cost / result` | db | Cost / result |
| 8 | `[ /mnt/…/watchmaking-guide.md#Cost /result ]` | bare path`#…` | `Watchmaking guide  #  Cost /result` | path | Cost /result |
| 9 | `< https://arxiv.org/… >` | URI | `https://arxiv.org/…` | external | — |

**Reading the table:**
- **Rows 1 & 2 are the same page, same rendered text, different mode.** A
  no-label link always renders to the resolved page's title — a reader cannot
  tell (from the text) whether it was found by path or by db title. Correct by
  construction.
- **Rows 3 & 4 override the title with the author's label** (`watches cost`);
  the resolved title is used only when the author gave no label.
- **Row 5 is the exception**: a current-page section names no page, so it
  renders as **just** `# section` (no title — the page is the one being read).
- **Rows 6–8 are row 2–4 with a section** appended as `  #  <section>`.
- **Row 9 is external**: the bare URI, never a page title.

### Palette roles the renderer consumes

| Outcome | Role | Symbol (double-encoding) |
|---|---|---|
| resolved internal (path or db, +optional section) | `link` | *(none)* |
| external URI | `link_external` | `↗` |
| unresolved (raw token verbatim) | `default` (plain prose) | *(none)* |

These two new roles (`link`, `link_external`) are *added* to the palette role
vocabulary (see `TODO-palette.md`); every other rendered style already maps to
an existing role.

### Consequences of the contract (deliberate, not bugs)

- A **bare path** and a **db title** that both resolve to the *same* page render
  to the **same title** (rows 1 and 2 *must* be identical). Correct — mode is a
  lookup strategy, not identity, and a no-label link's text is always the
  resolved page's title. If a reader needs to know *how* it was found, that's a
  separate subtle palette marker / tooltip, not part of the text.
- **Unresolved is invisible**: the source token renders verbatim, so prose that
  happens to look like a link (e.g. a path to a non-markdown file, or a
  punctuation-glued token) stays prose. No false-positive link edges.
- **The `#` is the same-page cue**: a current-page anchor renders as just
  `# section` — no redundant page title, because the page is already the one
  being read.

## Scope split (two phases)

**Phase 1 — wrappers + render contract (shell-only, gate-green now).**
`LinkStyle` stays as-is (it already encodes *target* shape: wikilink/relative/
absolute). Add:
- `display: Option<String>` to `kb_core::links::Link` (the inline label) — or
  carry it shell-side in `AnnotatedLink` (preferred, keeps core untouched).
- Three new scanner branches in `extract_links` + `annotated_links`:
  `[x]`, `[x](y)`, `<url>`, plus the inner-`[ ]` = db vs bare = path distinction.
- A `url`/external outcome (shown in `--list`, **never** resolved/navigable).
- The render contract above as a pure `render(link)` over the existing palette.

No new proof obligations; `./verify.sh` is unchanged; `bash gate.sh` must stay
7/7.

**Phase 2 — `#section` jump (core + Verus, isolated).**
- `Resolution::Unique(String)` → carry an optional section, **or** a new
  `Anchor { page, section }`. Either way, `kb-core/spec/resolve_spec.rs`
  changes → this is **verus-gated** and lands in its own commit + its own
  `bash gate.sh`.
- Wire the already-built `kb_core::section::resolve` (title-path / dotted-number
  matching — `Cost / result` *and* `#3.2` both work) to the page's sections at
  annotation time.
- `AnnotatedLink` gains a `section: Option<String>`; TUI/CLI render it (the
  `  #  …` suffix above), and the TUI `open_linked` jumps to the section instead
  of page-top.

Phase 1 already *renders* `Title  #  Section` from the scan-split (the `#` split
is shell), so the palette contract is **stable** when Phase 2 turns the suffix
into a jumpable anchor. That's why the split is safe: Phase 2 changes the
*resolution outcome*, not the *render shape*.

## Non-goals (this design)

- Standard Markdown *reference-style* links (`[t][label]` + `[label]: target`)
  — out of scope; only the inline `[t](target)` form.
- Navigating to an external URI from the TUI (they're shown + copyable, not
  opened).
- Multiple sections / nested `#a#b` — one `#section` per link.
- Backtick `` `…` `` as a link form — it's inline **code**, not a link (a common
  instinct, deliberately not supported).

## Open / settled

- **Settled**: the grammar, the render contract, the one-rule, the phase split,
  the `<>` = URI / `[]` = path+db / `[[ ]]` = db distinction.
  Innermost-bracket rule: `[label](path)` is **path** mode, `[label]([title])`
  is **db** mode.
- **Settled**: a `<…>` that isn't a well-formed URI (math `a<b`) is *not* a link.
