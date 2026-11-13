# Links — reference grammar, resolution, and render contract

One coherent rule lets a page reference another reference (a page, a section
of a page, or an external URI) in standard, recognizable Markdown-ish syntax,
resolved through the single `kb-core::resolve` waterfall, and rendered
consistently by the TUI palette (resolved = styled, no brackets; unresolved =
verbatim prose). A bare path in prose is *not* a link; only the bracket forms
are.

## Why this shape

The prior (v1) extractor recognised two shapes only — a `[[ … ]]` wikilink
and a *bare* whitespace-bounded path-token caught by a heuristic
(`is_pathlike` = `contains('/')` or known extension). Consequences: standard
Markdown `[ text ]( target )` was invisible; punctuation glued to a path
(`foo.pdf)`) stopped the extraction; only markdown pages in the corpus ever
resolved; and there was no stable rule for what a link's *text* is once
resolved, so the TUI couldn't distinguish "a working link" from "prose I
mistook for a link."

The grammar below replaces that: one scanner (`detect_refs` +
`classify_target`), one resolver (`kb-core::resolve`), one render contract
(pure function of the resolved outcome).

## The one rule

> **The brackets are syntax the resolver consumes. Resolved → paint it as a
> link (styled, no brackets). Unresolved → the brackets were never meaning;
> render the whole raw token as plain prose.**

Three layers, cleanly separated:

1. **Scanner** (pure, in `kb-core::links`) — turn source text into `RawLink`
   rows: a *kind* (Db / Path / External / Current), a *target*, and an
   optional display text (the inline label).
2. **Resolver** (`kb-core::resolve`, pure, Verus-verified) — the
   `type:slug → slug → path → substring` waterfall. Every kind funnels through
   this *one* waterfall. A kind is a **lookup strategy**, not an identity —
   a path that resolves and a db title that resolves to the *same* page are
   the same target.
3. **Renderer** (shell, pure) — `link_state(...)` / `render(...)`, a pure
   function of the resolved outcome. No new proof obligations: layers 1 and 3
   are shell over the already-verified core.

## The grammar

Bare token = **path** search. A token wrapped in its *own* `[ ]` pair = **DB**
search. The **outermost** `[ ]` (or `[ label ]( … )`) is "this is a link."
`#section` layers on top of *either*.

| Wrapper | Syntax | Display text (what the user sees) | Kind |
|---|---|---|---|
| **DB page, no label** | `[[ target ]]` | title of the resolved page | **db** |
| **Path, no label** | `[ path ]` | title of the resolved page | **path** |
| **Path, with label** | `[ label ]( path )` | `label`, verbatim | **path** |
| **DB, with label** | `[ label ]( [ target ] )` | `label`, verbatim | **db** |
| **Current-page section** | `[ # section ]` | `# section` (no page title — page is self-evident) | **current** |
| **Any + section** | `<above>` with `#section` appended to the target | display text + `  #  section` | as above |
| **External URI** | `< https://… >` | the bare URI | **external** |

### Scanning (left-to-right)

- Scan `[[ … ]]` first (inner may itself be a bare or bracketed target).
- Else `[ … ]`: read to the matching `]`; peek — if the *next non-whitespace*
  is `(`, it is the inline form `[ label ]( target )` and the text inside
  `()` is the target; otherwise the bracketed text is the target.
- Else `< … >`: read to the first `>` (**no inner whitespace** — CommonMark
  autolink rule, so `a < b` math is *not* a link); a well-formed URI →
  external, anything else → **not a link** (stay plain, no phantom edge).
- A lone `[` with no closing `]` is decorative (skip).
- `#` splits the **inner** target on the **first** `#` (keeps inner spaces:
  `Cost / result`). Empty page-part before the `#` = current page.
- Inner padding is trimmed before classification, so `[# x]` ≡ `[ # x ]` and
  `[lang.md]` ≡ `[ lang.md ]`.

## Render contract

The renderer is a **pure function of (kind, resolved, section, display)**:

```
resolved =
    current-section, empty page-part          → "#" + section
    other-page, inline label present          → label        (+ "  # " + section if a section was also given)
    other-page, no label                      → page.title   (+ "  # " + section if a section was also given)
unresolved →
    the whole raw token verbatim (plain prose; the brackets were never a link)
external →
    the bare URI (no scheme decoration, palette role `link_external`)
```

**Reference table** (3rd column = what the *user sees* after resolution). The
title `Watchmaking guide` is the canonical title of **one** page
(`/mnt/…/watchmaking-guide.md`, slug `watchmaking guide`); both row 1 (path)
and row 2 (db) resolve to it, so both render to the **same title** — that
*invariance across kind* is the point (kind = lookup strategy, not identity).

| # | Source in the file | Innermost | **Rendered (styled link text)** | Kind | Section |
|---|---|---|---|---|---|
| 1 | `[ /mnt/…/watchmaking-guide.md ]` | bare path | `Watchmaking guide` | path | — |
| 2 | `[[ watchmaking guide ]]` | `[ title ]` | `Watchmaking guide` | db | — |
| 3 | `[ watches cost ]( /mnt/…/watchmaking-guide.md )` | bare path | `watches cost` | path | — |
| 4 | `[ watches cost ]( [ watchmaking guide ] )` | `[ title ]` | `watches cost` | db | — |
| 5 | `[ # Cost / result ]` | `#…` (empty page) | **`# Cost / result`** | current | Cost / result |
| 6 | `[[ watchmaking guide#Cost / result ]]` | `[ title#… ]` | `Watchmaking guide  #  Cost / result` | db | Cost / result |
| 7 | `[ watches cost ]( [ watchmaking guide#Cost / result ] )` | `[ title#… ]` | `watches cost  #  Cost / result` | db | Cost / result |
| 8 | `[ /mnt/…/watchmaking-guide.md#Cost /result ]` | bare path `#…` | `Watchmaking guide  #  Cost /result` | path | Cost /result |
| 9 | `< https://arxiv.org/… >` | URI | `https://arxiv.org/…` | external | — |

**Reading the table:**

- **Rows 1 & 2 are the same page, same rendered text, different kind.** A
  no-label link always renders to the resolved page's title — a reader
  cannot tell from the text whether it was found by path or by db title.
  Correct by construction.
- **Rows 3 & 4 override the title with the author's label** (`watches cost`);
  the resolved title is used only when the author gave no label.
- **Row 5 is the exception**: a current-page section names no page, so it
  renders as **just** `# section` (no title — the page is the one being read).
- **Rows 6–8 are rows 2–4 with a section** appended as `  # <section>`.
- **Row 9 is external**: the bare URI, never a page title.

### Palette roles the renderer consumes

| Outcome | Role | Symbol (double-encoding) |
|---|---|---|
| resolved internal (path or db, +optional section) | `link` | *(none)* |
| external URI | `link_external` | `↗` |
| unresolved (raw token verbatim) | `default` (plain prose) | *(none)* |

### Consequences of the contract (deliberate, not bugs)

- A **bare path** and a **db title** that both resolve to the *same* page
  render to the **same title** (rows 1 and 2 *must* be identical). Correct —
  kind is a lookup strategy, not identity, and a no-label link's text is
  always the resolved page's title. If a reader needs to know *how* it was
  found, that's a separate subtle palette marker / tooltip, not part of the
  text.
- **Unresolved is invisible**: the source token renders verbatim, so prose
  that happens to look like a link (a path to a non-markdown file, a
  punctuation-glued token) stays prose. No false-positive link edges.
- **The `#` is the same-page cue**: a current-page anchor renders as just
  `# section` — no redundant page title, because the page is already the one
  being read.

## `kb links` — the two views

- **Per-occurrence list (default).** The richest agent-facing view — the
  *same* `annotated_links()` output the TUI `l` picker and `kb verify` check d
  read. Emits a numbered report with `kind` (via `style = kind.style_str`),
  `label`, `target`, `#section`, `resolves_to`, and `candidates`. This is the
  default `kb links <target>` shape; `--json` is the machine view.
- **Legacy edge set + backlinks (`--dedup`).** The pre-rewire view: the
  dedup'd `(target, style)` pair set + backlinks, still available as
  `kb links <target> --dedup[--json]` for scripts that pinned it.

Both views read the same `annotated_links()` body, so they cannot disagree.

## TUI `l` picker

The link picker in the TUI (Read mode) is a two-pane, find-style view:

- **Left pane (≈60 %)** — the *source* page body, with the focused link
  highlighted (`find_current`) and every other occurrence lighter
  (`find_match`). The focused link is kept centred via `find_padding`
  recomputed from the *actual* pane dimensions each frame (resize-safe).
  Alive links are spliced in-place to their display text (`splice_links_on_
  line`, high-to-low over `col`/`end`) so `[label](path)` reads as `label`;
  dead / ambiguous links render as plain prose.
- **Right pane (≈40 %)** — the focused link's *target info*: kind (short
  tag + full word), label, target, `#section`, resolution status (resolved /
  ambiguous + candidate list / external / current / dead), and a highlighted
  excerpt of the source line.
- `↑/↓` / `Home` / `End` move focus. `Enter` opens the target; on an
  **ambiguous** hit the inline candidate sub-state takes over (walk the
  collision set, `Enter` opens the highlighted, `Esc` returns to the flat
  list). `Esc` / `x` from the flat list pops back to the origin Read.
- Find-aware: `l` while find is armed pre-selects the link **nearest** the
  highlighted hit (one-keystroke "I just finished find, follow this" path).

## `#section` navigation

For a resolved link carrying a `#section`, the TUI `open_linked` uses
`kb-core::section::section_lookup_line` to find the heading's logical line
in the *open* body and scrolls the reader to it. The section lookup runs on
the **raw** body (so a spliced render does not change it); a missing /
ambiguous section falls back to row 0. The current-page form (`#` with empty
page-part) uses the same path against the source page; a present section
earns the navigable-section affordance (`🔗`); a missing section is Dead.

## Non-goals

- Standard Markdown *reference-style* links (`[t][label]` +
  `[label]: target`) — only the inline `[ t ]( target )` form.
- Navigating to an external URI from the TUI (shown + copyable, not opened).
- Multiple / nested sections (`#a#b`) — one `#section` per link.
- Backtick `` `…` `` as a link form — it is inline **code**, not a link.

## Implementation status

All of the above is shipped and gate-green:

| Piece | State |
|---|---|
| Core grammar (`kb-core::links::detect_refs` + `classify_target`, `LinkKind`, `RawLink`, `Ref`) | ✅ shipped |
| Shell rewire (`annotated_links`, `kb links --list`, `kb verify` check d, TUI `LinksEntry` + render) | ✅ shipped |
| `kb links` default flip (per-occurrence list is the **default**; edge set + backlinks demoted to `--dedup`) | ✅ shipped |
| TUI `l` picker UX rework (find-style single-link focus + right-pane target info; self-computed centering; `raw_to_rendered_span`) | ✅ shipped |
| `#section` navigable anchor (best-effort via `section_lookup_line` on the raw body) | ✅ shipped |
