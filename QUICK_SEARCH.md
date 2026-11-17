# `kb section search` (aliases `kb quick-search` / `kb qs`) — section-first full-text search

> **Status: DESIGN LOCKED — awaiting build.** A design, not (yet) code.
> Captures the agreed goals, the *schema* decision (a new `sections`
> table), the resolved decisions (see "Decisions, resolved" at the
> bottom), and the verb surface. The two user-facing questions are
> closed; the rest are implementation choices the build owns.

## What it returns

**One section's own-body.** Or a ranked narrow-list. Or an empty/error
outcome. That's the whole surface. No preview-line cap, no `--top N`
default: the *consuming* software (GUI hover box, agent loop,
human-in-a-terminal) decides how much of the body to display.

The IDE-hover model the user named: hover over `alice`, and the box
shows the *own-body* of the "alice" section — not a preview, not a
snippet, the whole thing the *user* wrote.

**Human-facing and `--json` are different surfaces.** The human
single-result output is *the body and nothing else* — no
page/section handle, no decoration: a quick lookup returns the answer,
and the *where* (which page, which section) is the **normal
search**'s job, not this one's. The `--json` output returns the
**full record** — page identity, section identity, both body spans,
score — so the software above (hover box, agent) filters and preserves
what it needs. The verb does not decide what is useful.

## Two stated goals

1. **Fast lookup** — `kb qs tag:stories alice` → *the full own-body of
   the best-matched section, and nothing else* (human path). No round
   trip to `show`; no page/section handle. The thyristor case: either a
   section called "Thyristor" in a component list, or the component
   page's body if that *is* the section; either way, one call gets the
   content. The *source* (which page/section) is the normal search's
   job, or the `--json` envelope if the user wants it.
2. **GUI hover** — the box on hover/click shows a section's
   own-body. That requires the section to be an **addressable unit** —
   a row in the DB with `(page_uuid, ref)` identity — not a *derived
   span over a page row* that only exists because we parsed the page
   this call. **The address need not be *stable* across refresh** (a
   `ref` renumbers when a heading is inserted/renamed above it, and a
   page can vanish on GC); the consumer re-resolves by `(page, ref)` on
   every call. We do not store a section uuid, and we do not promise a
   ref outlives its siblings. The two goals share one underlying need:
   **the section is the unit of relevance AND the unit of identity.**

## Why a new table (question 3, answered)

The FTS engine's function family — `fts_match / fts_score /
fts_highlight` over the `pages` FTS index — operates on **page rows**
(`title, description, body, frontmatter_text` as whole rows). That is
not a bug, it is the design: pages are the unit the index understands.
So a derived-view re-score works for the thyristor case (run the page
search, *then* parse each hit's body into sections, re-score by
"section title match + body token count"), but it hits two walls the
GUI-hover goal needs us not to hit:

| Wall                                    | Derived view (Option A, no new table) | Section table (Option B)            |
|-----------------------------------------|---------------------------------------|-------------------------------------|
| **Section address**                     | No row in the DB — a span over a page | `(page_uuid, ref)` is a row identity;|
|                                         | row, recomputed by re-parsing every  | GUI can *address* a section as a DB |
|                                         | call.                                | unit. (Not *stable* across refresh  |
|                                         |                                      | — the consumer re-resolves each call;|
|                                         |                                      | no section uuid is stored.)         |
| **Relevance unit (relevance unit)**     | Ranked by **page** `fts_score`; the  | Ranked by **section** `fts_score`;  |
|                                         | section pick is a post-filter on the | a section is a row in the FTS index |
|                                         | top-N pages. A 20-page essay that    | and can be ranked *above* a page    |
|                                         | mentions "alice" 2× will outrank     | that only mentions her in passing,  |
|                                         | the "alice" character sheet (which   | because the index unit is the       |
|                                         | has her name 2× but is dense).       | section, not the page.              |
| **Cross-page section discovery**        | Only works on *matched pages*; a     | Works on *matched sections*; a      |
|                                         | section whose tokens all match a     | section whose tokens all match wins |
|                                         | *other* page's gate phrase won't be  | regardless of what's in the *page*  |
|                                         | discovered.                          | (e.g., a page about `watchmaking`   |
|                                         |                                      | with a `## Cost` section that       |
|                                         |                                      | mentions `$300` — `q cost` finds    |
|                                         |                                      | that section directly).             |
| **Write cost**                          | Zero — no extra index to maintain.   | `refresh` re-derives sections per   |
|                                         |                                      | *changed* page (incremental); a     |
|                                         |                                      | full rebuild is O(sections).        |

**Recommendation: Option B.** A new `sections` table plus a new `fts`
index over it. The derived-view is a reasonable first cut — the table
can be added *later* if the derived view proves adequate — but the
derived view loses the GUI-hover goal, which the user explicitly wants
on the roadmap. We should not design around that loss.

## The section scheme

```
SCHEMA_VERSION: "2" → "3"
```

A new table in the kb DB. Mirroring the existing `pages` shape
(`kb-db/src/migrate.rs`, `kb-db/src/index.rs` — the
`upsert_page`/reconcile pattern already in the tree):

```sql
CREATE TABLE sections (
    page_uuid   TEXT    NOT NULL,      -- REFERENCES pages(uuid) is decorative:
                                        -- kb-db never sets the `foreign_keys`
                                        -- pragma (Turso), so CASCADE is inert.
                                        -- Page GC deletes sections explicitly
                                        -- (see "Reconcile contract"). No FK col.
    ref         TEXT    NOT NULL,      -- canonical form ONLY: the dotted
                                        -- number (1, 1.2) or the reserved
                                        -- sentinel '*' (page-level row). A
                                        -- title path is a RESOLUTION input,
                                        -- never a stored value (SectionAddr,
                                        -- below).
    ordinal     INTEGER NOT NULL,      -- position in document order, 0-based
                                        -- over the page's heading rows (the
                                        -- parser's numbering order, not the
                                        -- dotted numeric value). Candidate
                                        -- lists sort by (page_uuid, ordinal),
                                        -- so 1.2 precedes 1.10 naturally.
    level       INTEGER NOT NULL,      -- 0 = the page-level row (sentinel);
                                        -- 1..=6 = an ATX heading level
    title       TEXT    NOT NULL,      -- the trimmed heading text; for '*',
                                        -- the page title (pages.title, always
                                        -- present — build_page sets it to the
                                        -- resolved slug)
    own_body    TEXT    NOT NULL,      -- the span start..own_end; subsections
                                        -- EXCLUDED (the hover return). For '*',
                                        -- the PREAMBLE: body[0..preamble_end],
                                        -- the intro text before the first
                                        -- heading (see "The page-level row").
    full_body   TEXT,                  -- the span start..end; subsections
                                        -- INCLUDED (for `--full`). For '*',
                                        -- identical to own_body (the preamble
                                        -- has no subsections; it is flat).
    own_end_line INTEGER,              -- for GUI position hints; for '*',
                                        -- the line of the first heading (or
                                        -- the last body line when the page is
                                        -- heading-less)
    mtime_ns    INTEGER NOT NULL,      -- copied from pages.mtime_ns at build time
    own_hash    TEXT    NOT NULL,      -- etag of (ref, title, own_body)
    PRIMARY KEY (page_uuid, ref)
);

CREATE INDEX idx_sections_page ON sections (page_uuid);
CREATE INDEX idx_sections_ordinal ON sections (page_uuid, ordinal);

-- FTS index. Indexed columns: title + own_body (the two relevance surfaces).
-- `full_body` is STORAGE-ONLY (for `--full` returns); it is NOT indexed —
-- its content is a superset of own_body and an extra FTS column would
-- double-count tokens the query already matches.
CREATE INDEX idx_sections_fts ON sections (title, own_body) USING fts;
```

### The page-level row — the `*` preamble

`kb-core::section` (`Doc::from_data`, section.rs:109) emits a row for
**every ATX heading, level 1..=6** — an H1 *is* a section (`ref 1`).
So a page that opens with `# Thyristor` already has a level-1 row
carrying that heading's own-body; the `*` row would not add to it.
The content the section rows *can* miss is exactly one span: **the
preamble** — the body text *before* the first heading
(`Doc.preamble_end`, section.rs:100). Two shapes of that:

1. **A page whose first heading is H2+** (e.g. `watchmaking-guide.md`,
   whose first heading is `## Parts (Stage 1)`) — the intro paragraph
   before `## Parts` sits in the preamble and belongs to *no* section's
   `own_body`. A FTS over `sections` would never surface it.
2. **A heading-less page** — `preamble_end == 0` (section.rs:122), so
   the *entire* body is preamble. There are no section rows at all.

Fix: **every indexed page contributes exactly one extra row, the
page-level (`*`) row**, whose `own_body` is *only* the preamble — not
the whole page:

- `own_body` = `body[0..preamble_end]`, the text before the first
  heading. **The degenerate case is explicit:** when `preamble_end ==
  0` (heading-less page) the span is the *whole* post-frontmatter body.
  `kb-core::section` has no `preamble()` accessor (only the
  `preamble_end` offset); `build_page` computes the span with that
  `0 → full body` special case. This is the *page itself is a hit* case
  for a page whose answer is its top, not any heading's subsection.
- `ref` = the reserved sentinel `'*'` (a single char, guaranteed to
  never collide with any dotted number (`1`, `1.2`) or ordinal).
- `level` = `0` (below any ATX level; the page, not a heading).
- `title` = `pages.title` (always populated — `build_page` stores the
  resolved slug; for watchmaking this is the frontmatter title
  "Watchmaking: bespoke Flieger build log" — see
  `kb-db/src/index.rs:269`, `identity::resolve_slug`).
- `full_body` = identical to `own_body` (the preamble is flat; it has
  no subsection boundary to cross).
- `own_end_line` = the line of the first heading (the preamble's last
  line), or the page's last body line when the page is heading-less.
- `ordinal` = `0` (it is the first "row" of the page and sorts before
  every heading row — see below).

This row is **always present, one per page**. It is the natural
generalisation of `kb show <page>` with no ref, which already returns
the top of the page.

**Why preamble-only, not the whole page (decided).** The superset
`*` row would let a page *out-score* its own dense sections on a
scattered-token query, stealing rank the section row should win, and
it double-counts every token the query already finds in a child section.
Preamble-only scopes the `*` row to the one span no section owns. A
query whose answer genuinely *is* a sub-section will score that section
's row higher than the page's preamble; a query whose answer is the
page's intro wins the `*` row. The two cases are cleanly separated, and
neither case needs a special weight. `--force` and the ambiguity policy
treat the `*` row exactly like any other candidate.

**Ranking / ordering.** The `*` row's `ordinal` is pinned to `0`, so a
candidate list *within* one page always leads with the page row, then
`1`, `1.1`, `1.2`, … (ordinal `1,2,…`), in reading order. Across pages
the list is still `fts_score DESC`. The `*` row competes on `fts_score`
against every section of every matched page; a strong section on a
different page can and does rank above a weak page's preamble.

**The `hint` for a page-level match** is `kb show <page-uuid>` with
**no ref** — the same call that shows the top of the page today. The
consuming software sees `ref='*'` / `level=0` and knows this is the
*page*, not a sub-section, and renders it accordingly (no `§ 1.3`
prefix).

### The identity scheme: a *pair*, not a hash

The existing `uuid_of(id, abs_path)` (`kb-core/src/uuid.rs:62`) is a
content-derived FNV-1a 64 hash — unique, **not** secret, and the
*stored* identity of a page. By symmetry, a section's identity is the
pair **`(page_uuid, ref)`**:

- `page_uuid` is globally unique (already in `pages.uuid`).
- `ref` is unique *within a page* (that is the invariant that `kb show
  <file> 1.2` and `kb section set 1.2` rely on).
- The pair is therefore globally unique and addressable, **without**
  inventing a new hash. The GUI can address a section as
  `page_uuid + "\0" + ref` or `page_uuid:ref`, no FNV-1a needed.
- Cross-root collision of `type:slug` (`wiki:notes` in two roots) is
  resolved by `page_uuid`, exactly as it is today.

**No stability promise.** `ref` is a *display/numbering* address, not a
durable key: inserting a heading above a section renumbers it, and `GC`
of the page removes the section. That is acceptable because the address
is always **regenerated** — the consumer re-resolves by `(page, ref)`
on every call and never persists a raw `ref` across refreshes. We do
**not** store a section uuid (a stable-id key would promise durability
the underlying page cannot honour, since the page itself can vanish on
refresh/GC). Keep `(page_uuid, ref)`.

### `SectionAddr` — one shared section-address format

Links and quick-search both already speak the same section-address
language; this makes it one type instead of three ad-hoc strings.

- **Links today** carry a `#section` as *free text*, orthogonal to the
  link kind: `AnnotatedLink { target, section: Option<String> }`
  (`kb-core/src/links.rs:492`) — a `[[wiki:components#Cost]]` stores
  `(page=wiki:components, section="Cost")`, i.e. a `(page, atx-header)`
  pair already. The links table stores only the *page* target; the
  section is a side-channel resolved at read time.
- **Quick-search today** would store its `ref` as "dotted number **or**
  title path" — making *both* into stored identities, and a title-path
  a value the parser can't round-trip from a `Doc` (only a human writes
  one).

Merge into one canonical form the whole tree shares:

```
SectionAddr { page: PageId, ref: Option<SectionRef> }

  PageId      = page_uuid  |  type:slug  |  abs_path
                (existing resolver / resolve_target_path — unchanged)
  SectionRef  = "*"                     // page-level row (preamble)
              |  <dotted number>        // canonical 1, 1.2, …
              |  <title path>           // "A > B" — resolution input ONLY
                (all three resolve through SectionRef::resolve → Doc)

  Canonical  = "*" | <dotted number>    // the ONE form the DB stores
```

Consequences for the build:

1. **`sections.ref` stores only the canonical form** — the parser's
   dotted number (`1`, `1.2`, …) or the `'*'` sentinel. A title path
   (e.g. `"Cost / part"`) is a *resolution input* that maps **to** the
   canonical ref; it is never a stored value. This is what kills the
   `1.2` vs `1.10` lexicographic worry at the root: canonical refs are
   ordinal, and the `ordinal INT` column makes even that explicit.
2. **One home for resolution** — `Doc::resolve` (section.rs:523, the
   4-stage dotted → title-path → exact → substring ladder) becomes the
   single function the `links` T8 gate (`section_present`), `kb show
   <ref>`, and `kb qs`'s ambiguous-list hints all call. The same token
   a link writes as `#Cost` and a qs result returns as `ref` resolve
   through the same code.
3. **Link output can align** — `resolves_to` / `candidates` may
   surface `#<ref>` in the canonical dotted form (e.g.
   `wiki:components#1.3`), so a link's section and a qs candidate's
   section are the *same token*. No change to writing/storing — links
   keep the raw `#…` text the user typed (that is the user's voice);
   canonicalisation happens at *resolution* only.
4. **`hint` and the GUI address are one vocabulary** — `hint =
   "<page> <ref>"` (the `'*'` row renders as just `<page>`), identical
   to the link's `#<ref>` stripped of its `#`. Not three encodings.

This retroactively retires the earlier `#3`/`#5` open items: the
returned identity, the GUI address, and the link anchor now share a
single definition.

### Reconcile contract

`build_page` (`kb-db/src/index.rs`) today:

1. Reads the file, parses frontmatter, computes `id / slug / uuid /
   content_hash / mtime_ns`.
2. `upsert_page` (one `INSERT … ON CONFLICT` per page).

After the change, `build_page` *additionally*:

1. Parses the page's `body` into a section tree (already implemented:
   `kb-core::section` — pure, no IO, `Doc`-buffered, returns
   `Vec<Section>` with `start / own_end / start_line / own_end_line /
   level / title / ref`), **plus** computes the preamble span for the
   `*` row: `body[0..preamble_end]`, with the explicit `0 → full body`
   special case for a heading-less page (see "The page-level row").
2. Emits one `upsert_section` per **heading section** (all levels
   1..=6; H1 included — see the `*`-row note) **and one** for the
   `*` page-level row (`ref='*'`). Per row:
    - `page_uuid` (from step 1), `ref` (the Section's dotted ref, or
      `'*'`).
    - `ordinal`: the `*` row is pinned `0`; heading rows take their
      document order among *heading* rows (`1, 2, …`), i.e. `Section`
      index `i → ordinal = i + 1`. This is the parser's *position*, not
      the dotted numeric value, so `1.10` (ordinal 11) never sorts
      before `1.2` (ordinal 2).
    - `level` (the Section's level; `0` for the `*` row), `title`
      (heading text; `pages.title` for `*`).
    - `own_body` (`Section.own_body`; the preamble span for `*`),
      `full_body` (`Section.full_body`; == own for `*`).
    - `own_end_line` (`Section.own_end_line`; first-heading line or the
      last body line for `*`).
    - `mtime_ns` (copied from step 1), `own_hash = etag(ref \0 title \0
      own_body)`.
3. On a page *rebuild* (mtime/hash changed): **one transaction** —
   `DELETE FROM sections WHERE page_uuid = ?` then the re-inserts,
   committed atomically (a page that changes from 0 to N sections, or
   N to 0, must not leave a half-updated section set readers can see).
4. On a page *GC* (file removed from all roots): `DELETE FROM sections
   WHERE page_uuid = ?`. **No `ON DELETE CASCADE`** — `kb-db` never sets
   the `foreign_keys` pragma (it's the Turso façade, `kb-db/src/sql.rs`),
   so the constraint would be inert; the explicit delete is the real
   mechanism.

The *incremental* `refresh` (`kb-db/src/refresh.rs`) already keys on
`(mtime_ns, content_hash)` per page — the sections follow their page's
key. `gc_orphaned_pages` (refresh.rs:226) gains the section delete
(step 4). No new walk logic.

### The fts query over sections

Same function family, same SQL shape as today's `kb search`, just
pointed at `idx_sections_fts`:

```sql
SELECT page_uuid, ref, level, title, own_body, full_body
FROM   sections
WHERE  fts_match(sections.title, sections.own_body, ?fts_q)
   AND  page_uuid IN (
         SELECT pages.uuid AS page_uuid
         FROM   pages
         WHERE  /* the same filter predicates as today: type / language /
                   tag / absence / gate-phrase, already rendered by
                   search_grammar */
        )
ORDER BY fts_score(sections.title, sections.own_body, ?fts_q) DESC,
         sections.ordinal ASC          -- deterministic tie-break within a page
LIMIT  ?limit;
```

The key: **the filter predicates** (whitelisted keys `type: / language:
/ tag: / absence / gate-phrase`) run over `pages`, and the *content
FTS* (`fts_match` + `fts_score`) runs over `sections`. That is the
"leverage the normal search more" principle at the SQL level — we
*reuse* the existing `search_grammar` renderer and the existing
filter/column separation, and only change *which table the content FTS
targets*.

## The verb

**Canonical: `kb section search <query>`** — a new subverb in the
existing `SectionOp` family, alongside `set / replace / append /
insert / rm`. Gives read-verb parity in the section family and lets a
user discover it next to the section-write verbs.

**Aliases (user-facing "fast lookup" names):**

- `kb quick-search <query>` — top-level clap command.
- `kb qs <query>` — clap alias (`#[command(alias = "qs")]`) on the top-level command.

All three names dispatch to the same internal function:
`cmd_section_search(dbpath, query, json, force, full) -> Kb`.
(The `--limit` flag is a SQL-level ceiling, not a *display* cap; the
consuming software applies its own cap.)

In clap terms, `section search` is the canonical `SectionOp::Search {…}` subverb, and `quick-search` (alias `qs`) is a top-level command whose `run` calls the same `cmd_section_search` — two separate clap entry points, one internal handler.

### Grammar — unchanged

`kb quick-search` uses the **same** `search_grammar` (`kb-core/src/search_grammar.rs`)
as `kb search`. That means:

- `type:wiki` / `language:en` / `tag:cast` — the whitelisted
  **filters**. They filter on the *page side*; the section FTS only
  looks at sections *in* matching pages.
- `Null` absence (`type:Null`) — the page filter for "pages that have
  no `type:` / no `language:` / no `tag:`"; the section FTS is still
  over those pages' sections.
- `~OR` / `-NOT` / `"quoted phrase"` — the content tokens render into
  the *same* FTS string, and the *same* `fts_match` call, just against
  the sections index.

**The gate-phrase mechanism (`search_required_phrase`) is unchanged.**
A page with a `gate-phrase` still requires it to be in the query; the
section FTS then runs over those pages' sections. A `qs` query that
satisfies a gate phrase on a *page* can then match the *section* — but
a *gated* page that doesn't satisfy its gate is excluded before the
section FTS even runs, and none of its sections can be hit.

### Output shapes

**Single (human):**

```
<the full own-body, unbroken — and nothing else>
```

A one-result human lookup is *the answer*: just the section's
own-body, unbroken, no page handle, no `§ 1.3`, no title. The
"where" (which page it came from, which section) is the job of the
**normal search** — `kb search thyristor` returns the page and the
section handle. This verb is the *fast path* and returns the
*content*. (If the user wants the *source*, they have the `--json`
envelope, or they run the normal search.) This is the IDE-hover
return, one-to-one: hover → body, click → normal search → source.

**Ambiguous (N > 1, top-1 doesn't clearly dominate):**

```
⚠  ambiguous — 3 sections matched "thyristor":

   1.  wiki:components  ·  1.3  "Thyristor"
   2.  note:electronics ·  2.1  "Power semiconductors"
   3.  wiki:circuit     ·  1.1  "Switching diodes"

  to fetch a full body:   kb show <page-uuid> 1.3
  to force top-1:         kb qs thyristor --force
```

This is a *narrowing* list. It does **not** truncate *each row's*
body to a preview; it points the user at *how* to fetch each one. The
consuming software (GUI, agent) does the truncation.

**Empty:**

```
×  no section matched "thyristor"  (page matches: 0)
   did you mean:  kb qs tag:wiki thyristor
```

### `--json` envelope (the software-above surface)

The **full record** for every row. Every page/section field the verb
knows is returned; the consuming software filters and preserves. The
verb does *not* omit fields "because they look like fluff" — that is
the consumer's call, not the search's.

```
{
  "status": "single" | "ambiguous" | "empty",

  "query": "<the original query string, verbatim>",

  "match": {
    "page_uuid": "…",              // full 16-hex page identity
    "page_uuid_short": "…",        // shortest-unique prefix (resolver-friendly)
    "page_display_id": "wiki:components",   // type:slug
    "page_title": "…",
    "page_type": "wiki",
    "ref": "1.3",                  // canonical dotted number; '*' for the
                                    // page-level (preamble) row
    "level": 2,                    // 0 for the page-level row, else 1..=6
    "section_title": "Thyristor",  // the page title for the '*' row
    "own_body": "…",              // span, subs EXCL; the PREAMBLE
                                   // (body[0..preamble_end], or the whole
                                   // body for a heading-less page) for '*'
    "full_body": "…",             // span incl. subs; == own_body for '*' row
    "own_end_line": 124,          // GUI position hints
    "ordinal": 5,                // document order within the page (* is 0)
    "own_hash": "…",              // etag of (ref, title, own_body)
    "score": 12.4                 // fts_score over (title, own_body)
  } | null,
  // `match` is populated iff status == "single" (or "ambiguous" + --force)

  "candidates": [                  // populated iff status == "ambiguous"
    {
      "page_uuid": "…",
      "page_display_id": "wiki:components",
      "ref": "1.3",
      "level": 2,
      "ordinal": 5,
      "section_title": "Thyristor",
      "score": 12.4
    },
    { "page_uuid": "…", "ref": "*", "level": 0, "ordinal": 0,
      "section_title": "Watchmaking: bespoke Flieger build log", "score": 4.1 },
    { … }
  ],

  "hint": "<page-uuid> <ref>" | null   // copy-pasteable `kb show` target;
                                        // for the '*' row, ref is omitted
                                        // (kb show <page-uuid>, no ref)
}
```

For the GUI: read `status`; `single` → render `match.own_body` (or
`match.full_body` for `--full`); `ambiguous` → iterate `candidates`
and fetch a candidate's full body on demand via `kb show`
(→ `candidate.hint` is the paste target); `empty` → surface the
narrowing hint. The `match.*` fields the consumer does not render
(`page_uuid`, `own_hash`, `score`, `page_display_id`, …) are
**returned anyway** — preserve them, cache them, or drop them.

## Flags (the minimal set)

Four surviving flags. `--json` and `--full` are already
vocabulary in the `kb show` surface; `--force` and `--limit` are new
to this verb:

| flag        | effect                                                             |
|-------------|--------------------------------------------------------------------|
| `--json`    | emit the full-record envelope above; the default is the human shapes |
| `--force`   | always take top-1 (skip the ambiguity check; emit the top section)  |
| `--limit N` | cap the `LIMIT ?` in the SQL (default 20); the *consuming* software can cap further |
| `--full`    | emit `full_body` (incl. subsections) instead of `own_body`           |

**Not in the surface:** `--max-preview-lines N` (the *return* is
never truncated to a preview; the *consuming* software does that),
`--top N` (same — it's not a display cap, it's a *result-set* cap, and
`--limit` already does that).

## Reuse of existing machinery

| existing thing                                  | how `quick-search` reuses it                                       |
|-------------------------------------------------|--------------------------------------------------------------------|
| `search_grammar.render_fts()`                   | Unchanged. The FTS string is rendered the same way; it just goes   |
|                                                 | against `sections` instead of `pages`.                              |
| filter predicates (`type: / language: / tag: /  | Unchanged. They apply to *page* rows; a *section* is included only  |
| absence / gate-phrase)                           | if its `page` satisfied the predicates.                             |
| `kb_core::section::Doc` + `parse_tree`          | Unchanged. The section-tree is computed over `page.body` and each  |
|                                                 | `Section` is one row in `sections`.                                 |
| `kb_core::etag::of`                             | Unchanged. `own_hash` for a section row is `etag(ref \0 title \0   |
|                                                 | own_body)`.                                                        |
| `uuid_of(id, abs_path)`                         | Unchanged. `page_uuid` is the existing FNV-1a 64 hash of page's    |
|                                                 | id + abs_path.                                                     |
| `kb show <file> <ref>`                          | Unchanged. The user (or the GUI) fetches a section's own-body by   |
|                                                 | `show`. This verb returns it; the user doesn't need a round-trip    |
|                                                 | to `show`.                                                         |
| `SCHEMA_VERSION`                                | Bumped `2 → 3` in `kb-db/src/migrate.rs`; the one-logical-schema   |
|                                                 | transaction pattern is reused.                                      |
| `refresh` (incremental)                         | Unchanged. Sections follow their page's `(mtime_ns, content_hash)`  |
|                                                 | key; no new walk logic.                                             |
| `fts_match / fts_score / fts_highlight`         | Unchanged. The same function family; applied to the sections index  |
|                                                 | in addition to (not instead of) the pages index.                    |

## What this verb is *not*

- **Not a substitute for `kb search`.** `kb search` is page-level,
  returns pages, and is correct for the "which pages mention this?"
  question. `kb quick-search` is section-level, returns sections, and
  is correct for the "which section *is* this?" question.
- **Not a `section:<name>` filter on the grammar.** That's a *different
  verb* — "find the ## Install section anywhere" — and it needs a
  grammar-level filter (a new `key` in `FILTER_KEYS`), not a new table.
  (The new table doesn't add it *by itself*; it would be a follow-up.)
- **Not the MCP verb yet.** When `MCP.md` is open-sourced, this verb is
  what `kb_quick_search` wraps under MCP. The MCP surface stays
  tabled.
- **Not a GUI.** The verb emits text (or JSON). The IDE-hover box is the
  *consuming* software that reads the envelope. That software renders
  the `section.own_body` however it wants (truncated, full, coloured).

## Decisions, resolved (closed)

**A. `*`-row semantics — preamble-only.** The page-level row's
`own_body` is the **preamble** (`body[0..preamble_end]`), *not* the whole
page. It is the *page itself is a hit* case **only when the answer is
the page's top and no section matched better** — exactly the watchmaking
orphaned-intro shape, and the whole body for a heading-less page
(`preamble_end == 0`). A scattered-token query whose answer is a
sub-section scores that section's row higher; a query whose answer is
the intro wins `*`. The degenerate `0 → full body` special case is
explicit in `build_page`. *The two earlier candidate readings
(discounted superset; `*`-only-as-fallback) are both discarded. H1 is
already a section, so `*` never needs to "cover" it.*

**B. Section identity — `(page_uuid, ref)`, no stored section uuid;
`ref` stores canonical form only.** `ref` is a *display* address that is
always **regenerated on refresh** and never promised to outlive its
siblings (a page can itself vanish on GC), so a durable section-id key
would be illusory. We keep `(page_uuid, ref)`, and `ref` holds only the
canonical dotted number or `'*'`; a title path is a *resolution input*
(`Doc::resolve`) that maps to the canonical ref and is never stored.

**C. `SectionAddr` — one shared address format for links + qs.** See
"§ SectionAddr" above: `SectionAddr { page, ref }`, with
`SectionRef = '*' | dotted | title-path` and the *canonical* subset
(`'*' | dotted`) as the stored form. One `Doc::resolve` home for the
links gate, `kb show`, and `kb qs`. This retires the earlier `#3`/`#5`
open items (returned identity, GUI address, and link anchor are now one
vocabulary).

## Build-owned choices (no more user input; the build decides)

1. **Default `--limit` for the SQL.** 20 is the first guess; the
   consuming software can cap further.
2. **`--force` semantics.** *Ignite the ambiguity test, emit top-1*
   (the IDE-hover "take your best guess" reading). Not the strict
   "exactly one" reading.
3. **`own_body` byte-length cap on the *query* return.** DB stores
   full; a future `--byte-limit` for consumers. Default: no cap.
4. **Fence handling in `own_body`.** Verify the `fence_of` track
   (section.rs:812) in the synthetic fixture before relying on it — a
   `#` inside a fenced block must be body, not a heading. *This is the
   #4 spike: a scratch DB + a synthetic multi-section + fenced page,
   measuring the `*`-preamble-vs-section ranking before DDL lands.*
5. **`ordinal` column.** Already in the DDL above. Confirmed by the
   design (not a spike); the fixture in #4 exercises it.
