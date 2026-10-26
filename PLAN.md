# `kb` — a Rust knowledge-base tool (search, browse, tag, link, **and** edit)

## Contents

Source-comment cross-refs throughout the workspace cite these numbers (e.g. `PLAN §9`, `PLAN §10`, `§13f`); this table is the one place to resolve any of them. The **§9** proven-vs-assumed split and the **§10** workspace layout are the load-bearing ones — the comments that gate "is this line verified or assumed?" point at them.

| §        | Title |
|----------|-------|
| [1](#1-what-it-is) | What it is |
| [2](#2-guiding-principles) | Guiding principles |
| [3](#3-locked-decisions) | Locked decisions |
| [4](#4-page--identity-model) | Page & identity model |
| [5](#5-search-model) | Search model |
| [6](#6-tags-and-labels-two-separate-ideas) | Tags and Labels (two separate ideas) |
| [7](#7-section-editing-full-mdtoolpy-port) | Section editing (full `mdtool.py` port) |
| [8](#8-database-schema-rusqlite--fts5) | Database schema (rusqlite / FTS5) |
| [9](#9-verification-verus-proven-vs-assumed) | Verification (Verus: proven vs. assumed) |
| [10](#10-workspace-layout) | Workspace layout |
| [11](#11-cli-reference-flat-kb-cmd---db-path-defaults-to-localsharekbkbdb) | CLI reference (flat `kb <cmd>`) |
| [12](#12-milestones) | Milestones |
| [13](#13-m0-open-items-to-settle-non-blocking-for-the-design) | M0 open items to settle |
| [13a](#13a-m0-results--verified-and-pinned-in-code) | 13a — M0 results |
| [13b](#13b-m1-results--frontmatter--identity-verified-and-pinned-in-code) | 13b — M1 results |
| [13c](#13c-m2-results--tags--links--graph--labels--fuzz-corpus) | 13c — M2 results |
| [13d](#13d-m3-results--search_grammar--semantics--fs_snapshot-verified) | 13d — M3 results |
| [13e](#13e-m4-results--section-port-of-mdtoolpy-verified-and-pinned) | 13e — M4 results |
| [13f](#13f-m6-results--the-kb-single-file-agent-cli-built--tested) | 13f — M6 results (single-file agent CLI) |
| [13g](#13g-m6b-results--frontmatter-kernel-ops--fmfmkey-cli-surface) | 13g — M6b results (frontmatter kernel) |
| [13h](#13h-m5-results--the-kb-db-indexer-built--tested) | 13h — M5 results (kb-db indexer) |
| [13i](#13i-m7-results--the-kb-multi-file-namespace-cli-built--tested) | 13i — M7 results (multi-file CLI) |
| [13j](#13j-m7b-results--cross-page-link-resolution-built--tested) | 13j — M7b results (link resolution) |
| [13k](#13k-m7c-results--tag-alias-importexport-the-recovery-table-built--tested) | 13k — M7c results (tag-alias import/export) |
| [13l](#13l-m80-results--kb-tui-scaffold--browse-built--tested) | 13l — M8.0 results (TUI scaffold + browse) |
| [13m](#13m-m81-results--kb-tui-read--filter-built--tested) | 13m — M8.1 results (TUI read + filter) |
| [13n](#13n-m82-results--kb-tui-edit--autocomplete-built--tested) | 13n — M8.2 results (TUI edit + autocomplete) |
| [13o](#13o-m83-results--kb-tui-alias--roots-manager-built--tested) | 13o — M8.3 results (TUI alias + roots) |

## 1. What it is

A single Rust CLI, `kb`, with two flat command namespaces that share one **Verus-verified**
core crate:

- **Multi-file namespace** — register many (very deep) roots into **one embedded SQLite
  database**, then search / tag / link / browse across all of them.
- **Single-file namespace** — a port of `mdtool.py`: view and edit **sections** of one Markdown
  page by dotted ref (`3.2`) or title path (`Installation > Quick Start`), **without reading the
  whole file** (byte scan + byte splice, atomic save).

**The workflow they form:**
`kb search "AI deep learning"` → page path → `kb show <path> "3.2"` (read) →
`kb section --file <path> set "3.2" --from-file new.md` (write) → back to search.

## 2. Guiding principles

1. **Disk is authoritative.** Every *content* fact is in a note's frontmatter/body or derivable
   from its location. Delete the DB and you can rebuild **every derived table (pages content
   columns, page_tags, page_gates, links)** 1:1 from the tree. The only DB-resident, *non-derivable*
    state is `roots` (the list of registered roots **plus each root's `extra_exts` /
   `include_hidden` walk options — §3 Extensions/Walk**) + `tag_aliases` (§13.7) + the small
   `meta` runtime-table (e.g. `last_refresh_at`, §8/§11 `kb stats`) + the bookkeeping columns
   `pages.created_at`/`pages.updated_at` (see §8). All of these are re-registerable/re-addable
   at worst.
2. **The DB is a rebuildable cache + query accelerator.** It exists so search/browse/link run in
   milliseconds over millions of files. It is never the source of truth.
 3. **No external service, no async runtime.** Embedded SQLite via `rusqlite` (bundled). One
    file. Multiple clients. No server, no API, **no tokio / async**. Chosen over in-process
    async SQLite rewrites (e.g. Turso) specifically because their API is
    `async`-everywhere (`.await` / `std::sync::Mutex` per call) and this project doubles
    as a Rust/Verus teaching aid — sync Rust reads more clearly. Turso's FTS layer is
    also a Tantivy fork with a different SQL surface than SQLite FTS5; that would push
    the design work onto the engine layer rather than the verified `kb-core`.
 4. **Explicit + incremental refresh.** No background watchers by default. Reconcile by
    `(mtime_ns, content_hash)`. `kb refresh` (incremental) / `kb refresh --full` (rebuild).
    Optional `kb cron` paste line.
5. **Search is order-insensitive grammar** — filters + FTS terms + modifiers, commutative.
6. **Provable core, assumed shell.** Verus discharges the deterministic core (incl. query
   semantics and a read-only walk); FFI/IO/CLI are tested, not proven.

## 3. Locked decisions

| Area | Decision |
|---|---|
| Language & tooling | Rust; **Verus** on the pure core; **no async runtime** |
| DB engine | **rusqlite + SQLite FTS5** — embedded, single file, multi-client, no service. (Build needs a C compiler — probe at M0.) |
| DB location | **Default path** `~/.local/share/kb/kb.db` **+ `--db` override** |
| Roots | **Multi-root, arg-driven.** `kb add <deep/path>` registers + indexes (repeatable, many deep roots); `kb roots`; `kb drop-root <path>`; `kb refresh` reconciles all. **No `kb init <root>`.** **Nested/overlapping roots are allowed** — the same `abs_path` under more than one root collapses to one `pages` row (dedupe on refresh; innermost/most-recent root wins for `root`/`rel_path`). |
| Identity | **DB key = the page's absolute path** (`abs_path`, unique by construction). The *display/id* is `type:<slug>` where slug = **frontmatter `title`, else H1, else filename**. Types: `wiki`, `note`, `label` — taken from the **required** `type:` frontmatter field. **A page with no `type:` is skipped** (not indexed; it wasn't designed for this tool) and `kb add` warns. A page needs **no H1** if it has a `title:` field. Display ids may collide across files → every hit shows **abs_path**; `kb resolve` returns a candidate list (no hard error). |
| Tags | **Plain strings** in a `tags: [a, b, …]` frontmatter field on **any** page (`wiki`/`note`/`label`). Collected at indexing time into a page↔tag mapping. Tags are **not pages** and are **not defined by** any other page. |
| Tag aliasing | A **DB-level tag-string→tag-string map** (`tag_aliases` table), managed by `kb tag-alias`, not in frontmatter. Lets different spellings ("Czechia"/"Czech Republic") resolve to the same conceptual tag at query time. |
| Labels | A `type: label` page is a **small proxy note**: it carries `tags:` **and** references the real **files/folder** those tags describe (via a `## files` list, a leading path-list body, or a folder-wide default). Searching a tag finds the label, but the tool **displays the referenced files/folder** (read from the label note on disk), not the label page itself. |
| Non-MD resources | **Not in the DB at all.** Non-md files are never indexed as rows. They are only ever surfaced by expanding a label's file/folder reference (read live from disk). |
| `search_required_phrase` | **Visibility gate.** A page *with* `search_required_phrase` is **hidden from general search**; it appears only when the **entire** entry (an unquoted word or a quoted multi-word phrase, e.g. `"deep learning"`) occurs, as a contiguous span, *somewhere* in the query text (case-insensitive). Pages *without* are always visible (if content matches). **Zero results allowed & informative.** |
| Grammar | `key:value` = filter (incl. `tag:<x>`) · bare = AND · `~term` = OR group · `-term` = NOT · `"phrase"` = phrase. **Order irrelevant.** |
| Frontmatter | Permissive YAML-subset parser: scalars + string lists + `#`. Nested maps out of v1. **`type:` is required to be indexed — a `.md` without it is skipped** (see §4). `type:` is matched **case-insensitively** (`Wiki`/`WIKI` = `wiki`); the **canonical lowercase** form is stored (in the `type` column and the display id). Known optional scalar fields: `description` (≤255 chars, see below), `search_required_phrase` (gate list), `tags` (list). FTS scans title + description + body + frontmatter scalars. |
| Description | `description:` — **optional** one-line summary: a short answer to *"what is actually in this page?"*. **Hard limit ≤255 characters** (violations are warned and the field is dropped; the page still indexes). Stored in `pages.description`, part of the FTS index, and **displayed** in `kb search`/`kb browse` output when present. Limit is enforced at index time (not at render time), so it's never silently truncated in output. |
| Extensions | **`.md` only by default.** Add more via `kb add --ext <ext>` (repeatable) / config. Files with a non-listed extension, or **no** extension, are not indexed. The parser must survive **binary content** and a **missing trailing newline** without corrupting (§9). |
| Walk / skip rules | Walk **does not follow symlinks — permanent** (never overridable; prevents infinite loops over the filesystem). **Dot-directories** (`.git/`, `.obsidian/`, etc.) are **skipped by default**; **dot-files** (`.notes.md`, etc.) **are scanned normally**. `kb add --include-hidden` turns off dot-*directory* skipping only; it does not change the symlink rule. |
| Label file reference | `## files` heading (case-insensitive; list under it) > leading contiguous path-list run > **folder-wide default**. The reference lives in the **note body on disk**, not the DB. |
| Editing surface | **Full `mdtool.py`:** `info`, `section`, `replace`, `set-section`, `append-section`, `insert-section`, `diff`. Dotted ref or title-path addressing; atomic save; `--dry-run`/`--diff`. |
| Packaging | **Single binary, flat `kb <cmd>`**, two logical namespaces, one shared core crate. |
| Verus scope | **Prove** core + query-grammar/semantics + **read-only fs-walk** + section splice. **Assume** rusqlite FFI + SQL execution + real `std::fs` reads + CLI shaping. |

## 4. Page & identity model

The **unique identity of a page is its absolute path** (`abs_path = root + "/" + rel_path`). It is
the DB primary key and what every table joins on. It is unique by construction (a file has one
location). **Nested/overlapping roots** (e.g. `kb add ~/a` then `kb add ~/a/sub`) are allowed:
the same `abs_path` appears under both roots and **dedupes to a single `pages` row** on refresh
(the most recent innermost registration wins for the displayed `root`/`rel_path`).

`type:<slug>` is a **display id** (used for `[[wikilinks]]`, `kb resolve`, human output); it may
collide across files and is resolved back to a specific `abs_path` on demand.

| `type` | display id | slug source (priority) |
|---|---|---|
| `label`  | `label:<slug>`  | frontmatter `title` → H1 → filename |
| `wiki`   | `wiki:<slug>`   | frontmatter `title` → H1 → filename |
| `note`   | `note:<slug>`   | frontmatter `title` → H1 → filename |

**Slug priority:** (1) the `title:` frontmatter field, if present — in which case **no H1 is
required**; (2) else the **H1** heading in the body; (3) else the filename (minus extension). The
chosen slug is stored in `pages.title`/used for the display id. If frontmatter `title` and an H1
both exist and disagree, the **frontmatter `title` wins**.

**`type:` is required to be indexed.** A `.md` file with **no** `type:` frontmatter field is
**skipped** (no `pages` row) — absence of `type:` means the file wasn't authored for this tool.
`kb add`/`kb refresh` report the count of skipped (no-type) files as a warning, not an error.

**Resolution order** (map a `[[wikilink]]` / ref string → a specific `abs_path`):
1. `type:slug` exact — match `type` + slug; if exactly one page, return its `abs_path`
2. any-type exact slug, **if the slug is unique** across the corpus → its `abs_path`
3. explicit path ref (`root/rel/path`) → that `abs_path`
4. unique substring of a slug → its `abs_path`
5. else → return a **candidate list** of `abs_path` (no error)

## 5. Search model

### Grammar (order-insensitive, commutative filters)

- `type:wiki` `language:en` — SQL **filter** on a whitelisted frontmatter column (v1: `type`,
  `language`, exact string on the stored value; other frontmatter fields are searchable via FTS
  but not filterable in v1)
- `apple banana` — FTS **AND**
- `~apple ~orange` — FTS **OR** group
- `-pear` — FTS **NOT**
- `"exact phrase"` — phrase

Examples:
- `type:wiki language:en apple` → `type='wiki' AND language='en' AND fts('apple')`
- `type:wiki apple english` → `type='wiki' AND fts('apple AND english')`
- `type:label ~llm ~mllm -gpt` → `type='label' AND fts((llm OR mllm) AND NOT gpt)`

### `search_required_phrase` gate (the "reverse whitelist")

A page's `search_required_phrase` are **secret unlock phrases** — a marker that the page is a **deeper
topic** and should be *hidden* from general search:

- **`kb search "AI"`** → returns pages that content-match "AI" **and have no `search_required_phrase`**.
  Deep pages stay hidden. Clean surface.
- **`kb search "AI deep learning"`** → unlocks a page whose gate contains the **full phrase**
  `"deep learning"`. **Gate matching is whole-phrase substring, case-insensitive** (an entry
  matches if the entry text occurs *anywhere contiguously* in the query; e.g. a lone word like
  `mTLS` or a quoted phrase like `"deep learning"`). `kb search "AI deep"` alone does **not**
  unlock a page gated on `"deep learning"`; a page gated on the single word `mTLS` **is**
  unlocked by `kb search "mTLS"`.
- **Zero results** → valid signal the query was too strict; the tool says so.

So `search_required_phrase` partitions the corpus into **always-visible** (gateless) and **gated** (opt-in
deep dive). This reduces cross-domain clutter from shared vocabulary.

### SQL shape (rusqlite / FTS5)

```sql
SELECT p.id, p.abs_path, p.root, p.rel_path, p.title, p.description,
        snippet(pages_fts, :col, '<mark>', '</mark>', '…', 12) AS snippet,  -- :col = 0based column index; this SQLite build has NO `-1` "best column" mode (§13a), so M7 picks the index from the strongest per-column bm25 contribution. markup only appears under the MATCH clause below.
        bm25(pages_fts) AS score
FROM pages_fts
JOIN pages p ON p.rowid = pages_fts.rowid
WHERE pages_fts MATCH <fts_query>
  AND <ANDed key=value filters>
   AND ( NOT EXISTS (SELECT 1 FROM page_gates g
                     WHERE g.abs_path = p.abs_path)                      -- gateless
         OR EXISTS (SELECT 1 FROM page_gates g
                    WHERE g.abs_path = p.abs_path
                      AND lower(<lowered_query_text>) LIKE '%' || lower(g.term) || '%') )  -- any FULL gate entry occurs in the query
ORDER BY score
LIMIT <n>;
```

**Gate rule:** matching is against the **FTS portion of the query** (bare terms and quoted
phrases — *not* `key:value` filters), **whole gate entry**, **case-insensitive substring**.
A multi-word entry stored as `deep learning` (quotes stripped at index time) only matches
queries whose FTS terms contain the contiguous span `… deep learning …` —
`"AI deep"` does not; `"AI deep learning"` does. A lone-word entry (`mTLS`) matches any
query whose FTS terms contain `mTLS`.

**FTS index = `title`, `description`, `body`, and flattened frontmatter scalars** (`description` is optional, ≤255 chars, §3; NULL rows are skipped by FTS5 automatically).

**Tag filters (`tag:<x>`) are NOT frontmatter-column filters** — they join `page_tags` (after
alias expansion via `tag_aliases`), keyed on `abs_path`, e.g. `tag:llm` →
`EXISTS (SELECT 1 FROM page_tags pt WHERE pt.abs_path = p.abs_path
 AND pt.tag IN ('llm', <aliases of llm>))`
with **case-insensitive** comparison per §6 (SQLite `IN` is case-sensitive, so compare via
`lower()` or a `LOWER(tag)` expression index).
`kb search tag:llm …` and `kb tag llm` share the same tag expansion, but differ otherwise:
`kb search` still requires FTS content match and applies the `search_required_phrase` gate, and shows
page hits; `kb tag` lists **all** tagged pages (no FTS content match, no gate) and expands
labels to their referenced files.

## 6. Tags and Labels (two separate ideas)

**Tags** — plain strings, on any page:
- Any page (`wiki`/`note`/`label`) may list `tags: [a, b, …]` in frontmatter.
- At index time we collect the `abs_path ↔ tag` pairs into a mapping (see §8 `page_tags`).
- Tags are **opaque strings**. They are *not* pages, *not* rows in `pages`, and *not* defined by
  another note. `kb tag <x>` just means "find every page tagged `x`".
- Aliasable: `kb tag-alias --add "Czechia" --canonical "czech"` means the two strings are
  conceptually the same tag for query purposes.
- Tag matching is **case-insensitive** at query time ("Czech", "czech", "czechia" all normalize
  to canonical `czech`); stored tag strings keep their raw casing. **Alias lookup is
  case-insensitive too** (`kb tag czechia` finds the alias row written `Czechia`).

**Labels** — a `type: label` page is a *small proxy note* for something bigger (a set of files,
or a whole folder):
- It carries the tags it wants to be found by (e.g. `tags: [llm, attention]`) and its body points
  at the real files/folder.
- When you `kb tag llm` and a **label** page matches, the tool **does not display the label page
  as the result** — it reads the label note from disk and displays the **referenced files/folder**.
  The label is a pointer, not the payload.

**How a label names its referenced file(s)/folder** (all three resolved by reading the label
note's body — *not* the DB):
1. **`## files`** ATX heading present (**case-insensitive** match; "## Files" works) → the
   block under it is the file list.
2. A leading **contiguous run of lines** after the title (the H1; or, when the page has no H1 —
   i.e. it carries a `title:` field or is slug-from-filename — immediately after the frontmatter)
   are all relative paths (no prose before them) → that run is the file list. If prose appears
   before paths, or no such run exists → case 3. (Operational rule: A.1b.)
3. Otherwise (prose, no list) → **folder-wide** (the label's own folder is the reference).

Relative paths are resolved against the label note's directory; the folder-wide case resolves to
the label note's own folder, **non-recursive** (files directly in that directory only; use an
explicit `## files` list to name subfolder content). The referenced files/folder are **non-md**;
they are never indexed as DB rows.

## 7. Section editing (full `mdtool.py` port)

Byte-scan for headings (skipping fences) → section tree with byte ranges → **O(1) splice edits**
on the in-memory buffer → **atomic save** (temp file + rename).
Sections addressed by dotted ref or title path.
Commands: `info`, `section`, `replace`, `set-section`, `append-section`, `insert-section`, `diff`.

## 8. Database schema (rusqlite / FTS5)

```sql
CREATE TABLE roots  (
  path TEXT PRIMARY KEY,   -- the registered root path (canonical)
  added_at TEXT,
  extra_exts TEXT,          -- per-root `--ext` add-ons, '\n'-joined (e.g. 'markdown\nmdx'); NULL/'' if none (default = .md only)
  include_hidden INTEGER   -- per-root `--include-hidden`: 0/1; 0 = skip dot-directories (default); symlink rule is global+permanent, not stored here
);

CREATE TABLE pages  (
  abs_path TEXT NOT NULL,       -- canonical absolute path (after `~`/`..` resolution) = UNIQUE page identity (PK)
  id TEXT NOT NULL,             -- display id: label:<slug> | wiki:<slug> | note:<slug> (slug = title|H1|filestem); NOT unique
  type TEXT NOT NULL,
  root TEXT NOT NULL,           -- the winning registered root this was read under (see §4 dedupe: innermost/most-recent)
  rel_path TEXT NOT NULL,       -- path relative to that root
  title TEXT, language TEXT,    -- title = resolved slug/display name (frontmatter `title` > H1 > filename)
  description TEXT,            -- optional, ≤255 chars; short answer to "what is actually in this page?"; NULL if absent
  search_required_phrase TEXT,           -- gate entries verbatim, '\n'-joined; multi-word entries carry their
                               -- quotes, e.g. 'mTLS\n"deep learning"'; nullable if gateless
  frontmatter_text TEXT,        -- flattened scalar values (for FTS + filters)
  body TEXT,
  mtime_ns INTEGER NOT NULL, content_hash TEXT NOT NULL,
  created_at TEXT,             -- time we *first* indexed this `abs_path` (not disk mtime; on rebuild this column is re-initialised)
  updated_at TEXT,             -- time we last indexed (i.e. last `kb add`/`kb refresh` that touched this row); re-initialised on rebuild
  PRIMARY KEY (abs_path)
);
CREATE INDEX idx_pages_id  ON pages(id);         -- display id for `[[wikilink]]`/resolve; NOT a unique constraint (collisions are legal, §4/§13.3)

-- gate entries need per-entry matching (whole-phrase), so a row per entry:
CREATE TABLE page_gates (
  abs_path TEXT NOT NULL,       -- FK → pages.abs_path (the page key)
  term  TEXT NOT NULL,          -- exact entry as written, minus surrounding double quotes
  PRIMARY KEY(abs_path, term)
);

CREATE VIRTUAL TABLE pages_fts USING fts5(
  title, description, body, frontmatter_text,
  content=pages, content_rowid=rowid
);

CREATE INDEX idx_pages_type     ON pages(type);
CREATE INDEX idx_pages_language ON pages(language);
CREATE INDEX idx_pages_root     ON pages(root);

-- page ↔ tag (tags are plain frontmatter strings on any page, collected here), keyed by page path
CREATE TABLE page_tags (
  abs_path TEXT NOT NULL,       -- FK → pages.abs_path
  tag TEXT NOT NULL,             -- the raw tag string as written
  PRIMARY KEY(abs_path, tag)
);
CREATE INDEX idx_page_tags_tag ON page_tags(tag);

-- lightweight runtime bookkeeping (NOT a page-derived table; lost on wipe, re-computable for skip_count by re-walk, NOT re-computable for last_refresh_at)
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,   -- 'last_refresh_at' (ISO-8601) | 'no_type_skip_count' (int as TEXT)
  value TEXT NOT NULL
);

-- tag-string → tag-string aliasing (query-time normalization), DB-level, not in frontmatter
CREATE TABLE tag_aliases (
  alias TEXT PRIMARY KEY, canonical TEXT NOT NULL
);

-- links: endpoints are page abs_paths (or bare display ids pending resolution); root-agnostic
CREATE TABLE links (
  from_path TEXT,               -- FK → pages.abs_path
  to_path TEXT,                 -- FK → pages.abs_path once resolved; may be a pending display-id ref
  style TEXT,                    -- 'wikilink' | 'relative' | 'absolute'
  PRIMARY KEY(from_path, to_path, style)
);
CREATE INDEX idx_links_to ON links(to_path);
```

> **There is no table for non-md files.** Non-md content is never indexed. It only ever surfaces
> by expanding a **label** page's file/folder reference (read live from the note body on disk).

## 9. Verification (Verus: proven vs. assumed)

**Proven in `kb-core` (pure, no IO/async/FFI):**
- `meta` — `NoteType` enum invariants; `NoteMeta` (incl. `tags: Vec<String>`,
  `search_required_phrase: Vec<String>`, `description: Option<String>`) well-formed; **`description` ≤ 255 chars**
  (or `None`)
- `frontmatter` — parse→struct; **`type:` required (else the page is not indexable, §4)**; `type:`
  matched case-insensitively, canonical lowercase stored; `description` length-checked (≤255, else
  dropped + flagged); tag set preserved; gate entries parsed (quotes stripped from
  multi-word entries, lone words kept as-is); normalize idempotent; **binary-safe / no-trailing-newline
  robust** (parser never panics on a file with NUL bytes or no final `\n`)
- `identity` — `abs_path` unique per page; display id `type:<slug>` well-formed (slug =
  `title`→H1→filename per §4); **display-id collisions across files are modeled, not hidden**
  (same slug from two `abs_path`s is legal; resolution returns a candidate list)
- `links` / `graph` — no duplicate edges; style classification correct; backlink = inverse of
  forward **(for resolved edges — a `links.to_path` holding a pending display-id ref, §8, resolves
  to a single `abs_path` before the inverse is taken)**; no self-loops; adjacency maps consistent
- `labels` — label file-reference classification **deterministic**: `## files` / leading
  contiguous path-list run / folder-wide, exactly one wins; referenced paths well-formed
- `tags` — tag-string collection from frontmatter: duplicate-free; trim rule: leading/trailing
  whitespace per item removed, non-empty items only; empty `tags:` list → no rows
- `search_grammar` — token→FTS string well-formed; escaping/quoting correct;
  filter-vs-FTS separation preserved
- `semantics` — **gate = gateless ∨ ∃ whole gate entry occurring in query text (case-insensitive)**; **filter commutativity**
  (= "order doesn't matter"); AND/OR/NOT structure preserved
- `fs_snapshot` (pure walk over an immutable snapshot) — enumeration **sound + complete**;
   **no-mutation postcondition** (= "no unintended writes"); skip rules correct per §3 Walk
   (no symlinks followed — permanent; dot-directories skipped unless `--include-hidden`; dot-files scanned; non-indexed extensions skipped)
- `section` (ported from `mdtool.py`) — re-scan after splice well-formed; splice leaves bytes
  **outside `[lo,hi)` untouched**; tree level-consistent; refs renumber deterministically

**Assumed (tested, not proven):** `rusqlite` FFI + SQL execution; snapshot *builder* (real
`std::fs` reads, documented read-only); atomic-save syscall; CLI output shaping.

> **Honest ceiling:** "Verified SQL" = the query's **semantics** and **construction** are proven;
> the SQLite engine is trusted. "Verified fs-walk" = the walker logic is proven **read-only /
> complete**; the underlying syscalls are the IO boundary.

## 10. Workspace layout

```
knowledgebase/    (Cargo workspace)
├── Cargo.toml                    # workspace manifest
├── rust-toolchain.toml           # pin toolchain
├── README.md                     # what/why, build, verify workflow
├── kb-core/                      # pure, sync, Verus-verified (no std IO deps)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
    │       ├── meta.rs               # NoteType {label,wiki,note}, NoteMeta (tags: Vec<String>, search_required_phrase: Vec<String>, description: Option<String> ≤255, …)
│       ├── frontmatter.rs        # YAML-subset bytes→NoteMeta      [proven]
│       ├── identity.rs           # abs_path (PK) + display id `type:<slug>` (slug=title→H1→filestem); slug→abs_path resolution (§4 order) [proven]
│       ├── links.rs              # extract wikilink/rel/absolute   [proven]
│       ├── graph.rs              # forward/backlink adjacency      [proven]
│       ├── tags.rs               # collect/normalize tag strings   [proven]
│       ├── labels.rs             # label file-ref: ## files / leading path-run / folder-wide [proven]
│       ├── search_grammar.rs     # tokens→FTS string               [proven]
│       ├── semantics.rs          # query predicate + gate model    [proven]
│       ├── fs_snapshot.rs        # pure walk over snapshot         [proven]
│       └── section.rs            # scan+splice (ported from mdtool)[proven]
├── kb-db/                        # assumed-spec (rusqlite + FTS5)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── conn.rs               # open default/override path, migrate (idempotent DDL)
    │       ├── walk_snapshot.rs      # real std::fs walk → immutable snapshot (IO); skip rules per §3 Walk (no symlinks, dot-dir skip w/ --include-hidden, extension allow-list)
    │       ├── index.rs              # upsert pages/page_tags/page_gates/links; skip (warn) `.md` with no `type:` (per §4); tag_aliases is user-managed, not derived from disk
│       ├── refresh.rs            # incremental (mtime_ns,hash) diff + full-rebuild
│       └── query.rs              # SQL build: filters + MATCH + gate; run + shape rows
├── kb/                           # shared CLI source body (NOT a package — see below)
│   ├── main.rs                   # clap dispatch (one real body, `include!`d twice)
│   ├── diff.rs                   # unified-diff renderer + parser
│   ├── tui/                      # M8 (ratatui shell) — gated behind `tui` feature
│   │   ├── mod.rs
│   │   ├── input.rs
│   │   └── render.rs
│   └── testdata/                 # golden fixtures for the diff parser
├── kb-user/                      # package `kb-user` → bin `kb` (full: TUI + editor)
│   ├── Cargo.toml                # default features = ["editor", "tui"]
│   ├── src/main.rs               # one line: `include!("../../kb/main.rs");`
│   └── tests/                    # the CLI integration suites (drive kb)
│       ├── create_cli.rs
│       ├── m7_multifile.rs
│       ├── m9_agent_surface.rs
│       ├── m9_json_fidelity.rs
│       └── section_cli.rs
├── kb-hardened/                  # package `kb-hardened` → bin `kb-hardened` (headless)
│   ├── Cargo.toml                # default features = [] (no TUI, no editor)
│   └── src/main.rs               # one line: `include!("../../kb/main.rs");`
├── .cargo/config.toml            # aliases: `cargo user`, `cargo hardened`
└── tests/
    ├── fixtures/                 # golden deep multi-root MD trees (each type)
    └── integration/              # end-to-end: add/refresh/search/edit on fixtures
```

The two-package design (one shared body, two thin wrappers) lets a hardened
build drop `ratatui`/`crossterm` from the dependency tree entirely — see
`HARDENED_BUILD.md` for the locked requirements, why it can't be done with
`required-features`, and the current gate checklist.

## 11. CLI reference (flat `kb <cmd>`; `--db <path>` defaults to `~/.local/share/kb/kb.db`)

### Bare-string dispatch (ergonomics shortcut)

Any **single non-flag, non-command argument** is handled like:
- if it **resolves to an existing `.md` file path** → `kb info <path>`
- otherwise → `kb search <string>`

This mirrors `mdtool.py` (which defaults a bare filename to `info`).
Multi-token strings containing spaces are always treated as search queries.

### Multi-file (DB-backed)

```
kb add <path> [--ext <ext>...]      # register root + recursively index (repeatable, many deep roots; nested roots allowed — see §3 Roots); skips (warns) `.md` without `type:` (per §4); `--ext` adds extensions to the indexable set (§3 Extensions); `--include-hidden` also walks dot-directories (§3 Walk); symlinks are never followed (permanent)
kb roots                             # list registered roots
kb drop-root <path>                  # unroot — but the page survives if it's still registered under another (nested/overlapping) root; only removes if this was its last root. Note: this *also* drops that root's walk options (extra_exts / include_hidden, §8 `roots`) — re-`kb add` with the same flags to restore
kb refresh [--full]                  # reconcile all roots by (mtime_ns, content_hash); --full = rebuild. Walk options (`--ext`/`--include-hidden`) are read from each root's stored `roots` row (§8) — not re-passed here — so a refreshed root uses the same rules it was added with
kb full-rebuild                      # alias for refresh --full
kb create <name> [--path <dir>] [--type wiki|note|label] [-e] [--dry-run] [--yes] [--json]
    # scaffold a new `.md` page — full frontmatter frame (`type:` `title:` `description:` `tags:` `search_required_phrase:`) + matching H1 — then index it with the *same* `build_page`/`upsert_page` pair `refresh` uses, so the row reconciles byte-identically later. `.md` is appended when `<name>` has no extension; `--path` is the target directory (created if missing) and only a `.md` target is accepted (§3 Extensions); an existing file is never clobbered. `-e/--edit` opens the *written* page in `$VISUAL`/`$EDITOR` (fallback `vi`) and indexes the saved bytes — a non-zero editor exit leaves the scaffold on disk, unindexed. Indexing is skipped **with a stderr note, never an error** when no roots are registered, when the page is outside every registered root, or when the final bytes aren't indexable (the §3/§4 no-`type:` warning contract). `-e --json` is rejected (an editor flow is not JSON — §13p), matching `section edit --json`.
kb search <query> [--json] [--limit N]
kb browse [path]                     # numbered per-page list under [path]: location, title, tags, search_required_phrase, link count; `description` shown when present; label pages expand to their referenced files/folder
kb tag <tag>                         # expand tag (incl. aliasing) → tagged pages; label pages render as their referenced files/folder
kb tag-alias --add <alias> --canonical <tag>     # alias map (Czechia → czech)
kb tag-alias --list
kb links <id|path>                   # forward + backlinks
kb resolve <query>                   # disambiguate → candidate list (no error on collision)
kb stats                             # per-root counts, freshness, db size, last-refresh time, no-type skip count
kb cron                              # paste-ready shell line for scheduled refresh
```

### Single-file (M6 — agent read/write surface, pure byte ops over `Vec<u8>`)

Built and tested (see §13f). The read surface is **scoped + minimal**
(outline, or one section); the write surface takes the **complete new text**
(3 sources) — never a patch. Every mutating op prints a standard `diff -u`,
and takes the shared write flags below.

```
kb toc <file> [--json]
    # outline only: ref / level / title per section; --json = {"file":…,"sections":[…]}

kb show <file> [REF] [--body|--full] [--json] [--etag]
    # no REF → whole file; REF → scoped section (own body by default, --full
    # includes subsections). --etag → just the 16-hex FNV-1a etag of the span.

kb section --file <file> set    REF  --text|--from-file F|--stdin [--keep-heading] <flags>
    # replace the section (full span by default; --keep-heading → body only)

kb section --file <file> append REF --text|--from-file F|--stdin <flags>
    # append at end of the section (before the next sibling heading)

kb section --file <file> insert [--ref REF] --title T [--level N] \
    --placement before|after|child|top|end --text|--from-file F|--stdin <flags>
    # --child (default) needs --ref; top/end are absolute (no --ref)

kb section --file <file> replace REF --text|--from-file F|--stdin <flags>
    # replace the section body, keep its heading (the common "rewrite" case)

kb section --file <file> edit   REF <flags>
    # open the section in $VISUAL/$EDITOR (fallback vi); no-op if unchanged,
    # otherwise diff → confirm → set_section(whole span) → atomic write

kb replace <file> --find X --new Y [--scope REF] [--count N] <flags>
    # substring find/replace (no regex in v1); whole file unless --scope
```

* **Write flags** (all mutating ops): `--diff` · `--dry-run` · `--yes` ·
  `--if-unchanged <etag>`. Every op shows a unified diff before applying.
* **`ref`** = dotted `3.2`, or title path `Installation > Quick Start`, or a
  (case-insensitive) unique exact / substring title (the M4 `resolve`).
* **Concurrency**: `--if-unchanged` re-checks the FNV-1a 64-bit etag (16 hex)
  of the target section's own-body bytes; mismatch → **exit 3** (the agent
  re-reads + retries). A hash guard also aborts if the file changed on disk
  between read and write.
* **Save**: atomic (same-dir temp + `rename`). No `--out` yet — in place only.
* **Exit codes**: `0` ok · `1` generic (bad flags / IO / no text source) ·
  `2` resolve/section error (unknown ref, ambiguous, nothing-to-replace) ·
  `3` etag mismatch.
* **Editor**: `$VISUAL` → `$EDITOR` → `vi`; run over a `$TMPDIR` copy; an
  unmodified buffer is detected as a no-op (no diff, no write).
* **Diff is output, not input**: the `diff::` module also exposes a standard
  unified-diff *parser* + *applier* (pinning: `tests/section_cli.rs` + the
  pre-generated `testdata/standard.diff` round-trips through both). That keeps
  a future `kb patch`-style import available, but the write contract stays
  "send the complete new body".

## 12. Milestones

| ID | Deliverable |
|---|---|
| **M0** ✅ | Workspace skeleton; Verus wired over `kb-core` (**12 proofs verified via `verus` spec file**); CI runs `verus` on `kb-core`; **read-only probes**: rusqlite `bundled` build (SQLite 3.46), FTS5 `bm25/snippet` shape, multi-client WAL — **all confirmed green** (see §13a); **golden fixture tree** at `tests/fixtures/` (Appendix A.1–A.4/A.1b + 5 non-MD label targets) with a structural test in `kb-core/tests/golden_fixtures.rs`. |
| **M1** ✅ | `kb-core`: `meta` (M0) + `frontmatter` + `identity` (**39 further proofs verified via `verus`** — `fm_spec.rs` 19 + `identity_spec.rs` 20) + per-type example pages (`wiki/note/label/no-type`) with a `kb-core/tests/frontmatter_smoke.rs` end-to-end test against the real parser. **All green — see §13b.** |
| **M2** ✅ | `kb-core`: `tags` + `links` + `graph` + `labels` (**65 further proofs verified via `verus`** — `tags_spec.rs` 13 + `links_spec.rs` 21 + `graph_spec.rs` 12 + `labels_spec.rs` 19) + unit-test pinning (13/13/6/12 across the four modules) + **deterministic fuzz corpus** in `kb-core/tests/fuzz_corpus.rs` (seeded LCG, property-based asserts of the spec-proven invariants). **All green — see §13c.** |
| **M3** ✅ | `kb-core`: `search_grammar` + `semantics` + `fs_snapshot` (**81 further proofs verified via `verus`** — `search_grammar_spec.rs` 32 + `semantics_spec.rs` 21 + `fs_snapshot_spec.rs` 28) + unit-test pinning (15/9/7 across the three modules) + **empirical FTS5 render probe** in `kb-db/tests/fts5.rs::canonical_fts_render_means_what_the_grammar_intends` (6 FTS5 semantic assertions on SQLite 3.46.0: AND, OR, NOT, quoted phrase, `tag:` column-prefix). **All green — see §13d.** |
| **M4** ✅ | `kb-core`: `section` — port `mdtool.py` scan / resolve / splice / edit into pure Rust over `Vec<u8>` (**83 further lemmas verified via `verus`** — `section_spec.rs` 83: heading-level partition + 6 level witnesses + level-0/7 boundaries, fence-awareness negation, span non-overlap + document-order, preamble-disjointness, the 3-way unique/ambiguous/not-found outcome partition, the 4-stage resolution waterfall, splice range-accounting + insert/replace degenerates, the 5-way placement partition with child⇒ref and top/end⇒absolute, `count` all-vs-capped partition + zero-match-⇒-error, edit purity) + **25 exec tests** in `section.rs` pinning fence-aware `Doc::new`, dotted / title-path / unique-exact / unique-substring resolve, `splice` purity, and the four edit ops (`replace_in` in-scope & count-capped, `set_section` ± keep-heading, `append_section` blank-line separation, `insert_section` before/after/child/top/end). **All green — see §13e.** |
| **M5** ✅ | `kb-db`: `migrate` (full idempotent schema + 3 FTS5 sync triggers), `walk_snapshot` (real `std::fs` tree walk feeding the pure `kb-core::fs_snapshot` walker), `index` (per-page `build_page`→`upsert_page` over the pure `identity`/`frontmatter`/`tags`/`links` core, 4-way row derivation + FTS via trigger, no-type skip+count, FNV-1a `etag`), and `refresh` (incremental by `(mtime_ns, content_hash)`, full-rebuild, GC of orphaned rows). All over the verified M0–M6b core; **12 unit + 7 integration tests** in `kb-db`, incl. FTS-trigger sync, nested-root innermost-wins dedupe & `drop_root` survival, incremental-skip / change / delete-GC, and full-rebuild wipe-and-rewrite. `./verify.sh` still **317 verified / 0 errors**. **All green — see §13h.** |
| **M6** ✅ | `kb` **single-file namespace** (the agent read/write surface, §11): `toc` + `show` (scoped, `--body`/`--full`/`--json`/`--etag`) + `section set/append/insert/replace/edit` (3-way text source, `--keep-heading`) + `replace` (scoped substring) — all over the pure `kb-core::section` ops, with a **zero-depend standard `diff -u` renderer + parser** (byte-identical to `diff -u`, reverse-applicable by `patch(1)`), **etag (FNV-1a 64) optimistic concurrency**, `--dry-run`/`--yes`/`--diff`, **atomic save** (tmp+rename + hash guard), and an **editor flow** ($VISUAL/$EDITOR→vi, no-op detection). Exit codes 0/1/2/3. **21 integration tests** in `kb/tests/section_cli.rs` (binary-as-subprocess, incl. the `diff -u` body reverse-applied by GNU `patch(1)`) + **9 diff-module unit tests** (incl. the pre-generated `testdata/standard.diff` round-trip through the parser + applier). Multi-file ns remains M7 stubs (clean exit-1). **All green — see §13f.** |
 | **M6b** ✅ | Frontmatter kernel ops + `fm`/`fm.<key>` surface (over M6, no new subcommand): pure byte-level total field rewrites in `kb-core::frontmatter` (`find_fm`/`scalar_value`/`list_values` + `set_scalar`/`append_list_item`/`remove_field`/`remove_list_item`, `FmError` total) + CLI `section set/append/rm --value` + `show fm`/`fm.<key>` (whole-file etag, atomic save). **33 new `fm_tests`** + **13 new `section_cli.rs`** integration tests (incl. the block-list-collapse trailing-newline bug pinned) + **37 further proofs verified via `verus`** (`fm_ops_spec.rs`: frame bounds / body preservation / byte-account / field-classification / key-space partitions). `./verify.sh` **317 verified / 0 errors**. **All green — see §13g.** |
    | **M7** ✅ | `kb` **multi-file namespace** over the M5 `kb-db` index (§11/§13i): `add` (+ `--ext`/`--include-hidden`), `roots`, `drop-root`, `refresh`/`full-rebuild`, `search` (FTS5 + `type:`/`language:` column filters, **alias-expanded `tag:`** filters, `~` OR, `-` NOT, phrases, the `search_required_phrase` *gate* rule, `bm25` ranking via a trigger-maintained `pages_fts`, `--json`/`--limit`), `browse` (`[path]` scoped, label pages expand to their referenced files), `tag` (alias-expanded, label-expanded), `tag-alias --add/--rm/--list`, `links <id|path>` (forward + backlinks), `resolve <query>` (candidate list, no hard error on collision — §4), `info <path>` (full page record), `stats` (per-root counts + db size), `cron` (paste-ready line) — plus the **bare-string dispatch** ergonomics (one non-flag token that is not itself a subcommand → `info` for an existing `.md`, else `search`, preserving any `--db` flag). Zero-depend over the already-verified M0–M6b core; **20 integration tests** in `kb/tests/m7_multifile.rs` (binary-as-subprocess over a throwaway root + `--db`). `cargo test --workspace` **green, 0 failures** (279 total at M7 close). `./verify.sh` **317 verified / 0 errors**. **All green — see §13i.** |
| **M7b** ✅ | **Cross-page link resolution** — the §4 5-stage waterfall moved from a shell-string hack into a pure, *verified* kb-core primitive (`kb-core::resolve`: `Page`, `Resolution`, `resolve(query,pages)`, `split_type_slug`, `path_matches`), so the same corpus-identity logic powers all three verbs that need it (`resolve`, `links` forward-annotation, `links` true-backlink) rather than three different SQL queries that happened to disagree. The `resolve` verb now returns **`type:slug`** display ids (not paths); a bare-slug collision renders a candidate list (no hard error — §4 contract); a type-qualified id disambiguates to a unique page. `cmd_links` now *resolves* each forward edge's raw `to_path` against the corpus (rendering `→ type:slug` for a hit, `(unresolved)` for a dead link), and the **backlink set is computed by inverting resolved edges** — i.e. a page B appears as a backlink of A only when one of A's forward targets *resolves to* B's `abs_path`, not when the raw stored text happens to string-match. **17 further proofs** verified via `verus` (`resolve_spec.rs`: 3-way outcome partition, stage-1 / stage-2 precedence, determinism, candidate-set well-formedness, empty-outcome total). 6 new integration tests in `kb/tests/m7_multifile.rs` (wikilink-backlinks-resolve, resolve-type-qualified-unique, resolve-bare-slug-collision, resolve-bare-slug-unique, resolve-substring-unique-and-ambiguous, resolve-missing-slug-is-empty-no-error). `cargo test --workspace` **green, 0 failures** (297 total). `./verify.sh` **334 verified / 0 errors**. **All green — see §13j.** |
| **M7c** ✅ | **Tag-alias import/export (recovery table)** — the M8 "(optional) export/import `tag_aliases` to a file to recover alias knowledge after a DB wipe" item, landed early as the M7b namespace's one *non-derivable* state pathway (§2: `roots` + `tag_aliases` are the only DB-resident state that does not rebuild from the tree). A new pure, **verified** kb-core module owns the byte↔row codec so the shell only owns the file + the DB: `kb-core::aliases` — `to_text` (export: sort by `(alias,canonical)`, dedup, `alias\tcanonical` per line, empty set → empty string, deterministic over the set not the row order), `parse_line` (split on *first* tab, trim, reject empty fields, `None` = per-line skip not fatal), `line_is_ignorable` (blank / `#`-comment), `from_text` (import: last-wins on a repeated alias **matching the `--add` upsert**, first-occurrence order of survivors preserved). New CLI surface on `tag-alias`: `--export` (to `--file`, or **stdout** so `kb tag-alias --export > backup.tsv` works), `--import` (from `--file`, or **stdin** so `kb tag-alias --import < backup.tsv` works), with the skip count (blank/comment/malformed) reported on import so a partially-typed backup is never silently truncated. **16 new proofs** in `spec/alias_spec.rs` (separator is a real single token distinct from the newline terminator; data line decomposes as `alias ++ TAB ++ canonical ++ NL` with the alias a recoverable *prefix* and the canonical a recoverable *suffix*; alias is a strict prefix of the line so a line never degenerates to a bare alias; the export contains the separator; the per-line partition is clean+disjoint of ignorable vs data vs candidate; well-formed-pair guard; empty-set export is the empty sequence). 4 new integration tests in `kb/tests/m7_multifile.rs` (export-then-**wipe-the-DB**-then-import recovers the map and the alias still *resolves*; export-empty renders to the empty byte string and importing it is a no-op; import is idempotent / no row duplication; import skips malformed + blank + comment lines without losing the good rows). `cargo test --workspace` **green, 0 failures**. `./verify.sh` **350 verified / 0 errors** (14 spec files). **All green — see §13k.** |
| **M8** | **Interactive TUI** — `kb tui`, an in-process ratatui + crossterm surface over the same `Db` / `kb-core` / `kb-db` the CLI uses (single binary; shares the connection, no fork, no IPC). Split into four increments: **M8.0** scaffold + *browse* ✅ (`App` state machine, event loop + raw-mode/alt-screen restore via a `Drop` guard, key bindings ↑↓/jk/Home-End, `/`/`Enter`/`a` reserved-but-reported, a colored status bar, `list_pages` snapshot) — **see §13l**. **M8.1** *read* + *filter* ✅ (open a page by `abs_path` in a scrollable two-pane view — body left, `heading_of` outline right — with `PgUp`/`PgDn`/`g`/`G` scroll; `/` opens a case-insensitive substring filter over `id`/`title`/`rel_path` — **see §13m**). **M8.2** *edit* + *autocomplete* ✅ (`e` opens a page's frontmatter into `Edit` mode over the verified `kb-core::frontmatter` ops — `title` / `description` in-place scalars, `tags` / `search_required_phrase` lists with **tab-autocomplete** (`suggestions_for` substring-filtering the corpus's known tokens) and `x` remove-tail live preview; the render's "pending plan" and the commit both call the *same* `build_ops` so they cannot diverge; commit = on-disk hash-guard → `apply_op` → same `atomic_write` as the CLI → single-page re-index + browse refresh → green `StatusKind::Success`) — **see §13n**. **M8.3** *alias + roots* manager ✅ (`a`/`r` open the `tag_aliases` + `roots` registries; a shared "form vs. list" `Tab` contract — type into the add form (`Enter` upsert / registers) or move the list selection (`x` removes / drops) — reusing the *same* `Db::add_root` / `drop_root` + `ON CONFLICT(alias) DO UPDATE` + `kb-db::refresh` as `cmd_add` / `cmd_tag_alias` / `cmd_drop_root`) — **see §13o**. **M8 complete — every `kb tui` binding (Browse, Read, Edit, alias, roots) lands.** |
 | **M9** ✅ | **Agent-facing surface** — Part A (every TUI feature has a plain-CLI bypass), Part B (**every** CLI verb carries `--json`, with the JSON the *only* thing on stdout), Part C (`--db` global flag, already present — now re-asserted for the write path). **Part A**: browse→`browse`, read→`show`, filter→`search`, edit→`section … fm.<key>`/`replace`, alias→`tag-alias`, roots→`add`/`roots`/`drop-root`, cron→`cron` (all already CLI). **Part B**: new `--json` on `add`, `roots`, `drop-root`, `refresh`, `full-rebuild`, `browse`, `tag`, `tag-alias` (`--list`/`--add`/`--rm`/`--import`; `--export` stays the TSV recovery codec), `links`, `resolve`, `info`, `stats`, `cron`, plus the whole write path (`section …` + `replace` + `section edit` rejected). **All 25 CLI verbs** are now `--json`-able (the only exception is `tui`, which is inherently terminal-only). Hand-rolled JSON (zero serde): list verbs emit `[...]`, record/mutation verbs emit `{...}`, one top-level value, nothing else on stdout; human noise (diffs, "applied" notes) is *routed to stderr* under `--json`. New `PageJson` + `jbool` helpers + a `commit(...)` path that forces non-interactive and emits `{file,changed,dry_run,applied,bytes_before,bytes_after,diff}`. **12 integration tests** in `kb/tests/m9_agent_surface.rs` (binary-as-subprocess; each verb's stdout parsed by `python3 json` for shape + required keys, incl. an empty-`[]`/empty-`{}` fallthrough and `section edit --json` clean-reject). `cargo test --workspace` **green, 0 failures** (356 total at M9 close, up from 344). `./verify.sh` **350 verified / 0 errors** (unchanged — M9 is shell). **All green — see §13p.** |
 | **M9.5** ✅ | **`kb patch` — import an external unified diff** — the last single-file-namespace verb parked in §13f as *"~20 lines of `main.rs`: the parser + applier are already built and pinned"*. `Cmd::Patch` takes a target `FILE` plus a 3-way patch source (exactly one of `--patch-file [PATH|-]`, `--patch-stdin`, `--patch-text` — `allow_hyphen_values` on `--patch-text` so a `--- ...` leading line isn't misread as a flag); the shared write contract (`--dry-run` / `--yes` / `--if-unchanged` / `--json`) is inherited via the same `WriteFlags` + `commit()` as `section` / `replace`. The pure core does the work: `diff::apply_patch` consumes the *exact* standard unified diff output (cross-tested against GNU `patch(1)` on `testdata/standard.diff`; the `parse_unified` / `apply_patch` pair is unchanged this milestone). Result: a standard `diff -u` or `git diff` hunk is *consumed*, not re-derived — the write path closes the loop (every write verb already emits standard `diff -u` via `--diff`; `patch` imports that byte-identically). **8 new integration tests** in `kb/tests/section_cli.rs` (binary-as-subprocess over `testdata/*`: real `diff -u` byte-identical output vs. `section_new.md`, `--patch-stdin` via a pipe, `--patch-text` inline, `--dry-run` no-write, `--json` well-formed uniform envelope, 0-source / 2-source clean-reject, identity patch = "no changes" no-op). `--json` sweep extended from **25** to **26** verbs (all valid JSON). `cargo test --workspace` **green, 0 failures** (368 total, up from 360). **All green — see §13q.** |

## 13. M0 open items to settle (non-blocking for the design)

> **M0 results (2026-09-29, all green):** the build + two read-only probe suites
> both passed — see §13a below. Items 1, 2 and 4 are now **resolved on this
> machine**; the rest remain design decisions for M1+.

1. ~~**`cc`/C-compiler present**~~ — **resolved:** rusqlite `bundled` compiled
   SQLite **3.46.0** from source; `cargo build --workspace` succeeds (no C-link
   step required because `bundled` vendors the C source).
2. ~~**FTS5 `snippet()` arg order / collapse**~~ — **resolved:** the 6-argument
   form + the two gotchas (markup only under `MATCH`; no `-1` "best column")
   are pinned in `kb-db/tests/fts5.rs`. See §8 Search and §13a.
3. **Collisions / overlapping roots** — the `type:<slug>` display id is **not** unique; the same
   display id across two files is legal and keys off `abs_path` (§4). Every hit shows `abs_path`;
   `kb resolve` returns a candidate list instead of hard-erroring. Nested/overlapping roots
   dedupe to the same `abs_path` (§3 Roots). Confirm this is the desired UX.
4. **Multi-client concurrency** — **confirmed working:** `PRAGMA journal_mode=WAL`
   is set on open (`Db::open`); a held read transaction does not block a
   concurrent writer and a fresh read afterward sees the new row
   (`kb-db/tests/fts5.rs :: wal_allows_write_during_read_transaction`).
   Single-writer/WAL is the chosen model.
5. **Bare-string dispatch edge cases** — a string starting with `-` is a flag, not a search term;
   a single token that *is* an existing path uses `info`, everything else uses `search`.
6. **Walk options UX** — `extra_exts` and `include_hidden` are per-root (stored in `roots`, §8),
   and `kb drop-root` *drops* them with the root. Confirm this "flags follow roots" model (vs. a
   global config file) is desired. Symlink rule is global+permanent (not per-root) — confirm.
  7. **Where `tag_aliases` live on disk.** Per the locked model, aliases are a **DB-level
    tag-string→tag-string map** (they do *not* live in note frontmatter). But that means the DB is the
    *only* store. If the DB is wiped, alias knowledge is lost. **Decision: Option B —** the user can run
    `kb tag-alias --export` to dump the alias map to a durable file (or stdout, for
    `kb tag-alias --export > backup.tsv`), and `kb tag-alias --import` to restore it
    into a fresh DB (from `--file` or stdin). **Implemented as M7c** — a portable TSV codec
    (`alias<TAB>canonical` per line; blank / `#`-comment lines allowed; deterministic,
    diff-stable, last-wins on a repeated alias) owned by the pure core (`kb-core::aliases`) and
    verified (`spec/alias_spec.rs`, 16 proofs), rather than a JSON blob, so it is
    human-readable and `diff`-able. (Chosen over A, which accepts silent wipe-loss; over C, which
    splits the source-of-truth across two stores with no query-time upside. B keeps the DB the
    single query-time source and gives the user an explicit, opt-in durable backup knob —
    consistent with the "disk is authoritative, DB is rebuildable" model.) See §13k.

### 13a. M0 results — verified and pinned in code

All M0 verification is reproducible with `./verify.sh` (12 Verus proofs) and
`cargo test -p kb-db --test fts5` (5 SQLite probes).

**Verus / kb-core (exec↔spec):**
- Method: standalone `verus kb-core/spec/meta_spec.rs` (ARIA fork v0.2026.09.06,
  z3 4.16.0), *not* `cargo verus check` — this fork rejects user types declared
  outside a `verus!` block, so all verified logic lives in one self-contained
  spec file using only `&str` / `u8` parameters and empty-body `ensures`
  predicates. (Fork quirks are catalogued in the session log; the load-bearing
  one is that every spec-visible type must either be a primitive or live inside
  the `verus!` block.)
- Result: **`12 verified, 0 errors`** — decomposition (prefix + slug), prefix
  pairwise-distinctness over the 3 kinds, prefix witness, slug recoverability,
  and kind-tag `kinds_disjoint`. These are the *shape* invariants; the M1+
  modules (frontmatter, identity, links, graph, tags, search_grammar,
  fs_snapshot, section) each get their own verified spec file the same way.

**SQLite FTS5 (kb-db):** pinned in `kb-db/tests/fts5.rs`, all 5 pass:
- `bm25(pages_fts)` returns a scalar relevance score per row; combined with
  `ORDER BY rank` (i.e. `order by rank ASC` → best-first) to rank matches.
  `bm25_orders_matches_best_first` locks the ordering.
- `snippet()` in this build (SQLite 3.46) requires **exactly 6 arguments**:
  `(table, col_index_0based, open_tag, close_tag, ellipsis, maxtok)`. **4-arg
  and 5-arg forms are rejected** ("wrong number of arguments"). The first arg
  is the table *identifier* (`snippet(pages_fts, …)`), not a string.
- **Two gotchas confirmed empirically and asserted in the test:**
  1. *Markup only appears under a `MATCH` query.* `WHERE pages_fts.rowid = N`
     (no MATCH) returns the column verbatim and ignores `maxtok`; only
     `WHERE pages_fts MATCH '…'` triggers clip + `<b>…</b>` wrap. This matches
     the real `search → excerpt` flow, so the M7 query generator must always
     pair `snippet` with a `MATCH` clause.
  2. *There is no `-1` "best column" mode* here (it errors). We pick a
     specific column index, or run a per-column query to decide which column
     carries the strongest hit. **Implication for §8:** drop the `-1` in the
     example `snippet(pages_fts, -1, …)` SQL — pick the column from the best
     matching column's `bm25` contribution, or hard-code `title`/`body`
     (whichever has the match) at M7.
- WAL: single-writer / many-reader confirmed, see item 4 above.
- External-content FTS (`content='pages', content_rowid='rowid'`): the index
  is *not* populated from the content table automatically — each `INSERT`/
  `UPDATE` to a page must be paired with the matching `pages_fts` upsert; the
  M5 index/refresh code generates both.

**Workspace (M0 deliverables):** the three-crate layout builds
clean (`cargo build --workspace`), all unit + integration tests pass
(`cargo test --workspace`: kb-core 10, kb-db 5, kb skeleton), and
`verify.sh` reproduces the Verus + drift-check pipeline end-to-end.

### 13b. M1 results — frontmatter + identity verified and pinned in code

All M1 verification is reproducible with `./verify.sh` (now **51 Verus
proofs total**: `meta_spec.rs` 12 + `fm_spec.rs` 19 + `identity_spec.rs` 20,
all `0 errors`) and `cargo test --workspace` (kb-core 42 unit tests + 2 golden
fixtures + 5 frontmatter smoke tests, all pass).

**`kb-core::frontmatter` (exec, `src/frontmatter.rs`, 14 unit tests):**
the permissive YAML-subset parser. Framing (BOM-strip, opening/closing `---`,
no-frontmatter → `Frontmatter::none`), the `type:` gate (case-insensitive,
`Label`/`Wiki`/`Note`; typo → `unknown_type_present`), `description:`
≤255-char *flag* (value retained, `description_too_long: true`, drop applied by
the M5 indexer), `tags:` trimmed + unquoted + deduped (first-occurrence order),
`search_required_phrase:` verbatim (quotes/case/space retained — the gate contract), and
`frontmatter_text` / `body` extraction. Pinned by `spec/fm_spec.rs`.

**`kb-core::identity` (exec, `src/identity.rs`, 18 unit tests):**
- `resolve_slug(title, h1, filename)` — the priority chain
  `title → H1 → filename`, first non-empty-after-trim wins, else `None`.
- `find_h1(body)` — the first heading line; a valid page H1 is level 1
  (`# …`, one `#` then a space) *and is the first heading*; H2+ first → `None`.
- `filename_minus_extension(path)` — strips only the *last* extension; a
  leading dot (dotfile) is preserved.
- `display_id(kind, slug)` = `prefix ++ slug` (the pure re-exposition of
  `meta::NoteMeta::as_display_id`).
- Two full-pipeline tests (`pipeline_a1`, `pipeline_a1b`) drive
  `parse → find_h1 → resolve_slug → display_id` on real A.1 / A.1b-shaped pages.

**Spec files (verified, `kb-core/spec/`):**
- `fm_spec.rs` (19 proofs): kind partition (pairwise disjoint + satisfiable +
  `indexable_iff_known`), description bound (total + disjoint), tag hygiene
  (usable/empty partition), and the `search_required_phrase` unlock predicate (non-empty
  + contiguous prefix/suffix/exact — the hidden→unlocked gate shape).
- `identity_spec.rs` (20 proofs): the slug-priority *decision* as a total
  mutually-exclusive 4-branch choice (title/h1/filename/none) — proving the
  indexer's fallback chain is unambiguous — plus the display-id decomposition
  and slug-suffix-recovery (re-pinned generically for the M5 indexer) and the
  well-formedness (`usable` = non-empty) guard.

**Per-type example pages (`kb-core/examples/frontmatter/`):** `wiki.example.md`,
`note.example.md`, `label.example.md`, `no-type.example.md`. The smoke test
`kb-core/tests/frontmatter_smoke.rs` runs each through the **real** parser and
asserts the full contract: indexability, kind, tag dedupe, verbatim
`search_required_phrase` (`token bucket` / `mTLS` case-and-space exact), and that the
no-type page parses but is `!is_indexable` with `unknown_type_present == false`
(absent, not a typo). Every indexable page is additionally resolved to its
`type:slug` display id through the real identity pipeline.

**Fork-quirk note (M1):** the same ARIA-Verus shape constraints as M0 held.
New load-bearing findings, empirically confirmed this session: (a) `Seq<char>`
`==` / `is_prefix_of` / `is_suffix_of` on a spec-fn *call* plus a string-literal
`@` is the proven form (a lone `s@ == "lit"@` where `s` is a *parameter* `&str`
does not reduce — pass a `Seq<char>`); (b) `Seq !=` (negated equality) and
multi-conjunct `ensures` mixing a call-equality with a prefix claim do not
discharge — split into one-claim lemmas instead; (c) no `str::contains`,
`to_ascii_lowercase`, or `.at(i)` in spec bodies. `verify.sh` now verifies all
three spec files sequentially before the drift-check test run.

### 13c. M2 results — tags / links / graph / labels + fuzz corpus

All M2 verification is reproducible with `./verify.sh` (now **116 Verus
proofs total**: `meta_spec.rs` 12 + `fm_spec.rs` 19 + `identity_spec.rs` 20 +
`tags_spec.rs` 13 + `links_spec.rs` 21 + `graph_spec.rs` 12 + `labels_spec.rs`
19, all `0 errors`) and `cargo test --workspace` (kb-core: 12 meta + 14
frontmatter + 18 identity + 13 tags + 13 links + 6 graph + 12 labels unit
tests + 2 goldens + 5 smoke + 7 fuzz-corpus property drivers, all pass).

**`kb-core::tags` (exec, `src/tags.rs`, 13 unit tests):** the two distinct
operations §6 calls for — (1) *store-time* `normalize` (trim / drop-empty /
dedupe-first-occurrence, case-preserving — `Czechia` stays `Czechia`) and
(2) *query-time* `canonicalize` (case-fold + alias-chain conflate; "Czech" /
"czech" / "czechia" all land on the same canonical). `has_alias` mirror of the
DB `tag_aliases` table. Pinned by `spec/tags_spec.rs`.

**`kb-core::links` (exec, `src/links.rs`, 13 unit tests):** `extract_links`
scans a body for **wikilinks** (`[[ … ]]`, inner text trimmed, no newline
inside) and **path refs** (whitespace-delimited tokens containing `/` or
ending in a known extension), classifying each as `wikilink | relative |
absolute` (absolute ⟺ leading `/`). `dedup` collapses repeated
`(target, style)` pairs (the `links` PK, §9 "no duplicate edges"); first-
occurrence order preserved. `is_pathlike` is the shared heuristic (public,
reused by `labels`). Pinned by `spec/links_spec.rs`.

**`kb-core::graph` (exec, `src/graph.rs`, 6 unit tests):** a resolved-edge
graph over `abs_path`s. `add_edge` drops self-loops and dedupes; `out` /
`back` are `BTreeMap<String, BTreeSet<String>>` (forward / backlink). Pure
helper `inverse(out)` reconstructs the backlink view and drops self-loops
present in the input. `edges()` gives a total sorted view for the M5 indexer.
Pinned by `spec/graph_spec.rs`.

**`kb-core::labels` (exec, `src/labels.rs`, 12 unit tests):** `classify(body)`
is the deterministic 3-way decision (§6 + §9): case 1 (`## files` heading,
**case-insensitive**, block under it collected, prose inside the block
skipped, next heading ends the block) > case 2 (a **leading contiguous path
run** — no prose before it — in the body) > case 3 (folder-wide). Kept refs
are well-formed (`ref_is_well_formed`: non-empty + pathlike) and relative
(`ref_is_relative`: no leading `/` or `~`). `is_heading` handles headings of
any level; `is_files_heading` matches exactly `## files` / `## Files`. Pinned
by `spec/labels_spec.rs`.

**Spec files (verified, `kb-core/spec/`):**
- `tags_spec.rs` (13 proofs): normalize is idempotent, duplicate-free,
  empty-drop is a total+disjoint partition of the input; canonicalize is a
  total conflating decision (case-fold + alias collapse), empty-query ⇒ empty
  match set.
- `links_spec.rs` (21 proofs): style ⟹ target shape (three-way exclusive —
  absolute ⟺ leading `/`, relative ⟺ not-leading-`/`, wikilink ⟹ not-a-path);
  dedup is idempotent and total.
- `graph_spec.rs` (12 proofs): `out(a) ∋ b` ⟺ `in(b) ∋ a` (both directions),
  adjacency views name the same edge set, no self-loops, no duplicate
  neighbours.
- `labels_spec.rs` (19 proofs): the classification is a clean 3-way exclusive
  decision (exactly one of heading / leading-run / folder wins); priority — a
  `## files` heading is the top-priority discriminator (case 1 beats case 2
  when both are present); kept refs are well-formed (non-empty) and relative,
  and the well-formedness guard is a total decision.

**Fuzz corpus (`kb-core/tests/fuzz_corpus.rs`, 7 property drivers):** a
deterministic xorshift32 driver (seed `0x9e37_79b9`, fixed) drives ~5000
random inputs through all four modules and re-asserts the invariants the
specs prove symbolically — `normalize` idempotent / dup-free / never-adds /
drops-empties; `canonicalize` deterministic / subset-of-stored / empty-query
⇒ empty; links style⟹target + `dedup` idempotence (subsequence of input);
graph self-loop drop + inverse both-ways + no duplicate pairs + forward↔
backlink view equality; `labels` deterministic stability + kept refs well-
formed. The seed is a named constant so any regression is one-line
reproducible. This is the "fuzz corpus" the §12 M2 line asks for, done in a
portable way that needs no separate fuzz harness.

**Fork-quirk note (M2):** the ARIA-Verus shape constraints from M0–M1 held.
New load-bearing finding, empirically confirmed in this session: **`&` and
`||` inside an `ensures`-clause *requirement* form (a `requires` on a spec
predicate) discharge**, but **`==` / `!=` on a call-result of a spec
predicate does not** — split each boolean claim into its own one-claim
predicate (see the `labels_spec.rs` P-series: the `heading_present` /
`run_present` / `no_heading_no_run` predicates are each single-conjunction).
The proven primitive set from M1 (`u8` const comparisons, `bool`
combinators, `&str` literal equality, `is_prefix_of` / `is_suffix_of`)
carries over unchanged. `verify.sh` now verifies all seven spec files
sequentially before the drift-check test run.

**M2 deliverables complete** (the §12 M2 line): `kb-core` has `tags`,
`links`, `graph`, `labels` verified + pinned, plus a deterministic fuzz
corpus. Next up: **M3** (`search_grammar` + `semantics` + `fs_snapshot`,
all verified with specs in the same shape).

### 13d. M3 results — search_grammar / semantics / fs_snapshot (verified)

All M3 verification is reproducible with `./verify.sh` (now 10 spec files)
and with `cargo test -p kb-core --lib` (31 unit tests in the three M3
modules) and `cargo test -p kb-db --test fts5` (6 FTS5 semantic
assertions).

**Verus / kb-core (exec↔spec):**

| spec file | proofs |
|---|---|
| `search_grammar_spec.rs` | **32** |
| `semantics_spec.rs`      | **21** |
| `fs_snapshot_spec.rs`    | **28** |
| **M3 subtotal**          | **81** |

Workspace tally after M3: 12 (M0) + 38 (M1) + 65 (M2) + **80 (M3) = 195
verified lemmas** across 10 spec files, 0 errors under `verus` + z3
4.16.0.

**What the 81 lemmas pin down:**

`search_grammar_spec.rs` (32):
- **(F) filter whitelist (§5):** the v1 filter key set is exactly
  `{type, language, tag}` — three-element, pairwise-distinct, total;
  every non-whitelisted frontmatter field is *content* (the filter-vs-FTS
  separation at the token level); a `key:value` token with an empty value
  is *not a filter* (the §5 "filters on `type` with value `apple` only
  when the value is non-empty" guard).
- **(Q) quoting/escaping (§5 "quoting/escaping correct"):** the
  quote-required predicate is total + decisive (a term is exactly one of
  "must quote" or "renders bare"); the empty string is quote-required
  (the degenerate case); a space in a term forces quoting (a bare word
  cannot contain whitespace — FTS5 would split it); an inner `"` is
  doubled in the rendered literal (the FTS5 string-literal rule, pinned
  by `doubles_an_inner_quote`); an unambiguous bare word is rendered
  bare (the other half of the partition).
- **(R) AND/OR/NOT render order (§5 "AND/OR/NOT structure preserved"):**
  the three clause classes {OR-group, NOT, bare} are total + pairwise
  disjoint + distinct (a render clause is exactly one of them, no fourth
  kind — a future refactor that re-orders the clauses is caught); each
  clause kind is reachable (the render is non-degenerate).

`semantics_spec.rs` (21):
- **(G) the gate is a clean disjunction (§5 "gate = gateless ∨ ∃ whole
  gate entry occurring contiguously"):** the two page classes
  {gateless, gated} are total + pairwise disjoint; the entry-predicate
  shape — *inert* on the empty entry and *satisfiable* on a non-empty
  entry that IS a contiguous span — is pinned; a proper sub-span is NOT
  a hit (the "whole gate entry" rule: `"deep learning"` does not unlock
  on `"deep"`); a gateless page is *unconditionally* satisfied; a gated
  page is satisfied iff some entry is a hit.
- **(S) gate-vs-filter lane separation (§5 "matching is against the FTS
  portion of the query, *not* filters"):** a `key:value` filter token is
  in the filter lane, not the content lane; a bare term is the content
  lane, not a filter; for any concrete token exactly one lane owns it
  (the §9 "filter-vs-FTS separation preserved" bullet on the gate's
  input side).
- **(C) filter commutativity (§9 "order doesn't matter"):** the
  two-filter conjunction is invariant under adjacent reordering; the
  one-filter conjunction is invariant under the trivial reorder; the
  empty conjunction is vacuously true (the base case the adjacent swap
  inducts from); commutativity distributes through a deterministic
  `hold(p,k,v)` (the §9 line pinned on a concrete pair).

`fs_snapshot_spec.rs` (27):
- **(K) node-kind partition (§3 "walk rules"):** the three node kinds
  {dir, file, symlink} are total + pairwise disjoint + distinct (the
  walker's branch on kind is total + disjoint — a node triggers exactly
  one skip-rule, never two).
- **(R) the walk rules are a pairwise-exclusive skip partition (§3
  "non-indexed extensions skipped"):** the extension allow-list is a
  clean 2-way partition (an ext is either allowed or not, exactly one
  way); `md` is *always* allowed (the §3 "md only by default" permanent
  default) and the `extra_exts` lane is additive; the skip-unlisted-ext
  and indexed outcomes are XOR (no third outcome); a `.md` file is
  indexed; a symlink is skipped.
- **(D) the dot-directory rule is independent of the extension rule:**
  a symlink is skipped regardless of `include_hidden` (the §3 "not
  overridable" permanent rule); a dot-dir is skipped when
  `!include_hidden` and walked when `include_hidden` (the §3
  "dot-directories skipped unless `--include-hidden`" bullet); a non-dot
  dir is never skipped by the dot-rule; a dot-*file* (e.g. a file named
  `.notes.md`) is scanned normally by the extension rule (the §3
  "dot-files scanned normally" bullet); the extension rule is
  *independent* of the `include_hidden` toggle (the two rules are
  orthogonal: one is a directory rule, one is an ext rule).
- **(N) no-mutation / determinism (§9 "no-mutation postcondition"):**
  the enumeration predicate is pure in its inputs (same
  `(snapshot-id, options)` ⇒ same membership answer — the §9 "no
  unintended writes" half pinned as a pure function); the enumeration
  answer is total (the membership is a boolean, not partial); the walker
  is read-only (the walker reads the snapshot, does not write to it).

**Exec / kb-core tests (drift-check):**

| module | `#[test]` |
|---|---|
| `search_grammar.rs` | **15** |
| `semantics.rs`      | **9** |
| `fs_snapshot.rs`    | **7** |
| **M3 subtotal**     | **31** |

`cargo test -p kb-core --lib`: **117 passed** (86 pre-M3 + 31 M3 unit
tests, 0 failed).

**Empirical FTS5 probe — `kb-db/tests/fts5.rs` (6 assertions, all green):**

`canonical_fts_render_means_what_the_grammar_intends` — seeds a known
9-row FTS5 table on the bundled SQLite 3.46.0 and asserts each
`kb-core::search_grammar::parse(query).render_fts()` string returns the
*intended row set*:
1. two bare AND terms → both (A);
2. `(a OR b …)` → the OR-union (D ∪ E);
3. `NOT` excludes a row (B excluded → A only);
4. `(llm OR mllm) NOT gpt attention` → E (the canonical render form);
5. quoted phrase `"an exact phrase"` → the exact row (G);
6. `tag:llm` and `tag:llm attention` → the column-prefix matches (the
   filter-vs-FTS separation: `tag:llm` is in the *filter* lane and is
   *not* rendered as FTS content; only the content terms are
   FTS-matched).

**Load-bearing lesson (catalogued from M0–M3, repeated for the M7 index
author):** the *render form* is **not** the *semantic intent* at the
FTS5 surface. The render form is the *string* that FTS5 parses, and the
FTS5 parser is the authoritative semantic. The canonical form
`[OR group] → [NOT terms] → [bare AND terms]` (space-joined implicit
ANDs, `NOT` reads as infix-after-prefix, `""` is the no-term literal) is
**pinned by the empirical probe, not by re-derivation** — every future
change to `render_fts` MUST re-run the probe against the same
seed table, or the *shape* (the §9 "AND/OR/NOT structure preserved"
bullet) drifts from the *semantics* (the §5 "AND/OR/NOT structure
preserved" contract). The fork quirks are unchanged from M0–M2: every
spec-visible type must be a primitive, one-claim-per-lemma, `&str` +
`u8` + `bool` only in the proven set.

**M3 deliverables complete** (the §12 M3 line): `kb-core` has
`search_grammar` + `semantics` + `fs_snapshot` verified (81 proofs) +
pinned (31 unit tests) + probe-tested (6 FTS5 semantic assertions). Next
up: **M4** (`section` — port `mdtool.py` scan + splice logic; verify
the scan + splice invariants in the same shape).

`tag_aliases` decision (PLAN §13.7 item 7): **B** — `kb tag-alias
--export <path>` / `--import <path>` to a durable file (user-managed,
opt-in), keeping the DB the single query-time source. *(Implemented as
M7c — see §13k; landed as a portable TSV codec, not a JSON blob, in the
pure core + a verified spec, before M8.)*

### 13e. M4 results — `section` (port of `mdtool.py`) verified and pinned

Reproducible with `./verify.sh` (now 11 spec files, `section_spec.rs`
added) and `cargo test -p kb-core --lib section::` (25 unit tests).

**Verus / kb-core (exec↔spec):** `section_spec.rs` — **83 verified
items, 0 errors** under `verus` + z3 4.16.0. Workspace tally after
M4: 195 pre-M4 + **83 (M4) = 278 verified items** across 11 spec files,
0 errors.

**What the 83 lemmas pin down (`section_spec.rs`):**
- **(H) heading detection (§4):** the ATX level range 1..=6 is a
  clean total + disjoint partition (`level0_not_legal`,
  `level7_not_legal`); all six levels are legal witnesses; a heading
  title is non-empty after trimming (via `Seq<char>::len() == 0`, the
  proven set); a heading line is `#`-prefixed by a legal run and never
  by a non-`#` line.
- **(F) fence-awareness (the scan's soundness core):** a heading line is
  a clean function of (is-heading-text, in-fence) with in-fence
  *negating* it — never inside a fence; the fence state is a clean
  2-way partition; a fence-open line (3+ `` ` ``/`~`) is a disjoint
  class from a `#` heading (the leading-character branch is unique).
- **(S) section spans (§4):** two emitted spans are non-overlapping
  (a_end ≤ b_start) and strictly document-ordered (prev_end ≤
  next_start ≤ next_end); every buffer offset is either in the
  preamble (before the first section) or exactly inside one section
  span (preamble and section are disjoint + total).
- **(R) reference resolution (§4):** the outcome is a clean 3-way
  partition {unique | ambiguous | not-found} — total + pairwise
  disjoint (a reference never resolves to two outcomes); the three
  outcome tags are pairwise distinct; the 4-stage waterfall
  (dotted → title-path → unique-exact → unique-substring) is a total
  4-way with distinct, ordered stages; exact-match is symmetric and
  exact ⇒ substring (the weaker fallback stage).
- **(P) splice = pure byte-range rebuild (§4):** the range
  0 ≤ lo ≤ hi ≤ n is pinned (lo ≤ hi, hi ≤ n, lo ≥ 0); the byte-account
  `n − (hi−lo) + ins` is exact for both the insert case (lo==hi ⇒
  grows by exactly ins) and the delete case (ins==0 ⇒ shrinks by
  exactly hi−lo); the insert (lo==hi) and replace (lo<hi) cases are a
  clean disjoint partition.
- **(G) placement (§4):** the five placements
  {before, after, child, top, end} are pairwise distinct and a total
  5-way; **child ⇒ requires a reference** (legal only with a target,
  rejected without); **top/end are absolute** (reject a reference — the
  position is owned by the placement, not the target); the
  relative-vs-absolute split is a clean total + disjoint 2-way
  partition covering all five; the level
  inference clamp stays in 1..=6 (`child_of_level6_stays_level6` — a
  level-6 parent yields a level-6 child, never 7).
- **(C) replacement count semantics (§4):** count is a clean
  all-vs-capped 2-way partition (count==0 ⇒ all, count>0 ⇒ capped);
  n is non-negative and bounded by the cap; **zero-match
  ⇒ NotFound** ("nothing to replace") — a replace that finds nothing
  is an error, never a silent no-op; positive match ⇒ ok.
- **(T) edit determinism / purity (§9 "no-mutation postcondition"):**
  the edit outcome is a total, single-valued function of its inputs —
  two invocations over the same inputs give the same result (the edit
  is a pure function of `(data, ref, text, …)`, no I/O, so repeat =
  repeat); an edit is either a success or a *named* failure
  (NotFound / Ambiguous / NoSection) — never a silent partial write.

**What the 25 exec tests pin (`section.rs`):** fence-aware `Doc::new`
(headings inside ``` ` `` / `~~~` fences are sections **and**
non-headings outside them — close-fence-needs-same-char-and-length);
dotted-number resolve; the title-path cascade; unique-exact;
unique-substring; empty-ref and ambiguity (the 3-way outcome on the
exec side); `splice` purity; and the four edit ops —
`replace_in` (whole-file vs in-scope; count-capped;
no-match ⇒ `SectionError::NotFound`), `set_section` (default vs
keep-heading), `append_section` (before next heading + blank-line
separation), `insert_section` (before ⇒ target.start, after/child ⇒
target.end, top ⇒ preamble_end, end ⇒ data.len; inferred child level).

**M4 deliverables complete** (the §12 M4 line): `kb-core` has
`section` verified (83 items) + pinned (25 unit tests) +
reference-faithful (ported from `mdtool.py` §replace/set_section/
append_section/insert_section). The exec↔spec shape
(fence-awareness, span ordering, 3-way resolve, splice
accounting, 5-way placement, count semantics) is closed. M5
(`kb-db::conn`/`migrate`, `walk_snapshot`, `index`,
`refresh` — incremental + full) is deferred; M6 (the single-file
CLI surface) landed next because it has no external dependency.

---

### 13f. M6 results — the `kb` single-file agent CLI (built + tested)

**Contract (locked with the user).** The read surface is *scoped
and minimal* (an outline, or one section — never the whole file, so
an agent's context stays small). The write surface takes the
*complete new text* (three sources: `--text`, `--from-file F`,
`--stdin`) — never a patch. **Diff is output, not input**: every
mutating op prints a standard `diff -u` for the agent to review, but
the write contract is "send the complete new body" (agent-generated
patches are fragile to parse). Concurrency is handled by an etag:
`--if-unchanged <etag>` is FNV-1a 64-bit (16 lowercase hex chars)
over the target section's own-body bytes; on mismatch the command
exits 3 with "expected … got …" so the agent can re-read and retry.

**Why the diff is standard `diff -u`, not our own shape.** The user
and I locked this: agents (and `patch(1)` / LLMs) expect the exact
`--- label` / `+++ label` / `@@ -a,b +c,d @@` / ` /-/+` shape. Our
renderer is **byte-identical** to the output of the real `diff -u`
CLI on equivalent inputs (verified by hand: strip the `---`/`+++`
label lines from both, `diff` the bodies → empty), and **reverse-
applicable** with GNU `patch(1)` (verified two ways: manually by
feeding our emitted diff to `patch` over a real edit and diffing the
result, and by the `diff.rs` unit test `roundtrip_our_diff_is_
parseable_by_our_applier` which feeds our *own* output back through
the parser + applier and asserts it equals the target).

**Empty-side range form** (the subtle case): GNU's `diff` prints
"empty side" positions as **line number + `,0`** — e.g.
`@@ -0,0 +1 @@` (insert at start of an empty file) or `@@ -1,2 +1,3 @@`
(insert a line into a 2-line file, empty side has 2 lines of
context). Confirmed by running real `diff` on both cases
(experiment in §13f note). Our renderer emits the same shape (see
`head()` + `used_old_range()` in `diff.rs`). Our *parser* accepts
both `N,M` and the bare `N` (= 1-line) forms, so it reads
`git diff` (which uses only bare `N` for single-line ranges)
too.

**Files landed.**

- `kb/src/diff.rs` (~520 lines): LCS-based unified-diff *renderer*
  (line-granular, 3-line context, byte-identical to `diff -u`)
  + a standard unified-diff *parser* (`parse_unified`) +
  *applier* (`apply_patch`, non-fuzzy: lines out-of-order or
  out-of-range fail). Zero runtime deps — keeps `kb` a single
  static binary with no `diff`/`git` fork-exec. **9 unit tests**,
  including:
  - `applies_the_pregenerated_patch_to_get_the_new_file`:
    parse + apply the **real `diff -u` output** (checked in at
    `kb/testdata/standard.diff`) over `kb/testdata/section_old.md`,
    byte-identical to `kb/testdata/section_new.md`.
  - `parses_the_pregenerated_standard_patch`: hunk count, `+`/`-`
    counts, and the empty-side `old_start` / `old_count` (i.e.
    the `@@ -12,4 +12,5 @@` header) are all honoured.
  - `roundtrip_our_diff_is_parseable_by_our_applier`: `apply(x,
    unified_diff(x,y)) == y` — the renderer and the parser agree
    on the same byte stream.
- `kb/Cargo.toml`: no new dep (kept zero-dep binary).
- `kb/src/main.rs` (~700 lines): full M6 CLI surface — `toc`,
  `show`, `section set/append/insert/replace/edit`, `replace`;
  shared `WriteFlags` (dry-run / yes / if-unchanged / diff);
  shared `TextSource` (text / from-file / stdin); typed exit codes
  via the `Kb = Result<i32, (String, i32)>` alias with a `?`-able
  fallible-step pipeline (each command unwinds to the right code —
  section/resolve→2, etag→3, generic→1). Atomic write
  (same-directory tmp + `rename`). Confirm prompt (TTY-aware:
  non-tty stdin does not auto-approve). Editor flow: $VISUAL →
  $EDITOR → `vi` over a `$TMPDIR` file, no-op if unchanged, hash
  guard (re-read the target before writing; abort if the bytes
  differ). Etag check happens **before** any other work in the
  command (so a stale-guard abort is free, no side effects).
- `kb/tests/section_cli.rs`: **21 integration tests** driving the
  real binary as a subprocess. Covers: outline shape + JSON
  envelope, scoped `show` (minimal context; `--full` pulls in
  subsections), etag is 16-hex, unknown ref exits 2, all 5 section
  verbs set/append/insert/replace/edit happy-path + a no-source
  error exit 1, `replace` with a count report on stderr and
  not-found ⇒ exit 2 (and the file untouched), dry-run shows the
  diff but does not write, `--if-unchanged` matching (proceeds)
  and stale (exit 3), stdin / from-file text sources, editor flow
  via a script that appends a line (edit works) and a no-op
  script (no write, no diff), **the emitted diff body reverse-
  applied by GNU `patch(1)` reproducing the binary's actual write
  byte-for-byte**, and the multi-file namespace (M7) stubs
  exiting 1 cleanly.

**Fixture files** (`kb/testdata/`): `section_old.md`,
`section_new.md` (a 2-hunk, realistic edit), and
`standard.diff` (**pre-generated by the real `diff -u` CLI**,
not by our module — this is the key property that proves our
parser is compatible with GNU `diff` / `git diff` output, not
just self-consistent).

**Result.** `cargo build --workspace` clean (0 warnings).
`cargo test --workspace` **191 passed / 0 failed**. The pure
M4 core is untouched; the M6 shell binds to it via the
existing public API (`kb_core::section::{Doc, splice,
SectionPlacement}`, `Doc::set_section`, `Doc::append_section`,
`Doc::insert_section`, `Doc::replace_in`, `Doc::own_bytes`,
`Doc::full_bytes`). Diff is byte-identical to `diff -u` and
reverse-applicable by `patch(1)` on the test fixtures.

**Explicit non-goals** (parked for M7/M8):
- DB-backed commands (add / drop-root / refresh / search / browse /
  tag / tag-alias / link / info / stats) — declared in the CLI,
  exit 1 with a "M5/M7" message. M5 (the `kb-db` indexer) is the
  actual blocker.
- `--out` (write elsewhere than the source). M6 writes in place
  only; the atomic save already isolates a crash mid-write.
- `--regex` on `replace` (substring-only in v1; the M4 core has
  no regex dep). A future `search` (Fuzzy) op can bring regex
  in at the *query* layer without touching the *write* core.
- `kb patch` (import an external unified diff). The parser +
  applier are already built and pinned — a command to surface
  them is ~20 lines of `main.rs`.
- TUI (`kb-tui` over the shared core) — M8, deferred.

### 13g. M6b results — frontmatter kernel ops + `fm`/`fm.<key>` CLI surface

**Scope.** The M6 read/write verbs addressed only markdown *sections*. M6b
adds the *frontmatter* as a first-class edit target, addressed through the
**same `section` / `show` surface** via `fm` (whole frame) / `fm.<key>`
(one field) refs — no new subcommand. The pure core is a set of
byte-level, total (no-panic), no-IO field rewrites in
`kb-core::frontmatter`:

- read: `find_fm` → `Option<Fm { open, close }>`, `scalar_value` →
  `Option<String>`, `list_values` → `Option<Vec<String>>`;
- write: `set_scalar`, `append_list_item`, `remove_field`,
  `remove_list_item`, each `&[u8] → Result<Vec<u8>, FmError>`.

`FmError` is a named, *total* outcome (never a panic / silent partial
write): `NoFrontmatter` / `NotScalar` / `IsList` / `Missing` / `IsScalar`
/ `ValueAbsent`. The CLI maps that to the existing exit-code contract
(`NoFrontmatter`/`NotFound`-class → 2, etag mismatch → 3, generic → 1).

**Kernel invariants (proved, `kb-core/spec/fm_ops_spec.rs`, 37 lemmas).**
Same minimal shape as `fm_spec.rs` / `section_spec.rs` — empty-body, one
claim per lemma, `&str`/`u8`/`bool`/`Seq<char>` only:

- (F) **frame bounds** — `0 <= open < close <= N`; framing is a clean
  2-way (present | absent) so an `fm` / `fm.<key>` read is *in-range*
  (the no-panic contract);
- (B) **body preservation** — every op rewrites only `[..close)`; the
  body `[close..N)` byte-length is a shared lower bound the op never
  touches (frame+body partition the doc; a frame rewrite changes only the
  total by the frame delta);
- (C) **byte-account / purity** — an op is a total function on its inputs;
  a splice over `[lo,hi)` changes the length by exactly
  (inserted − deleted); outcome is `ok` | named-`FmError` (never both);
- (K) **field classification** — scalar | list is a clean 2-way partition
  (so `fm.title` vs `fm.tags` addressing is unambiguous); scalar-present vs
  Missing, list-nonempty vs ValueAbsent, are total + disjoint boundary
  predicates;
- (S) **key space** — known scalar keys {type,title,description} and list
  keys {tags,search_required_phrase} are each a clean membership set and *mutually
  disjoint* (a key routes to exactly one helper); `fm`-ref vs `fm.<k>`-ref
  is total + disjoint.

**CLI wiring (`kb/src/main.rs`).** `set`/`append` gained `--value`; a
`rm` verb was added to `section`. `cmd_section` dispatches `fm` /
`fm.<key>` refs to the kernel ops and `cmd_show` to a `cmd_show_fm`
reader. For `fm` ops the etag guard is the **whole-file** FNV-1a
(stricter than the section-scoped etag, simpler for a 1-frame write);
`check_etag_whole` + the existing atomic save / confirm / dry-run all
reuse unchanged.

**Tests.** `kb-core::frontmatter::fm_tests` grew 33 cases (find/read/set/
append/remove, non-UTF-8 no-panic, body preservation, `join_lines`/
`lines_of` inverse). `kb/tests/section_cli.rs` gained **13** integration
tests over the real binary: `show fm`, `show fm.<key>` (scalar / list /
absent-key-empty / no-frame exit-2), `section set/append/rm` on `fm` refs
(with `--yes`), absent-item ⇒ exit 2 + file-untouched, no-frame ops ⇒
exit 2, stale `--if-unchanged` ⇒ exit 3 + file-untouched, and `--dry-run`
shows the diff but writes nothing. One bug pinned by test: collapsing a
block list to `key: [a, b]` dropped its trailing newline (next field fused
onto the rewritten line) — fixed in `list_rewrite_vals` (`{nl}` spliced),
covered by `remove_list_item_block_preserves_newline` /
`..._followed_by_field`.

**Result.** `cargo build --workspace` clean (0 warnings).
`cargo test --workspace` **green, 0 failures** (kb-core 175 incl. the new
fm kernel tests; kb CLI tests incl. the 13 new fm cases). Full
`./verify.sh`: **12 spec files, 317 verified items, 0 errors** (was
290; `fm_ops_spec.rs` +37). The pure M6 section surface and the M0–M4 core
are untouched.

**Explicit non-goals** (parked for M7/M8):
- Frontmatter *create* (a file with no frame) — currently `NoFrontmatter`
  ⇒ exit 2; a `kb section set fm.x` that synthesises a frame from scratch is
  a small M7 add once the multi-file namespace owns page lifecycle.
- Nested maps / multi-word *scalar* values (v1 frontmatter stays
  scalar + list-only, per Appendix A).
- DB indexing of edited frontmatter — the on-disk bytes are the
  source of truth; the M5 indexer reads them on the next `refresh`.

### 13h. M5 results — the `kb-db` indexer (built + tested)

**Scope.** M5 turns the `kb-db` crate from the M0 `Db` shell + FTS5 probes
into the *DB-backed index layer* that M7 (the multi-file namespace) builds
on. It is 4 modules over the already-verified M0–M6b pure core — no new
Verus surface, the pure logic that used to live here is now the
`kb-core::frontmatter`/`identity`/`tags`/`links`/`fs_snapshot` functions we
already proved:

- **`migrate`** (`kb-db/src/migrate.rs`) — the full §8 schema
  (`roots`/`pages`/`page_tags`/`page_gates`/`links`/`tag_aliases`/`meta` +
  `pages_fts`) with an idempotent `migrate(conn)`, `SCHEMA_VERSION`, and —
  per §13a — the **3 FTS5 sync triggers**
  (`pages_ai`/`pages_au`/`pages_ad`) so the indexer only ever writes
  `pages` and the external-content `pages_fts` stays in lockstep.
  Idempotency is unit-tested; `SCHEMA_VERSION` is a `meta` row, not a
  comment.
- **`walk_snapshot`** (`kb-db/src/walk_snapshot.rs`) — the *impure half* of
  the M3 §10 `fs_snapshot`: a real `std::fs` recursive walk that records
  every node + its `children` into the pure `kb-core::fs_snapshot::Walk`,
  which `enumerate(Options)` then re-discovers. `indexed_files` applies the
  allow-list (`{md,markdown,txt}` + `--ext`) + `include_hidden` rules;
  `is_under` / `winning_root` implement the §4 dedupe
  (innermost = longest root path; ties → most-recently-registered `seq`).
- **`index`** (`kb-db/src/index.rs`) — per-page derivation
  (`build_page` → `PageRecord`): `parse(text)` → `resolve_slug`/`display_id`
  → `tags::normalize` + gate terms (quotes stripped) →
  `extract_links` + `LinkStyle::as_str()` (raw written target, resolved at
  M7 `kb resolve`) → FNV-1a `etag_of`. `upsert_page` is a single transaction
  (DELETE + INSERT `pages`, then the 3 row tables); `delete_page` mirrors it.
  `upsert_or_skip` + `IndexOutcome` surface the skip-no-type count M7 shows.
  `is_indexable` (no `type:`) is honoured: such files are *not* indexed but
  *are* counted.
- **`refresh`** (`kb-db/src/refresh.rs`) — the §4 reconciliation loop.
  `refresh` (incremental): walk all roots, for each candidate decide
  unchanged-by-`(mtime_ns, content_hash)` → skip, else upsert; then
  `gc_orphaned_pages` for any `pages` row no registered root covers.
  `full_rebuild`: DELETE all `pages` + row tables (triggers fire per-row),
  then re-index from scratch. `RefreshSummary` (`{indexed, skipped_no_type,
  gc}`) is what the M7 `refresh`/`full` verbs print.

**Design notes pinned by test.**
- **FTS is trigger-maintained, never hand-upserted** —
  `m5_integration::index_derives_all_row_tables_and_fts` asserts a
  `MATCH 'attention'` over `pages_fts` is satisfied purely by the `pages`
  INSERT (the trigger did the index work), and that `page_tags` (2),
  `page_gates` (quoted `"token bucket"` stored as `token bucket`), and
  `links` (`relative` + `wikilink`) each derive 1:1 from one page.
- **Dedupe is by `abs_path`, innermost root wins** —
  `nested_roots_dedupe_to_one_row_innermost_wins`: a file under
  `outer` *and* `outer/sub` is one page, `root=outer/sub`,
  `rel_path=shared.md`.
- **`drop_root` keeps pages that survive under another root** —
  `drop_root_surviving_under_another_root_keeps_the_page`: drop the inner
  first → `gc=0`; drop the outer too → `gc=1`, row gone. This is the §4
  invariant that a `roots` row is a *registration*, not a page owner.
- **Incremental is cheap and correct** —
  `refresh_is_incremental_and_gc_reclaims_deletes`: unchanged re-run ⇒
  `indexed=0`; content change ⇒ `indexed=1` + new `description` read back;
  file delete ⇒ `gc=1`, row + its rows gone.
- **Full rebuild is a clean rewrite** —
  `full_rebuild_wipes_and_rewrites`: a file deleted *without* a refresh is
  gone after rebuild, the survivor re-indexed exactly once.
- **Walk surfaces only indexable files** —
  `walk_snapshot_surfaces_only_indexable_files`: a `.git/` subdir and a
  `.txt` file are both excluded by the default `include_hidden=false` +
  allow-list; `.git/hidden.md` and `plain.txt` do not appear; `keep.md` does.
  `build_page` on it yields `page_type=note`, `id=note:Keep`.

**Result.** `cargo build --workspace` clean (0 new warnings).
`cargo test --workspace` **green, 0 failures** (kb-core 175 unchanged; kb
CLI unchanged; kb-db **12 unit** in `lib` — idempotency, `build_page`
happy/skip, `winning_root` 2-way, `full_rebuild`/`incremental`/`gc` unit
loops — + **7 integration** in `tests/m5_integration.rs`). Full
`./verify.sh`: **12 spec files, 317 verified items, 0 errors** (M5 is a
pure-`kb-core` consumer; no new Verus surface, so the count is unchanged —
see the M0–M6b sections).

**Explicit non-goals** (parked for M7):
- **Multi-file CLI verbs** (`add`/`roots`/`refresh`/`search`/`tag`/
  `tag-alias`/`links`/`browse`/`resolve`/`stats`/`cron`) and the
  *resolution* of `links.to_path` to `abs_path` (M7 `kb resolve`): M5
  stores the *raw written target* so the resolver can decide.
- **`tag_aliases` writes**: the table exists (migration); the CLI surface
  to manage + apply aliases is M7.
- **Watch / watch-rebuild loop**: `refresh` is on-demand; an inotify/fanout
  cron runner is M7 (`kb cron`).
- **`search` ranking**: `pages_fts` + `bm25` are wired; the
  `search_grammar`→FTS5 render + the §4 multi-root search merge is M7.

### 13i. M7 results — the `kb` multi-file namespace CLI (built + tested)

**Scope.** M7 binds the M5 `kb-db` index to the CLI — the multi-file read
surface of §11. It is pure `kb/src/main.rs` shell code over the already-migrated
schema and the already-verified `kb-core` (search grammar, tag canonicalization,
label classification) — no new Verus surface, so the verify.sh count is unchanged.
Each verb is thin: open the DB, run the query, render stdout. The DB default is
`~/.local/share/knowledgebase/kb.db` (dirs crate), overridable per-call by the
global `--db PATH` flag — which also makes the whole namespace testable in
isolation (throwaway DB, throwaway root).

**The verbs** (`kb/src/main.rs`, ~1860 lines):
- **root lifecycle** — `add <path> [--ext ...] [--include-hidden]`
  registers the root in `roots` then runs `refresh`; `roots` lists registrations
  (with their stored walk options, most-recent first); `drop-root <path>`
  unregisters, leaving a GC of the now-orphaned pages to the refresh that
  follows (M5 `gc_orphaned_pages`). All three print exactly one line: what
  happened and the count.
- **reindex** — `refresh [--full]` walks every registered root, diffs each
  candidate against `(mtime_ns, content_hash)` and upserts the changed ones,
  then GCs orphans; `full-rebuild` is the `refresh --full` alias. Both print
  the M5 `RefreshSummary` line (`N root(s) · M page(s) indexed · S skipped
  (no-type:no-slug) · G GC'd`). `add` = `insert root` + `refresh` (the M5
  non-goal "M7 add" closed this way).
- **search** — the §5/§8 query surface, one argument. A `search_grammar::parse`
  split (AND / `~` OR-group / `-` NOT / `key:value` filters) drives:
  - an FTS5 `MATCH` over the trigger-maintained `pages_fts` (via the M3
    `render_fts` — bare AND, OR-group, trailing NOT, quoted phrases),
    `bm25(pages_fts)`-ranked,
  - `type:` / `language:` **column filters** (case-insensitive equality on
    `pages.<key>`),
  - **`tag:` filters** (alias-expanded through `tag_aliases` + the corpus,
    `canonicalize` from the M2 core, case-insensitive IN-list over
    `page_tags`), and
  - the **`search_required_phrase` gate rule** (a page with gate terms is hidden unless
    the rendered FTS text contains the full entry — the §8 "gated" behavior,
    computed on the *rendered query* not the raw word set).
  A **documented v1 limitation** pinned by test: a NOT-only query (`-beta`)
  has no leading AND/OR term, so FTS5's "no leading NOT" rule means no MATCH
  is possible — the search degrades to a full scan with the gate/
  filter/limit clauses applied, with a printed note. `--json` emits the
  per-result object (id, type, title, description, abs_path, rel_path,
  score); `--limit N` caps results (default 20). `bm25`-ranking is only
  selected when a `MATCH` is actually present (the two SQL shapes are
  distinct — a NOT-only full scan selects `NULL AS score` and orders by
  title, which is what keeps the `bm25(pages_fts)` column from being
  referenced without the `JOIN` that only exists under `MATCH`).
- **browse** — a numbered, per-root per-page list over all registered roots
  (or under a `[path]` when given), showing id / title / tags / `search_required_phrase`
  / link count. **Label pages are expanded to their referenced files**
  (`kb-core::labels::classify`), not listed as themselves — the §10
  "a label is a pointer" contract.
- **tag** — `kb tag <tag>`: the full page set carrying that tag (aliases
  expanded, case-insensitive), with label pages expanded the same way as
  `browse`. One line per page (id, title, relative path).
- **tag-alias** — `--add <alias> --canonical <tag>`, `--rm <alias>`,
  `--list`. The `tag_aliases` table is M5's (created by migration); M7 is
  the CLI surface that reads + writes it. The `add` is an `upsert`
  (`ON CONFLICT(alias) DO UPDATE SET canonical = excluded.canonical`), so
  re-adding an existing alias re-targets rather than double-inserting —
  pinned by test. **Multi-word tag values are supported via the quote-aware
  tokenizer**: a `key:` with an empty value followed by a quoted phrase
  (`tag:"large language models"`) is parsed as a single filter whose value
  is the phrase's inner text, not as a filter + separate FTS content term.
- **links** — `kb links <id|path>`: forward (from this page) + backlinks
  (into this page) from the `links` table. The target resolves to an
  `abs_path` via `id` (exact), then `abs_path=` (exact), then
  `abs_path LIKE '%' || ?` (suffix) — so a wikilink's raw written target
  (`[[B]]` → `to_path='B'`) is shown verbatim; the *resolution* of that
  raw target to an `abs_path` is `kb resolve`'s job, not stored here
  (M5 stored the raw text explicitly so the resolver could decide).
- **resolve** — `kb resolve <query>`: the query is matched case-insensitively
  against `id` (exact or substring) and `title` (exact or substring),
  returning a **numbered candidate list** (position, id, type, title,
  abs_path) — *never a hard error*, even if there are multiple matches
  (the §4 "collision is legal; `resolve` disambiguates" contract).
- **info** — `kb info <path>`: the full indexed record for one page (id,
  type, title, description, gate terms, tags, forward/backlink count, mtime,
  created). This is the "show me what the DB thinks about this file" verb —
  the M0–M6 single-file surface is text-only, so `info` is where the indexed
  identity becomes user-visible.
- **stats** — per-root page counts (most-recent root first), totals
  (pages, tags, `search_required_phrase`/gate rows), plus a line for the DB's file
  size and the latest `MAX(updated_at)`/`MAX(created_at)` (the "is this DB
  alive?" diagnostic).
- **cron** — prints a paste-ready `<exe> <db> refresh` line (the DB's
  default path or the resolved `--db`), with the recommended
  `<hourly> * * * *` skeleton. This is the M5 non-goal watch/cron loop's
  "on-demand" half — the *runner* is the user's crontab, not ours.

**Bare-string dispatch.** `main()` inspects `std::env::args()` before `cli
parse`: when exactly one **non-flag** token is given that is **not itself a
known subcommand**, it is dispatched ergonomically — an existing `.md` path
→ `info`, anything else → `search`. A static known-subcommand list keeps
`kb roots`, `kb stats`, `kb tag <one-word>` etc. from being misread as bare
search terms. The dispatcher re-injects the
original flags (e.g. `--db PATH`) in a fresh `Cli::parse_from`, so the bare
form runs against the same DB as the explicit `kb search "..."` form — a
real bug that was found and fixed during this pass (the first version had
dropped the flag and silently searched the default DB). This is the M5 non-
goal "the M7 add" ergonomics, and the one CLI surface where the shell does
an *intentional* pre-parse rather than delegating to `clap`.

**Design notes pinned by test** (`kb/tests/m7_multifile.rs`, 20 tests):
- **add/roots/drop-root round trip** — `add` registers the root and indexes
  in one call (`1 root(s) · 1 page(s) indexed …`); `roots` echoes the
  registered root; `drop-root` removes it and reports the drop. Three tests,
  three verbs, one lifecycle. (M5's `drop_root` semantics are inherited:
  the page survives if it sits under another still-registered root —
  re-pinned here through the CLI.)
- **refresh is idempotent and honest about skips** — the summary is
  `1 root(s) · 1 page(s) indexed · 1 skipped (no-type:1 / no-slug:0) · 0 GC'd`
  on a fresh add (a `no-type` page is skipped + counted, not indexed);
  `full-rebuild` is a drop-in alias of `refresh --full`. Two tests, one
  summary contract.
- **search surfaces the FTS + filter + gate rules** — a content match hits
  the tagged and untagged pages in bm25 order, a `--json` run emits a
  single-line array, a `tag:` filter with a *single-token alias*
  (`tag:models self`, alias `models→llm`) resolves through `tag_aliases`
  to the canonical `llm` and hits, and a `tag:` filter with a **quoted
  multi-word alias** (`tag:"large language models"`, direct tag value, no
  alias needed) is parsed as a single filter whose value is the phrase —
  a grammar fix folded into this pass (§13i non-goals, see below). A
  NOT-only query (`-beta`) degrades to the documented full-scan with no
  `bm25` column referenced — the two-SQL-shape fix is what keeps that path
  from erroring on the `JOIN`-less full-scan.
- **browse / tag expand labels the same way** — both render a label page
  as its *referenced files*, not as itself (the M2 core `labels` module is
  what decides "is this a label?" and what its references are; the shell
  just calls it).
- **tag-alias round trip** — add / list / tag-by-alias / remove, four verbs
  one test; the add is an upsert (pinned by the `→` echo and the subsequent
  `tag <alias>` hit), and the remove reports `removed alias: ...`.
- **links / resolve / info each show their distinct role** — `links` is the
  *stored raw targets* (forward `B`, backlinks `none` because wikilink
  targets are stored as raw text and the `from_path`/`to_path` in the
  table is the *indexer's* absolute path, not the raw string — so a
  wikilink's reverse view is empty by construction), `resolve` is a
  *candidate list* that never hard-errors (`1 note:Attention ... /root/a.md`),
  `info` is the full *indexed row* (id/type/title/desc/gates/tags/links/
  mtime/created). The three verbs are deliberately distinct; the test
  asserts each against its own distinct output shape.
- **stats / cron are diagnostics** — `stats` reports 2 pages, 2 tags, and
  the DB file size as "this DB is real"; `cron` prints the paste-ready
  refresh line with the *resolved* DB path (not the default), plus a
  crontab hint. Neither writes anything; both are the "is the indexer
  alive" surface the user would want in a health-check.
- **bare-string dispatch is the only pre-parse** — `kb <foo.md>` (existing)
  → `info`; `kb zebra` (non-existing, unknown) → `search`. The dispatcher
  re-injects the `--db` flag it received, so the bare form and the explicit
  form run against the same DB. A test pins both halves.

**Result.** `cargo build -p kb` clean (0 warnings). `cargo test --workspace`
**green, 279 tests, 0 failures** (kb-core 177, +2 for the multi-word tag
filter fix; kb CLI 9 diff + 34 section + 20 new M7 integration, +1 for the
same fix; kb-db 12 unit + 6 FTS + 7 integration; kb-core integration
5 + 7 + 2). Full `./verify.sh`: **12 spec files, 317 verified items,
0 errors** (the fix is in `kb-core`'s `parse`, a consumer of the
whitelist/is_filter_key surface spec — the spec does not pin `Token`
or `tokenize` internals, so the verified count is unchanged).

**Explicit non-goals** (parked for M8+):
- **`--snippet` output** — the FTS5 `snippet()` 6-arg form is wired (M0
  probe, §13a) but not exposed in the CLI; the M7 search rows show
  `rel_path` and the description, not a highlighted excerpt. The `snippet`
  SQL is written and tested on SQLite but not surfaced.
- **`--out` / watch-rebuild loop** — `refresh` is on-demand; a cron runner
  is the *user's* job (`kb cron` prints the line; the scheduling is their
  crontab). An inotify/fanout M8 candidate.
- **TUI (`kb-tui`)** — see M8.
- **Export/import `tag_aliases`** — landed as M7c (see §13k); the recovery
  pathway is no longer deferred.

### 13j. M7b results — cross-page link resolution (built + tested)

**Scope.** M7b closes the §4 "collision is legal" gap: the 5-stage
waterfall that was *promised* in §4 and *sketched* in §11 is now *built* as
a pure, Verus-verified `kb-core` primitive rather than a SQL-shaped hack in
the shell. The core adds exactly one module and its spec:

- `kb-core/src/resolve.rs` — `Page { abs_path, slug, typ, rel_path }`,
  `Resolution::{Unique, Candidates, Empty}`, `resolve(query, pages)`,
  plus helpers `split_type_slug` (the type-qualified prefix parser) and
  `path_matches` (root-relative path matching). All pure; `pages` is
  passed by value so the resolver is a *function of the corpus*, not an
  object with an implicit state.
- `kb-core/spec/resolve_spec.rs` — 17 lemmas pinning the contract:
  3-way outcome partition, stage-1 (`type:slug`) over stage-2 (bare slug)
  precedence, determinism, candidate-set well-formedness (non-empty, no
  duplicates), and empty-outcome total — i.e. the *shape* of the
  resolution is the only thing the spec claims; the *order* of the
  waterfall is pinned by the source, the *set* of outcomes is pinned by
  the spec, and a `verus` change to either surface is a compile /
  verification failure.

The shell's `resolve` / `links` verbs become *thin consumers* of this one
primitive: the corpus projection `load_corpus(db) -> Vec<resolver::Page>`
is the single place where SQL meets the pure core, and the three verbs
share it. This is what makes "wikilink backlinks" actually work: the old
SQL `WHERE to_path = ?` on `links` failed for any raw target that was a
bare slug ("B") rather than an absolute path; the new backlink set is
computed by *inverting resolved edges* — i.e. for each candidate source
page A, we look at A's forward edges, resolve each raw `to_path`, and
count a backlink if any of those resolutions return B's `abs_path`.

**Result.** `cargo build -p kb` clean. `cargo test --workspace`
**green, 297 tests, 0 failures** (all suites — kb-core lib, kb-core
integration, kb diff, kb section, kb CLI, kb DB, kb FTS, kb grammar,
kb fs_snapshot, kb links, kb section, kb fm; +12 `resolve` unit tests
and +6 M7b link-resolution integration tests net-new over the M7 close).
`./verify.sh` **13 spec files, 334
verified items, 0 errors** (the new `resolve_spec.rs` adds 17).

**Explicit non-goals** (parked for M8+): the `resolve` verb does not yet
do `type:slug` *fuzzy* matching (only the exact 5-stage waterfall); the
`links` backlink view is O(corpus × edges) and not precomputed (a
`backlinks` view-index is an M8 candidate once `kb-tui` needs it).


---

## Appendix A — Example note files (frontmatter + body)

Concrete examples anchoring the schema, identity rules, tags, labels, and the `search_required_phrase`
gate. These are the shapes the parser (`kb-core::frontmatter`) and indexer
(`kb-db::index`) must accept. All three types live under **different registered roots** to show
multi-root identity.

> **Convention:** frontmatter is delimited by `---` on the first line. Allowed values in v1:
> scalars (string / int / bool), inline lists `[a, b]`, block lists (`- item`), and `#` comments.
> **No nested maps** in v1.
> Multi-word `search_required_phrase` entries are **double-quoted** (`"token bucket"`). In `pages`,
> entries are stored verbatim (quotes included); in `page_gates`, quotes are stripped so the
> matching unit is the whole phrase `token bucket`.

---

### A.1 — `type: wiki` (a visible knowledge page that *mentions* a term)

File: `wiki/transformer.md` (root: `~/notes`)

```markdown
---
type: wiki
title: Transformer (architecture)
language: en
description: How the Transformer works — self-attention, positional encoders, multi-head attention. Foundation of modern LLMs.
tags:
  - transformer
  - llm
---

# Transformer (architecture)

The Transformer is a neural network architecture for sequence transduction,
introduced by Vaswani et al. (2017). It replaces recurrence with self-attention
and became the basis of modern LLMs.

> The primary paper is tracked by the [[LLM]] label — see its Files list for
> the actual PDF (`papers/attention-is-all-you-need.pdf`).

## Key components

- Multi-head self-attention
- Position-wise feed-forward layers
- Positional encoders (no recurrence, no convolution)
```

**What this exercises:**
- **Visible (gateless)**: no `search_required_phrase` in frontmatter → it appears in a general
  `kb search "transformer"` (clean surface). It is the *explanatory* page.
- `wiki:Transformer (architecture)` display id; slug from frontmatter `title` (no H1 required).
- **Mentions a term** ("transformer", "attention", "LLM") — the link that matters is the
  `[[LLM]]` wikilink, which points at the **label**, and the label's `## files` list points at the
  **real PDF** behind that term. So the wiki is the concept; the label is the pointer to the source.
- **Carries `tags: [transformer, llm]`** — any page type can carry tags; `kb tag llm` returns
  *both* this wiki and the LLM label (rendered as its 4 files).

---

### A.2 — `type: note` (a note about its own folder)

File: `projects/api-gateway/README.md` (root: `~/work`)

```markdown
---
type: note
title: API Gateway — notes
language: en
description: Internal design notes — gRPC vs REST, JWT auth, mTLS, rate-limit algorithms (token bucket).
tags:
  - rust
  - http
  - grpc
---

# API Gateway — notes

Working notes for the team's gateway. This folder is the source of truth for
the design. The actual implementation lives in `../rust/gateway/`.

## Decisions

- gRPC for internal, HTTP/REST for external.
- Auth via JWT; see [[Transformer (architecture)]] for the team's attention-based
  rate-limiting spike.

## Open questions

- Do we need mTLS for east-west traffic yet?
- Rate-limit algorithm: token bucket vs sliding window
```

**What this exercises:**
- `note:API Gateway — notes` display id, slug from frontmatter `title` `API Gateway — notes`.
- `tags: [rust, http, grpc]` → three rows in `page_tags` for this note's `abs_path`. Tags are
  plain strings on *any* page; here they simply mark the note as relevant to rust/http/grpc.
- Folder binding is *implicit* (this note describes `projects/api-gateway`).
- Contains a `wiki` link to the Transformer page (root `~/notes`) — a cross-root reference.

---

### A.2a — same note, *gated* (the `search_required_phrase` version)

If the same gateway note were a deep-dive page, its frontmatter would additionally carry:

```yaml
search_required_phrase:
  - mTLS
  - "token bucket"
```

(Gate entries are words **or quoted phrases**; multi-word entries are always quoted in
frontmatter.)

Now `kb search "gateway"` no longer shows it — it only surfaces once the **entire** gate
entry occurs in the query: `kb search "gateway mTLS"` unlocks it (lone word `mTLS` present),
and `kb search "gateway token bucket"` also does (full phrase `token bucket` present), but
`kb search "gateway token"` does **not**. This is the **only** example in the appendix
that carries the gate; A.1/A.2/A.3/A.4/A.1b are all gateless (always visible).

---

### A.3 — `type: label` (a label page with a `## files` list, pointing at non-MD resources)

File: `library/llm.md` (root: `~/library`)

```markdown
---
type: label
title: LLM
language: en
description: Private collection of 4 LLM papers/books (PDF/EPUB) tracked by the team.
tags:
  - llm
  - attention
---

# LLM

Large Language Model papers we read or reference. Each file below is one book
or paper in our private collection.

## Files

papers/attention-is-all-you-need.pdf
papers/multitask-chain-of-thought.pdf
papers/scaled-robust-fine-tuning.pdf
books/deep-learning-goodfellow-ch3.epub

## Summary

Mixed reading. The attention paper is the canonical starting point. Goodfellow
ch3 is a refresher on the math underneath.
```

**What this exercises:**
- `label:LLM` display id — a label is an ordinary page keyed by its `abs_path` like any other.
- **Carries `tags: [llm, attention]`** → rows in `page_tags`; this is how `kb tag llm`
  finds it. (Any page could carry these tags — the label just additionally *points at files*.)
- **No alias knowledge in the note.** Alias knowledge (e.g. that
  "large language models" should also land here) is a **DB-level string→string mapping** managed
  by `kb tag-alias --add "large language models" --canonical "llm"` — it does *not* live in the
  note. Tags are strings; aliases map strings to strings.
- **`## files`** heading present → the 4 lines under it are the referenced files.
- **Non-MD**: none of those 4 files are in the DB (they're not `.md` indexed pages). They are only
  surfaced when the tool expands this label (`kb tag llm` → these 4 rel-paths), read live from the note.

### A.4 — `type: label` (folder-wide default, no `## files` heading)

File: `library/czech.md` (root: `~/library`)

```markdown
---
type: label
title: Czech
language: en
description: Czech language, culture, history — folder-wide label.
tags:
  - czech
---

# Czech

Everything about the Czech language, culture, and history we track. This label
references every non-MD file in its own folder (`library/`), unless a more specific
label says otherwise.
```

**What this exercises:**
- **No `## files`** heading, body is prose (only 2 sentences, no path lines).
- Body opens with prose (no leading path-list run) → **folder-wide default** applies: the
  label references its own folder (non-MD content of `library/`).
- **Carries `tags: [czech]`** → `kb tag czech` finds this label and displays the folder.
- **No alias knowledge in the note.** If you want `kb tag "Czechia"` or
  `kb tag "Czech Republic"` to also land here, you add that mapping via
  `kb tag-alias --add "Czechia" --canonical "czech"` — a **DB-level string→string** mapping,
  not a property of the label note itself.

### A.1b — `type: label` (body is *only* a path list; the "actual PDF behind a term" case)

File: `library/transformer.md` (root: `~/library`)

```markdown
---
type: label
title: Transformer (papers)
tags:
  - transformer
---

# Transformer (papers)

papers/attention-is-all-you-need.pdf
papers/transformer-xl-attentive-language.pdf
```

**What this exercises:**
- **No `## files`** heading, body *is* only a list of relative paths (after the H1).
- **Case (2)**: This is the
  **"label pointing at the real PDF behind a search term"** pattern: the visible wiki page
   (A.1, `transformer.md` in `~/notes`) explains *what* a transformer is; the label here
    (`transformer.md` in `~/library`) points at the actual PDFs. Same **filename**, different
    slug (`Transformer (architecture)` vs `Transformer (papers)`) and different folder →
    different `abs_path` (the real key) and different display ids `wiki:`/`label:`.
- Contrasted with A.4 (folder-wide). The parser distinguishes case (3) "body = prose, no
  leading path run" from case (2) "body opens with a run of path lines". Rule: look for a
  `## files` heading *first* (case 1); if absent, take the lines after the title (the H1; or,
   when the page has no H1, immediately after the frontmatter) and require the
   run to be **leading** (case 2: first non-empty line is a relative path, continue while
   lines are relative paths; prose appearing *before* any path line forces case 3); if the
   first non-empty line is prose → case (3).

---

### A.6 — Reference: identity map for the examples above

| File | type | slug source | display id | root |
|---|---|---|---|---|
| `wiki/transformer.md` | wiki | `title:` | `wiki:Transformer (architecture)` | `~/notes` |
| `projects/api-gateway/README.md` | note | `title:` | `note:API Gateway — notes` | `~/work` |
| `library/llm.md` | label | `title:` | `label:LLM` | `~/library` |
| `library/czech.md` | label | `title:` | `label:Czech` | `~/library` |
| `library/transformer.md` | label | `title:` | `label:Transformer (papers)` | `~/library` |

**Note on identity:** every page's **real key is its absolute path** (the `abs_path` PK, §4/§8);
the `id` column is only a **display string** and may collide between files. Here the files are in
distinct roots so the display ids happen to be unique, but the tool never *relies* on that — it
joins on `abs_path`, then resolves `id` for any `[[wikilink]]` on demand (§4 resolution order). The
`tag_aliases` **DB table** is the controlled way to say "when the user types `large language
models`, also match the tag `llm`". It is not a property of any one label note; it is a global
string→string map managed by `kb tag-alias`.

---

### A.7 — Example: what the DB looks like after indexing A.1–A.1b

`pages` (keyed by `abs_path` — truncated for width; full values are the `description:` frontmatter from each example; `Transformer (papers)` is intentionally **without** a `description`, showing the optional case):
```
abs_path                                type    search_required_phrase  description (first ~50 chars)
--------------------------------------  ------  -----------  -------------------------------------------------
~/notes/wiki/transformer.md              wiki    NULL         How the Transformer works — self-attention, posi…
~/work/projects/api-gateway/README.md    note    NULL         Internal design notes — gRPC vs REST, JWT auth, …
~/library/llm.md                         label   NULL         Private collection of 4 LLM papers/books (PDF/EP…
~/library/czech.md                       label   NULL         Czech language, culture, history — folder-wide lab…
~/library/transformer.md                 label   NULL         NULL
```

> All 5 pages are **visible** (gateless). If a future page needs the `search_required_phrase` gate,
> it would *not* show in `kb search "transformer"` (general) until the query contains one
> of its full gate entries (word or quoted phrase, e.g. A.2a's `"token bucket"`).
> **Non-md files are not rows.** `papers/*.pdf`, `books/*.epub`, and the non-MD content a
> label's folder-wide reference would expose, have *zero* representation in the DB — they only
> exist as live disk paths behind a label.

`page_tags` (tags are plain frontmatter strings on any page; keyed by `abs_path`, middle column is the display id for readability):
```
abs_path                                display id                        tag
--------------------------------------  --------------------------------  ---------
~/notes/wiki/transformer.md             wiki:Transformer (architecture)   transformer
~/notes/wiki/transformer.md             wiki:Transformer (architecture)   llm
~/work/projects/api-gateway/README.md   note:API Gateway — notes          rust
~/work/projects/api-gateway/README.md   note:API Gateway — notes          http
~/work/projects/api-gateway/README.md   note:API Gateway — notes          grpc
~/library/llm.md                        label:LLM                         llm
~/library/llm.md                        label:LLM                         attention
~/library/czech.md                      label:Czech                       czech
~/library/transformer.md                label:Transformer (papers)        transformer
```

> The *file references* of `label:LLM` (the 4 PDFs/EPUBs) live only in the **note body on disk**,
> not in the DB. `kb tag llm` finds `wiki:Transformer (architecture)` *and* `label:LLM`; it then
> renders the label by reading its body and listing the 4 files.

`tag_aliases` (managed *by the user* via `kb tag-alias --add … --canonical …`, not by any note):
```
alias                          canonical
------------------------------  -------
large language models          llm
Czechia                        czech
Czech Republic                 czech
```

`links` (endpoints are page `abs_path` values; `display id` shown for readability):
```
from_path (=~/notes/wiki/transformer.md)        to_path (=~/library/llm.md)           style
------------------------------------------------  --------------------------------  ----------
~/notes/wiki/transformer.md                      ~/library/llm.md                      wikilink
~/work/projects/api-gateway/README.md            ~/notes/wiki/transformer.md           wikilink

display: wiki:Transformer (architecture) → label:LLM ; note:API Gateway — notes → wiki:Transformer (architecture)
```

**The workflow these tables support (the A.1 ↔ A.1b ↔ LLM chain):**

1. User types `kb search "transformer attention"` → the FTS query is an implicit **AND** of the
   two tokens. Both `wiki:Transformer (architecture)` (A.1) **and** `label:Transformer (papers)`
   contain "transformer" (title/body) *and* "attention" (the `attention-is-all-you-need.pdf` path),
   so both match; the wiki is the explanatory read, the label the pointer. `label:LLM` does **not**
   match — its body has "attention" but never "transformer". A single-token query like
   `kb search "attention"` would also surface `label:LLM` (it has that token).

2. User reads the wiki page → sees "the primary paper is tracked by the
   [[LLM]] label" → runs `kb tag llm` → returns:
   - `wiki:Transformer (architecture)` (also tagged `llm`)
   - `label:LLM` → **expanded to its 4 referenced files** (the real sources).

3. User types `kb tag "large language models"` → `tag_aliases` resolves
   `large language models → llm` → same result set as step 2.

This is the **"wiki mentions a term; label points at the actual files behind that term"**
pattern, plus **DB-level tag-string aliasing** that lets the user reach the same target by
different spellings. The label notes themselves carry *no* alias knowledge.

### 13k. M7c results — tag-alias import/export, the recovery table (built + tested)

**Scope.** `tag_aliases` is one of only *two* pieces of DB state that do **not**
rebuild from the tree (§2: `roots` + `tag_aliases`). `roots` is user-registered and
re-`add`-able; the alias map is *knowledge* the user curated (`Czechia → czech`) and
has no on-disk derivation. So a wiped DB (a crash, a `rm ~/.local/share/kb`, a
fresh-clone) silently loses the alias map forever. M7c closes that with a recovery
pathway: export the map to a portable, human-readable file, and re-import it after a
wipe.

**Design.** Split exactly as every other module:
- *Pure core* (`kb-core::aliases`, no IO, no `std::fs`): the byte↔row codec.
  `to_text(&[(String,String)]) -> String` (export; sorts by `(alias, canonical)`
  so the output is a *function of the set*, not the DB's row order; dedups;
  `alias\tcanonical` per line; empty set → the empty string, **not** a lone
  `\n`), `parse_line(&str) -> Option<(&str,&str)>` (split on the *first* tab;
  trim both fields; a line with no tab / an empty field / an ignorable line →
  `None`), `line_is_ignorable(&str) -> bool` (blank or `#`-comment), and
  `from_text(&str) -> Vec<(String,String)>` (the inverse of `to_text` over the
  DB's domain; **last-wins on a repeated alias**, matching the `--add` upsert,
  with first-occurrence *order* of the surviving set preserved).
- *Verified spec* (`kb-core/spec/alias_spec.rs`, 16 lemmas): the separator is a
  real single token (and distinct from the newline terminator, so a field-sep
  and a line-terminator are never the same byte); a data line **decomposes** as
  `alias ++ TAB ++ canonical ++ NL`, with the alias a recoverable *prefix* and
  the canonical a recoverable *suffix* (the parse-recovers-the-fields contract);
  the alias is a *strict* prefix of the line (a line never degenerates into a
  bare alias); the export contains the separator; and the per-line import
  decision is a clean, total, **disjoint** partition of ignorable / data /
  candidate. Concrete round-trips (export→import == identity on the DB's
  unique-alias, tab-free domain; idempotence; skip-malformed-without-losing-the-rest)
  are pinned by the unit tests — the same exec↔spec split as `tags.rs`.
- *Shell* (`kb/src/main.rs`, `cmd_tag_alias`): owns the file + the DB. New flags
  on `tag-alias`: `--export` (to `--file`, or **stdout** so
  `kb tag-alias --export > backup.tsv` is the natural idiom) and `--import`
  (from `--file`, or **stdin** so `kb tag-alias --import < backup.tsv` works).
  `--import` reports the *skip count* (blank/comment/malformed) so a
  partially-typed backup is never silently truncated, and upserts under a single
  `BEGIN`/`COMMIT` (one transaction, no half-loaded state).

**Why this is the recovery pathway and not a feature.** `roots` re-registration
is already a verb (`kb add`). This is the *only* other non-derivable state, and
it has no `add`-equivalent: the knowledge lives nowhere but the row. Exporting
to a boring, diff-stable, comment-annotatable TSV is the durable backup;
importing is the restore. The two verbs together are the "the DB got wiped,
my aliases came back" contract.

**Tests (all green).** 16 new core unit tests (`kb-core/src/aliases.rs`), 4 new
integration tests (`kb/tests/m7_multifile.rs`): `…_export_then_wipe_then_import_
recovers_the_map` (export to a file, **physically delete the DB file**, confirm
the list is empty, import the file, confirm the map is back *and still
resolves* through `tag <alias>`), `…_export_empty_is_empty` (+ importing the
empty backup is a no-op), `…_import_is_idempotent` (import twice → no row
duplication, exactly the two rows), `…_import_skips_malformed_lines_without_
losing_the_rest` (comment + blank + no-tab + empty-field lines are skipped and
reported, the two good rows still load). `cargo test --workspace` **green,
0 failures**. `./verify.sh` **350 verified / 0 errors** (14 spec files).

### 13l. M8.0 results — `kb tui` scaffold + browse (built + tested)

**Scope.** M8 is the *interactive* surface `kb` has been carrying as a
deferred item through M0–M7c: an in-process TUI over the same `Db` /
`kb-core` / `kb-db` that every CLI verb already uses. The two properties that
make it safe to build on the existing stack:
1. **Shared in-process.** `kb tui` is a subcommand (not a sibling binary), so
   it opens the *same* `Db::open`, and the *same* core modules. Zero IPC,
   zero fork, zero re-derivation of state a fresh CLI verb would have to
   rebuild. The TUI is just another consumer of the contract.
2. **Shell, not core.** The TUI is the `assumed` half of the split
   (§11 design): it owns terminal state (alt-screen, raw mode, event loop)
   and the paint pass, **not** the corpus-identity or edit semantics. The
   verified core (`kb-core::frontmatter` for M8.2 edits, `kb-core::resolve`
   for M8.3 links) is unchanged by this milestone — the TUI just *calls*
   it. So **no new spec files** land in M8.0; the 350 verified count is
    held, not extended (M8.1 carries the same shell-only property, and
    M8.2's tab-autocomplete contract may add a small
   spec if the matching predicate turns out to need a proof, but the
   scaffold itself is a pure-shell increment).

**Design.** Four files in `kb/src/tui/`:
- `mod.rs` — the `App` state machine (one value, no hidden state, the event
  loop owns it), the `PageRow` / `Mode` / `Status` / `StatusKind` data,
  `list_pages(&Db)` (a snapshot query so a keystroke is O(1) on an
  already-built `Vec`), and `run_tui(Option<&Path>)` — raw-mode on →
  alt-screen on → `Drop`-guard restores on exit (the *one* invariant: the
  user's terminal is never left in raw mode, even on a panic).
- `input.rs` — `handle_event(&mut App, KeyEvent) -> bool` (the `bool` is
  "quit?"). Only `Press`/`Repeat` kinds reach the bindings; `Release` is
  filtered up in mod.rs. Binding table (M8.0): `↑`/`k`, `↓`/`j`,
  `Home`/`PgUp`, `End`/`PgDn` move the selection; `Enter`/`a`/`/` are
  reserved for M8.1/M8.2/M8.3 and *report* themselves to the status bar
  (rather than being silent, so the user always knows whether their key
  was seen); `q`/`Esc`/`C-c` quit. Stray keys are ignored *deliberately* —
  the status bar reflects the last *meaningful* event, not every
  keystroke.
- `render.rs` — a ratatui `Layout` (header 1 row / list `Min(1)` / status
  1 row). The list is a `StatefulWidget` so the selection highlight is
  driven by the `app.selected` index (not a separate cursor, which would
  risk two sources of truth). `truncate(s, n)` caps by **char** (not byte)
  so a multi-byte title never panics a `&s[..]` at a non-boundary; the
  stored title is untouched (render-side cosmetic only).
- `main.rs` — the `Cmd::Tui` variant, the "tui" entry in the `SUBCMDS`
  allow-list (so `kb tui` is not misread as a bare search for the word
  "tui"), and the dispatch arm that lifts `run_tui`'s `i32` into the
  `(String, i32)` `Kb` contract the rest of the CLI uses.

**Invariants the shell owns.**
1. `move_selection` is **total** on any list and **bounded** on
   `[0, rows.len()-1]` (pinned by `move_selection_is_total_and_bounded`,
   including the empty-list no-op path — `k` at row 0 stays 0, `j` at the
   last row stays last).
2. `StatusKind::Info / Success / Error` are **three distinct values**
   (pinned by `status_kinds_are_distinct`) so the status bar's color is a
   function of the *kind*, not a fuzzy text match — M8.2 will be the first
   consumer of `Success`/`Error` (edit-commit outcome), and that code is
   safe to write because the "the kind I passed is the color I get"
   contract is already locked down.
3. `App::selected` never aliases out of `rows` range (enforced by the two
   mutators, checked by the invariant tests above).

**Tests (all green).** 2 new unit tests in `kb/src/tui/mod.rs`
(`move_selection_is_total_and_bounded`, `status_kinds_are_distinct`), 11
in the CLI crate total (up from 9, the two new ones). `cargo test
--workspace` **green, 0 failures** (319 total at M8.0 close). `./verify.sh`
**350 verified / 0 errors** (unchanged — M8.0 is purely shell).

**Next up — M8.1.** The `Read` mode (`Enter` on a selection): open the
page in a scrollable two-pane view (body left, section outline right), and
wire `/` to a filter over the `rows` snapshot (the *filter predicate* can
reuse the verified `search_grammar::matches` so the same query string a
`kb search` accepts drives the TUI's filter — one more proof of the
"shell, not core" split).

### 13m. M8.1 results — `kb tui` read + filter (built + tested)

**Scope.** M8.1 completes the two most-used TUI verbs: *reading* a page
(open, scroll, and see its section outline) and *filtering* the browse
list by name. Both are pure-shell: they add terminal state and a paint
pass to the M8.0 scaffold, and **reuse already-verified kb-core**
(`kb-core::section::heading_of` for the outline — the *same predicate*
`kb toc` renders, so the TUI's outline can never drift from the CLI's; the
frontmatter ops that M8.2 will add are untouched this milestone). No new
spec files — the 350 verified count is **held**, not extended.

**Design.** The M8.0 `App` grows three things, all in `mod.rs`:
- `view: Vec<PageRow>` — the *filtered* row list the browse screen paints.
  `rows` stays the full snapshot; `view` is derived from it by
  `apply_filter()` (case-insensitive substring over `id` / `title` /
  `rel_path`). This is deliberately **not** FTS — a filter over 500 pages
  is a name search, and the user is honest about it in the header
  (`"filter: attention"` vs. a query grammar). Selection indexes `view`,
  not `rows`, so a filter never points at a hidden row.
- `filter_text` + `filter_active` — the input state (text typed since `/`).
  `filter_active` is the "am I in the filter box?" flag; the event loop
  checks it *before* mode dispatch, so a `/` in filter-box mode commits
  rather than re-arms, and `Esc` clears the box rather than quitting.
- `Mode::Read(ReadState)` — `ReadState` carries `abs_path` (the *stable*
  identity, so a re-ordered `view` after a `full-rebuild` can't open the
  wrong page), `id` / `title` / `row_idx` (for the header), `body`
  (loaded lazily from the DB by path on `open_read`), and
  `outline: Vec<(usize, u8, String)>` (line index, level, title — the
  `heading_of` extraction). `scroll_top: usize` is the first visible line;
  `scroll_body(n)` / `_top` / `_bottom` mutate it under a `max_top` cap so
  it is **total + bounded** on any body (an empty body caps at 0; the
  render window takes from `scroll_top`).

  Bindings (M8.1): `Enter` opens the selection into `Read`; in `Read`,
  `↑`/`↓`/`j`/`k` scroll a line, `PgUp`/`PgDn` a page, `g` top, `G` bottom,
  `Esc` → `close_read` (back to Browse with the selection *preserved*).
  In Browse, `/` enters the filter box, and an empty `view` makes `Enter`
  a clean no-op (total on the degenerate input). `q` and `C-c` quit from
  any mode (checked before mode dispatch).

**Invariants the shell owns.**
1. `scroll_max_top(body)` is a **pure function of the body** and returns
   `lines - 1` (capped, so every reachable `scroll_top` renders a
   non-empty window; empty body → 0). Pinned by
   `scroll_max_top_is_a_pure_function_of_the_body`.
2. The scroll mutators are **total and bounded**: `scroll_body(n)` leaves
   `scroll_top ∈ [0, max_top]` for any `n`, and stepping past an edge is a
   no-op (clamped). Pinned by `scroll_body_is_total_and_bounded`
   (+ the extremes test, including the empty-body degenerate).
3. `apply_filter` is **total**: empty filter → `view == rows`; no matches
   → empty `view` with `selected == 0` (in-range, never OOB). Pinned by
   `apply_filter_is_total_and_case_insensitive` (case-insensitive, and
   `selected` clamps into the new bounds on every re-filter).
4. `open_read` resolves by **absolute path**, and `close_read` preserves
   `selected` — so Esc-then-`j` continues where the user left off, and a
   DB re-scan cannot open the wrong page. Pinned by
   `open_read_closes_and_preserves_selection` +
   `open_read_on_empty_view_is_a_noop`.
 5. The outline is exactly a `heading_of` extraction (valid ATX headings
    only, in body order, each tagged with its line index + level). Pinned
    by `open_read_outline_is_a_heading_extraction_in_body_order` — the
    "one predicate, two surfaces" contract: the TUI outline and `kb toc`
    are the same function over the same bytes, and the assertion runs
    through `open_read` (the Read page's outline is populated by the same
    `outline_of` that renders it), so the *pipeline* is the pinned surface,
    not just the pure helper. (One honest deviation from the §13l "Next up"
    sketch: the filter is a substring predicate, not
    `search_grammar::matches` — the name-search is smaller and more
    predictable, and FTS-grade search is already one keystroke away on the
    CLI.)

**Tests (all green).** 7 new unit tests in `kb/src/tui/mod.rs`
(`scroll_max_top_is_a_pure_function_of_the_body`,
`scroll_body_is_total_and_bounded`, `scroll_top_bottom_are_the_extremes`,
`open_read_outline_is_a_heading_extraction_in_body_order`,
`apply_filter_is_total_and_case_insensitive`,
`open_read_closes_and_preserves_selection`,
`open_read_on_empty_view_is_a_noop`). `cargo test --workspace` **green,
0 failures** (326 total at M8.1 close, up from 319). `./verify.sh`
**350 verified / 0 errors** (unchanged — M8.1 is purely shell). A pty
smoke (`/tmp/opencode/tty_smoke.py`) drives the release binary through
Browse → `Enter` → Read (body + `L1`-tagged outline render) → `Esc` →
`q`, and the child **exits 0** with the terminal restored; `kb tui` on a
non-TTY soft-fails with "could not enable raw mode" and exit 1.

**Next up — M8.2.** The `Edit` mode: frontmatter `title` / `description`
/ `tags` / `search_required_phrase` over the *already-verified* `kb-core::frontmatter`
ops (`set_scalar`, `append_list_item`, `remove_list_item`, `remove_field`,
`list_values`, `scalar_value`), with **tab-autocomplete** listing matching
tags/search_required_phrase from the corpus, and a green/red commit status (the first
consumer of `StatusKind::{Success, Error}` — the contract locked in
§13l). This is the milestone that may add a small spec file if the
autocomplete matching predicate turns out to need a proof.

### 13n. M8.2 results — `kb tui` edit + autocomplete (built + tested)

**Scope.** M8.2 is the first TUI verb that *writes*: it lets the user edit
a page's frontmatter — `title` / `description` (scalars) and `tags` /
`search_required_phrase` (lists) — in place, with **tab-autocomplete** over the
corpus's known tokens, and commit the change atomically. It is still
**shell-only**: every byte-level frontmatter op delegates to the *already
verified* `kb-core::frontmatter` (`set_scalar`, `append_list_item`,
`remove_list_item`, `remove_field`, `list_values`, `scalar_value`); the
TUI owns only the *composition* (which op, in what order, over which
baseline). **No new spec files** — the autocomplete predicate
(`suggestions_for`) is a plain case-insensitive substring filter over a
`Vec<String>`, small and trivially spec-able if it ever needs a proof.
The `./verify.sh` 350 count is **held**, not extended.

**Design.** The M8.0/M8.1 `App` grows the `Edit` mode, all in `mod.rs`:

- `Mode::Edit(EditState)` — `EditState` is the working state for one page:
  `abs_path` (stable identity — the *file* is the source of truth) and
  `id` (header); `field: EditField` (the cursor target); and **two**
  byte baselines that keep the disk-guard and the working state honest:
  `entry_bytes` / `entry_list` (fixed at `open_edit`, **never** mutated
  until a commit succeeds — the "file changed on disk?" hash guard, and
  the no-op rule's reference) vs. `current_list` / `working_bytes`
  (the *live* state, mutated by `x` remove-tail previews). The `value`
  seed depends on the field kind: a **scalar** seed is the *current* value
  (edit-in-place; an unchanged commit is a no-op), a **list** seed is
  empty (here it is the *new* comma-separated tokens to append; the
  existing items show separately in `current_list`).
- `EditField::{Title, Description, Tags, SearchTerms}` with
  `key()` / `label()` / `is_list()` / `next()` / `prev()` (the
  `↑`/`↓`/`j`/`k` cycle, wraps at the edge).
- `EditOp::{SetScalar, AppendItem, RemoveField, RemoveItem}` — a *data*
  record of "what will commit apply" (surfaced in the render as the
  "pending plan" and run by `commit_edit`). The actual byte work is in
  `apply_op`, a thin dispatch to the verified core.

  **The one-invariant that matters:** the *display* path and the *commit*
  path are the **same function**. `EditState::plan()` calls
  `build_ops(field, value, original, existing)` — exactly the call
  `commit_edit` runs — so the "pending" footer the user sees before
  pressing `Enter` cannot diverge from what is written. `build_ops` is
  the "edit-in-place" contract, and it is the pinned core:
  - scalar, `v == original` (trimmed) → **no-op**; scalar, changed +
    non-empty → one `SetScalar`; scalar, cleared from non-empty → one
    `RemoveField`; scalar, cleared from empty → **no-op**;
  - list, empty buffer → **no-op**; list, one-or-more comma tokens → one
    `AppendItem` per *new* token (dedup'd against `existing`, empty /
    whitespace tokens dropped, order preserved).

**Bindings (M8.2).** In Browse, `e` opens the selected page into `Edit`
(and in Read, `e` opens *the page already being read* — the natural
"edit this" shortcut). In `Edit`: `↑`/`↓`/`j`/`k` cycle the field (wraps);
any printable char is typed into `value`; `Backspace` pops; `x` drops the
tail item of a *list* field (live `working_bytes` preview — the only path
that mutates working state before commit); `Tab` is a no-op on the *state*
side (suggestions are a pure function of `(corpus, value)`, recomputed on
every type/backspace — the render simply shows them); `Enter` commits;
`Esc` → `close_edit` (back to Browse, **selection preserved**, pending
buffer discarded — `close_edit` never writes to disk); `a` is reserved for
M8.3 (reports itself); `q` / `C-c` quit.

**The commit pipeline (one path, no forks).** `commit_edit`:
1. **read the file on disk** and compare to `entry_bytes` — a mid-session
   external edit (another editor, a `kb commit`, a re-index) is **refused**
   with a clean error, not clobbered (the "re-read before write" contract
   the CLI `commit` uses in M6, now in the TUI);
2. **apply** the `plan()` ops in order over a copy of `working_bytes`
   (`apply_op` never mutates its input, so a mid-way failure leaves the
   disk untouched — the write below is the only mutator);
3. **atomically write** via the *same* `crate::atomic_write` the CLI uses
   (a no-op commit skips the write — "open then commit" leaves the file
   byte-identical: no artifact);
4. **re-index the single page** (`build_page` re-reads the file from disk,
   parses the verified frontmatter → `PageRecord`; `upsert_page` writes it
   in one transaction) and **refresh** the browse `rows` — the
   "source of truth is the file, the DB is a cache" contract;
5. **green status** with a human op-summary (first real use of
   `StatusKind::Success`; the step-3/4 failure paths are *warnings*, not
   errors — the data is on disk, a later `kb refresh` re-syncs).

**Invariants the shell owns (pinned).**
1. `suggestions_for(corpus, buffer)` is **pure + total**: empty corpus →
   empty (no panic); empty buffer → the *full* corpus (the "available
   tokens" hint); a non-empty buffer filters by **case-insensitive
   substring**. Pinned by `suggestions_for_is_pure_and_filters_correctly`.
2. `build_ops` obeys the **edit-in-place contract** on both branches —
   unchanged scalar is a no-op, the no-op/`SetScalar`/`RemoveField`
   dispatch is exact, and the list branch appends *only new* tokens
   (dedup'd, trimmed, order preserved, empty tokens dropped). Pinned by
   `build_ops_scalar_contract` + `build_ops_list_contract`.
3. `apply_op` is **total over all four variants** and delegates to the
   right verified op, so the dispatch cannot drift (the *byte* behavior is
   kb-core's, proven there — the TUI test asserts each op produces a
   parseable result with the expected value, via `scalar_value` /
   `list_values`). Pinned by `apply_op_is_total_over_the_four_variants`.
4. `EditState::plan` **dispatches by field kind** to the same `build_ops`,
   so the render's "pending" footer and `commit_edit` are one function —
   pinned by `edit_state_plan_dispatches_by_field_kind` (scalar
   unchanged→no-op, scalar changed→`SetScalar`, list empty→no-op, list
   new→`AppendItem`, list re-add→no-op) + `current_plan_delegates_to_edit_state_plan`.
5. `open_edit` on an **empty view is a no-op** (total on the degenerate
   input; mode unchanged; `Ok`). Pinned by
   `open_edit_on_empty_view_is_a_noop`.
6. `close_edit` **discards the pending buffer without writing** — the file
   is byte-identical after a round trip with no commit — and returns to
   Browse. Pinned by `close_edit_discards_the_pending_buffer`.
7. `commit_edit` **round-trips**: a modified scalar is written to disk,
   the DB row is refreshed to the new value (asserted through `app.db`),
   the status is green, and the mode is left in `Edit` (a follow-up field
   switch / commit needs no re-open). Pinned by
   `commit_edit_round_trip_on_modified_scalar`.

**Tests (all green).** 9 new unit tests in `kb/src/tui/mod.rs`
(`suggestions_for_is_pure_and_filters_correctly`,
`build_ops_scalar_contract`, `build_ops_list_contract`,
`apply_op_is_total_over_the_four_variants`,
`edit_state_plan_dispatches_by_field_kind`,
`current_plan_delegates_to_edit_state_plan`, `open_edit_on_empty_view_is_a_noop`,
`close_edit_discards_the_pending_buffer`,
`commit_edit_round_trip_on_modified_scalar`).
`cargo test --workspace` **green, 0 failures** (335 total at M8.2 close, up
from 326). `./verify.sh` **350 verified / 0 errors** (unchanged — M8.2 is
purely shell). A pty smoke (`/tmp/opencode/edit_smoke.py`) drives the
release binary through Browse → `e` (→ `Edit: title`) → `↓↓` (→ `tags`) →
type `llm` (→ live `append 'llm' to tags` pending plan + corpus
suggestions `mTLS` / `transformer`) → `Enter` (→ **green** `committed …
tags`) → `Esc` → `q`, and the child **exits 0** with the terminal
restored; the *on-disk* page gains the `- llm` tag **and** the DB's
`page_tags` gains the `llm` row (the file→DB "source of truth" contract
verified end-to-end on a real registered root). `kb tui` on a non-TTY
still soft-fails with "could not enable raw mode" and exit 1 (M8.0
contract, unchanged).

### 13o. M8.3 results — `kb tui` alias + roots manager (built + tested)

**Scope.** M8.3 closes the last TUI binding: the **alias + roots** manager
over the live tree. The user can `a` open the tag-alias map to
**list / add (upsert) / remove** rows, and `r` open the `roots` registry to
**list / add / drop** registered roots. Both paths **reuse the exact
`Db` methods and SQL the shell already owns** — the add path is the same
`ON CONFLICT(alias) DO UPDATE` as `cmd_tag_alias --add`, and the roots add/
drop call the *same* `Db::add_root` / `Db::drop_root` that `cmd_add` /
`cmd_drop_root` call (followed by the *same* `kb-db::refresh::refresh`
GC-orphan pass). The TUI owns only the *composition* (which rows, in what
order, and the "form vs. list" focus contract); it adds **no new proven
code**, so the `./verify.sh` **350** count is held, not extended.

**Design.** The M8.2 `App` grows two modes (still all in `mod.rs`):

- `Mode::Alias(AliasState)` — `AliasState { list: Vec<AliasRow>, input,
  selected, focused }`. `AliasRow { alias, canonical }` is one `tag_aliases`
  row. `input` is the **add-form buffer**: the user types
  `<alias> <canonical>` (whitespace-separated), and `Enter` parses it.
- `Mode::Roots(RootsState)` — `RootsState { list: Vec<RootsRow>, input,
  selected, focused }`. `RootsRow { path, extra_exts, include_hidden,
  added_at }` is one `roots` row. `input` is the **path buffer**.

  **The "form vs. list" contract (shared by both managers):**
  `focused == false` = the *add form* is active (type into `input`;
  `Enter` submits an upsert / registers a root). `focused == true` = the
  *list* is active (`↑`/`↓`/`j`/`k` move the selection; `x` removes /
  drops the selected row). `Tab` toggles `focused`. This is the one place
  a single `Tab` key switches sub-panels, so it is the load-bearing bit.

**Bindings (M8.3).** In Browse *and* Read, `a` → `open_alias`, `r` →
`open_roots` (the two "reserved M8.3" placeholders in §13l/§13n are now
wired). In `Alias` / `Roots`: printable chars type into `input` (form
mode only); `Backspace` pops; `Tab` toggles form ⇄ list; `↑`/`↓`/`j`/`k`
move the list selection (list mode, clamped, no-op on an empty list);
`Enter` submits the add form; `x` removes / drops the selected list row;
`Esc` → `close_alias` / `close_roots` (back to Browse, **selection
preserved**); `q` / `C-c` quit (globally, the M8.2 convention).

**The add/remove pipeline (one path each, reusing the shell's writes).**
- `add_alias`: `parse_alias_form` (a *pure* `&str → Option<(alias,
canonical)>`, the single predicate render + commit agree on) → `upsert`
  into `tag_aliases` → `reload_aliases` → clear the form → green
  `StatusKind::Success`. A malformed form is a clean no-op (buffer
  preserved, red bar) so the "type, correct, re-Enter" UX never loses work.
- `remove_alias`: delete the *selected* row (`WHERE alias = ?`) →
  `reload_aliases` → green bar. Total: empty list / form mode is a no-op.
- `add_root_tui`: trim the path → `canonicalize_dir_for_add` (pure guard:
  must exist **and** be a directory) → `Db::add_root` → `kb-db::refresh`
  (indexes any new pages, GCs orphans) → `reload_roots` + `reload_rows`.
- `drop_root_tui`: `Db::drop_root` → `kb-db::refresh` (GCs pages no longer
  under any root) → `reload_roots` + `reload_rows`.
- `close_alias` / `close_roots` are the "back" — plain `Mode::Browse`,
  never touching the DB; the snapshot is re-loaded on the next open (total).

**Tests (all green).** 9 new unit tests in `kb/src/tui/mod.rs`
(`parse_alias_form_is_pure_and_total` — the form parser is total +
splits-on-first-whitespace + trims; `open_alias_is_total_on_empty_db`;
`add_alias_upsert_round_trip` — last-wins upsert via a real in-memory DB;
`add_alias_malformed_is_a_clean_noop`; `remove_alias_round_trip_and_total`;
`alias_move_is_bounded_and_focus_toggles` — selection is total + bounded
and `Tab` cleanly flips form ⇄ list; `canonicalize_dir_for_add_is_total` —
the pure guard rejects missing paths and file-not-directory;
`roots_add_then_drop_round_trip` — register a real dir + a page under it,
then drop + GC; `add_root_tui_empty_form_is_a_noop`), plus a bug-fix for a
pre-existing `mut` warning and the `bool::MAX` slip. `cargo test
--workspace` **green, 0 failures** (344 total at M8.3 close, up from 335
at M8.2). `./verify.sh` **350 verified / 0 errors** (unchanged — M8.3 is
purely shell). A pty smoke (`/tmp/opencode/alias_roots_smoke.py`) drives
the release binary through Browse → `a` → type `newalias NEWCANON` →
`Enter` (→ the row appears) → `Tab` → `j` → `x` (→ the seeded alias is
removed) → `Esc` → `r` → type a brand-new dir path → `Enter` (→ root
registered + its page indexed) → `q`, then **asserts the DB itself** via
the plain CLI: `tag-alias --list` shows `newalias→NEWCANON` and no longer
shows `seedalias`, `roots` contains the new dir, and `browse` contains the
new root's page. **5/5 runs pass**; the child exits 0 with the terminal
restored.

**M8 is now complete.** Every `kb tui` binding lands — Browse, Read, Edit,
Alias, Roots — over the same `Db` / `kb-core` / `kb-db` every CLI verb
 uses. The shell (ratatui/crossterm) remains *assumed*; every pure TUI
 invariant (selection is total + bounded at the edges) is pinned by tests.

 ### 13p. M9 results — the agent-facing surface (built + tested)

 **Scope.** The agent drives `kb` as a plain CLI (not the TUI), so three
 things must hold and are now *pinned* rather than assumed:

 * **Part A — bypass.** Every TUI feature has an equivalent plain-CLI verb,
   so an agent never has to "click":

   | TUI feature (M8)         | plain-CLI bypass (M0–M8)                          |
   |--------------------------|---------------------------------------------------|
   | Browse (list pages)      | `browse [path]`                                   |
   | Read (open a page)       | `show <file|id> [--body/--full]`                  |
   | Filter (`/` substring)   | `search <query>` (FTS + `type:`/`tag:` cols)      |
   | Edit (frontmatter ops)   | `section set/append/insert … fm.<key>`, `replace` |
   | Alias (manage map)       | `tag-alias --add/--rm/--list/--export/--import`   |
   | Roots (manage registry)  | `add <path>`, `roots`, `drop-root`                |
   | (no TUI analogue)        | `cron` (paste-ready refresh line)                 |

   **Part B — `--json` everywhere.** Every verb that returns data *or
   mutates state* now accepts `--json`, and on that path stdout carries
   **exactly one JSON document** and nothing else.

 * **Part C — `--db`.** The global `--db PATH` flag (default
   `~/.local/share/knowledgebase/kb.db`) is honored by *every* command,
   including the write path (`section` / `replace`), so an agent can run
   against a throwaway DB with no `~` state.

 **The JSON contract.** One top-level value per command, nothing else on
 stdout, the rest of the surface kept identical:

 * **list verbs** → a JSON **array** `[ … ]`: `roots`, `browse`, `tag`,
   `search`, `tag-alias --list`. Empty result is a clean `[]`, never a
   human sentence.
 * **record verbs** → a JSON **object** `{ … }`: `info`, `links`, `stats`,
   `add`, `drop-root`, `refresh`, `full-rebuild`, `resolve`,
   `tag-alias --add` / `--rm` / `--import`, and every write op
   (`section …`, `replace`).
 * **`--export` stays TSV** — it is the *recovery codec* (the byte↔row
   format `--import` reads back), not a display surface.
 * **Errors** go to **stderr** through the existing `Kb =
   Result<i32,(String,i32)>` contract; exit codes are unchanged (0/1/2/3).
 * **No dependency.** JSON is hand-rolled (the same `jstr` / `jstr_opt`
   helpers the M6/M7 `search --json` / `toc --json` paths already used) plus
   a new `jbool` and a `PageJson` renderer — `serde` is **not** added.

 **Write path (`section` / `replace`).** These were the only verbs with no
 `--json` and an interactive confirm prompt. `commit(…)` now takes a
 `json` flag: the unified `diff` + "applied"/"no changes" notes are *routed
 to stderr* so stdout stays one JSON document, and `confirm` is forced
 (a JSON result is non-interactive — write happens, or `--dry-run` holds
 it). The result object is uniform across every op:
 `{ file, changed, dry_run, applied, bytes_before, bytes_after, diff }`.
 `section edit --json` is a **clean reject** (exit `EXIT_SECTION`, error
 names the flag) because `--edit` is an editor flow, not a JSON one.

 **Tests (all green).** 12 new integration tests in
 `kb/tests/m9_agent_surface.rs` (binary-as-subprocess over a throwaway root
 + `--db`, mirroring `m7_multifile.rs`). Each test pipes the verb's stdout
 through `python3 -c 'import json,sys; ...'` to assert it parses as a valid
 JSON document **and** is the right top-level kind (array vs object) **and**
 carries the keys an agent needs: `part_c_db_flag_is_honored`,
 `roots_json_roundtrip`, `add_droproot_refresh_json`, `browse_tag_json`
 (incl. the empty-`[]` tag fallthrough + empty `tag-alias --list`),
 `tag_alias_json_roundtrip` (add/list/rm), `resolve_info_links_json`
 (unique + empty outcomes, full `info` key set, `links` page/forward/
 backlinks), `stats_json`, `cron_is_pure_cli_bypass`, `section_op
_json_roundtrip` (append + `--dry-run` non-write), `replace_op_json_roundtrip`
 (count-capped + all), `section_edit_json_rejects_gracefully`, and
 `search_json_still_works` (regression: the pre-existing `search --json`).
 `cargo test --workspace` **green, 0 failures** (356 total at M9 close, up
 from 344 at M8). `./verify.sh` **350 verified / 0 errors** (unchanged — M9
 is shell, no new `kb-core` proofs).

 **M9 is now complete.** Every TUI feature has a plain-CLI bypass, every
 CLI verb is `--json`-able with a single well-formed document on stdout,
 and `--db` is honored across the read *and* write surface — so an agent
 (or `jq`, `curl | kb`, a script) can drive the whole of `kb` without a
 terminal.

 ### 13q. M9.5 results — `kb patch` (the parked item, built + tested)

 **Scope.** §13f parked one single-file verb with the infrastructure already
 built: *"kb patch (import an external unified diff). The parser + applier
 are already built and pinned — a command to surface them is ~20 lines of
 `main.rs`."* This milestone does exactly that — no new pure core, no new
 proof obligation: `diff.rs::parse_unified` + `diff.rs::apply_patch` are
 unchanged (and their unit tests already prove standard-`diff -u`
 conformance by cross-applying `testdata/standard.diff` with GNU `patch(1)`
 and by round-tripping `unified_diff(x, y)` → `apply`). What lands is the
 *shell* plumbing and its contract surface.

 **Surface.** `Cmd::Patch` (added next to `Cmd::Replace` in the single-file
 namespace — §11). One required positional `FILE` plus a 3-way patch
 *source*:
 - `--patch-file [PATH|-]` — read the patch from a file; `-` = stdin. This
   is the "I have a `foo.patch` on disk" pathway, including the
   round-trip `kb replace ... --dry-run > foo.patch ; kb patch --patch-file foo.patch` (the
   *generate* → *import* loop through the standard `diff -u` codec).
 - `--patch-stdin` — read the patch from stdin (for `... | kb patch --patch-stdin <file>`).
 - `--patch-text "…"` — inline literal patch. Clap `allow_hyphen_values`
   is set because a patch starts with `−−− `; without the flag clap
   would read the first `−−−` as a flag.

 Exactly one of the three must be provided (0 or 2+ is a clean exit-1,
 name-of-the-fix — `one of --patch-file / --patch-stdin / --patch-text is
  required (got N)`). The write contract is the *same* `WriteFlags`
 (`--dry-run`, `--yes`, `--if-unchanged`, `--diff`, `--json`) and the
 *same* `commit()` call as `section …` / `replace`: atomic write, hash
 guard between the read and the write, uniform JSON envelope
 `{file, changed, dry_run, applied, bytes_before, bytes_after, diff}`,
 the "apply?" prompt suppressed under `--json`.

 **Tests.** 8 new integration tests in `kb/tests/section_cli.rs` (added to
 the existing M6 suite, not a new file — the verb belongs to M6):
 1. `patch_applies_a_real_external_diff_byte_identical` — apply
    `testdata/standard.diff` to a copy of `testdata/section_old.md` and assert
    the result byte-matches `testdata/section_new.md`.
 2. `patch_via_stdin_applies` — same fixture through `--patch-stdin` via
    a piped child process.
 3. `patch_via_inline_text_applies` — same fixture through `--patch-text`
    (this is also the test that exercises `allow_hyphen_values`).
 4. `patch_dry_run_prints_diff_but_writes_nothing` — `--dry-run` emits
    `@@` on stdout and leaves the file's bytes unchanged.
 5. `patch_json_is_well_formed_envelope` — the `--json` envelope carries
    every key the M9 agent contract demands, and reports `applied: true`.
 6. `patch_no_source_is_exit_1` — no source at all is a clean exit-1.
 7. `patch_two_sources_is_exit_1` — two sources (`--patch-file` +
    `--patch-text`) is a clean exit-1.
 8. `patch_identity_is_a_noop_changeless` — a hand-crafted all-context
    patch that changes nothing is a no-op through `commit()` (exit 0,
    "no changes", file bytes unchanged).

 **Gate.** `cargo test --workspace` **green, 0 failures** (368 total,
 up from 360 at M9 — the 8 `patch_*` tests; the M9 fidelity test was
 already counted in M9's close).
 `cargo build --release` still 0 warnings / 0 errors. `./verify.sh`
 unchanged (no new `kb-core` proofs — `diff.rs` is unchanged this
 milestone). `--json` surface sweep 25/25 → **26/26** (the verb is
 now listed; its `--json` output parses as the same uniform envelope the
 other write verbs produce).

 **M9.5 is now complete.** The single-file agent surface now closes the
 generate → *import* loop: every write verb already emits standard
 `diff -u` (via `--dry-run` or under `--diff`), and `kb patch` consumes
 a standard `diff -u` byte-identically. The round-trip
 `kb patch --patch-file <(kb replace … --dry-run)` therefore composes
 with any external patch producer (`diff -u`, `git diff`, a LLM's
 patch, `sed -n 'l' … | diff -u`). §13f's last parked single-file item
 is now resolved; the next parked single-file item is `--out` (write
 elsewhere than the source), which is explicitly marked as not a
 v1-contract change.


