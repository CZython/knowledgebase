# `kb` — a Rust knowledge-base tool (search, browse, tag, link, **and** edit)

## Contents

Source-comment cross-refs throughout the workspace cite these numbers (e.g. `PLAN §9`, `PLAN §10`); this table is the one place to resolve any of them. The **§9** proven-vs-assumed split and the **§10** workspace layout are the load-bearing ones — the comments that gate "is this line verified or assumed?" point at them. Per-feature behavior (links, page verification) lives in the dedicated `FEATURES-*.md` docs at the repo root.

| §        | Title |
|----------|-------|
| [1](#1-what-it-is) | What it is |
| [2](#2-guiding-principles) | Guiding principles |
| [3](#3-locked-decisions) | Locked decisions |
| [4](#4-page--identity-model) | Page & identity model |
| [5](#5-search-model) | Search model |
| [6](#6-tags-and-labels-two-separate-ideas) | Tags and Labels (two separate ideas) |
| [7](#7-section-editing-full-mdtoolpy-port) | Section editing (full `mdtool.py` port) |
| [8](#8-database-schema-turso--purerust-sqlite-compatible-engine) | Database schema (Turso — pure-Rust SQLite-compatible engine) |
| [9](#9-verification-verus-proven-vs-assumed) | Verification (Verus: proven vs. assumed) |
| [10](#10-workspace-layout) | Workspace layout |
| [11](#11-cli-reference-flat-kb-cmd---db-path-defaults-to-localsharekbkbdb) | CLI reference (flat `kb <cmd>`) |
| [A](#appendix-a--example-note-filesfrontmatter--body) | Appendix A — example note files |


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
   `include_hidden` walk options — §3 Extensions/Walk**) + `tag_aliases` (the DB-level tag alias map, §3) + the small
   `meta` runtime-table (e.g. `last_refresh_at`, §8/§11 `kb stats`) + the bookkeeping columns
   `pages.created_at`/`pages.updated_at` (see §8). All of these are re-registerable/re-addable
   at worst.
2. **The DB is a rebuildable cache + query accelerator.** It exists so search/browse/link run in
   milliseconds over millions of files. It is never the source of truth.
 3. **No external service, no async runtime.** An embedded, single-file, pure-Rust
    SQLite-compatible engine — [Turso](https://turso.tech) — with a **synchronous** API.
    One file. Multiple clients. No server, no API, **no tokio / async**. Chosen precisely
    because the driver surface is sync (the engine compiles in — the build still needs a
    C compiler for its C dependencies, e.g. `zstd` via Turso's Tantivy) and its FTS layer
    (Tantivy-backed, exposed through `fts_match` / `fts_score` / `fts_highlight`) pushes the
    design work onto the engine layer rather than forcing the verified `kb-core` to know
    SQLite's FTS5 SQL.
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
| DB engine | **Turso** (pure-Rust, SQLite-compatible) + its **Tantivy-backed FTS** — embedded, single file, multi-client, no service, **sync API**. (Build pulls C dependencies, e.g. `zstd` via Turso's Tantivy — so a C compiler is still required, but no system SQLite.) |
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
| Verus scope | **Prove** core + query-grammar/semantics + **read-only fs-walk** + section splice. **Assume** the DB engine (`kb-db`) + SQL execution + real `std::fs` reads + CLI shaping. |

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

### SQL shape (Turso FTS)

Turso's FTS is **function-based**: there is no `pages_fts` external-content
table, no `MATCH` operator, and no `snippet()`/`bm25()` — the engine
maintains a Tantivy index over the listed columns and exposes it through the
scalar functions `fts_match(cols, q)`, `fts_score(cols, q)`, and
`fts_highlight(cols, q, …)`. A positive FTS query is rendered by
`kb-core::search_grammar` (`render_fts`) and passed as a **parameter**;
exclusions (`-term`) are appended as ` <NOT term>` in that same string, and a
**NOT-only** query (no positive term, which has no positive FTS string to rank
against) instead becomes one correlated `NOT EXISTS … fts_match(cols, <NOT
term>)` predicate per excluded term — the form Turso's planner accepts.

```sql
SELECT p.id, p.abs_path, p.root, p.rel_path, p.title, p.description,
        fts_score(title, description, body, frontmatter_text, ?q) AS score,  -- ?q = render_fts(parsed); the query is a parameter, not an operator
        fts_highlight(title, description, body, frontmatter_text, ?q, '<mark>', '</mark>') AS snippet,
FROM pages p
WHERE fts_match(title, description, body, frontmatter_text, ?q)             -- the positive FTS clause (absent on a NOT-only query)
  AND <NOT EXISTS … fts_match(cols, <NOT term>) per -term>                  -- exclusions; a lone NOT-only query is *only* this set
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

**FTS index = `title`, `description`, `body`, and flattened frontmatter scalars** (`description` is optional, ≤255 chars, §3; NULL columns are simply unindexed). The index is the Turso FTS index built over these columns; `fts_match` / `fts_score` / `fts_highlight` query it.

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

## 8. Database schema (Turso — pure-Rust SQLite-compatible engine)

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
CREATE INDEX idx_pages_id  ON pages(id);      -- display id for `[[wikilink]]`/resolve; NOT unique (collisions are legal, §4)

-- gate entries need per-entry matching (whole-phrase), so a row per entry:
CREATE TABLE page_gates (
  abs_path TEXT NOT NULL,       -- FK → pages.abs_path (the page key)
  term  TEXT NOT NULL,          -- exact entry as written, minus surrounding double quotes
  PRIMARY KEY(abs_path, term)
);

-- Full-text search is a Turso native FTS index over the `pages` columns
-- (title, description, body, frontmatter_text). The engine keeps it in
-- lockstep with `pages`, so the indexer only ever writes `pages` — there is
-- no external-content table and no sync triggers. No `CREATE VIRTUAL TABLE`
-- statement; the index is a Turso engine object, queried via the
-- fts_match / fts_score / fts_highlight functions.

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

**Assumed (tested, not proven):** the `kb-db` engine + SQL execution; snapshot *builder* (real
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
├── kb-db/                        # assumed-spec (Turso + its Tantivy-backed FTS; sync API)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── conn.rs               # open default/override path, migrate (idempotent DDL)
    │       ├── walk_snapshot.rs      # real std::fs walk → immutable snapshot (IO); skip rules per §3 Walk (no symlinks, dot-dir skip w/ --include-hidden, extension allow-list)
    │       ├── index.rs              # upsert pages/page_tags/page_gates/links; skip (warn) `.md` with no `type:` (per §4); tag_aliases is user-managed, not derived from disk
│       ├── refresh.rs            # incremental (mtime_ns,hash) diff + full-rebuild
 │       └── query.rs              # SQL build: filters + FTS (fts_match) + gate; run + shape rows
├── kb/                           # shared CLI source body (NOT a package — see below)
│   ├── main.rs                   # clap dispatch (one real body, `include!`d twice)
│   ├── diff.rs                   # unified-diff renderer + parser
 │   ├── tui/                      # the ratatui shell — gated behind the `tui` feature
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
    # scaffold a new `.md` page — full frontmatter frame (`type:` `title:` `description:` `tags:` `search_required_phrase:`) + matching H1 — then index it with the *same* `build_page`/`upsert_page` pair `refresh` uses, so the row reconciles byte-identically later. `.md` is appended when `<name>` has no extension; `--path` is the target directory (created if missing) and only a `.md` target is accepted (§3 Extensions); an existing file is never clobbered. `-e/--edit` opens the *written* page in `$VISUAL`/`$EDITOR` (fallback `vi`) and indexes the saved bytes — a non-zero editor exit leaves the scaffold on disk, unindexed. Indexing is skipped **with a stderr note, never an error** when no roots are registered, when the page is outside every registered root, or when the final bytes aren't indexable (the §3/§4 no-`type:` warning contract). `-e --json` is rejected (an editor flow is not JSON), matching `section edit --json`.
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

### Single-file (agent read/write surface, pure byte ops over `Vec<u8>`)

The read surface is **scoped + minimal**
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
   (case-insensitive) unique exact / substring title.
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

