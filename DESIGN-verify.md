# `kb verify <FILE>` — page lint + link check

**Design doc.** Goal: a single, scriptable command that answers —
for one supplied file/path — *is this a valid, well-formed kb page, are all
its links resolvable, and (if it's a `type: label`) what does it point at?*
so a user or agent can decide, in one shot, whether a page is "done" before
committing or trusting it. No new proof obligations: every check reuses
already-built `kb-core` primitives (`frontmatter::parse`, `identity`,
`annotated_links` + `resolver::resolve`, `labels::classify`).

## Why

- Today the "is this page OK?" question is answered by *absence of a skip
  warning* during `kb refresh` — which is noisy, corpus-wide, and interleaves
  with indexing. There is no fast, targeted, exit-code-driven check for
  *one* file.
- A page can silently be wrong in several distinct ways (bad framing, no
  `type:`, unknown `type:`, `description` > 255, no slug, dead links) and an
  agent editing one needs a **machine-readable** verdict, not a wall of prose.
- The `label` type has a second meaning (`## files` / list / folder-wide)
  that is only visible once the page is read — `kb verify` surfaces it so a
  proxy note can be sanity-checked for "does this still name real files?".

## Command

```
kb verify <FILE> [--db PATH] [--json]
```

- `<FILE>` — one path. A directory is *not* a valid target in v1 (use
  `kb verify` per file); passing one is a clean `EX_USAGE` with a suggestion.
- `--db PATH` — the same global `--db` as every other verb (corpus for link
  + label resolution). Absent → default (~/.local/share/knowledgebase/kb.db).
- `--json` — emits exactly one JSON document on stdout (part of the same
  `--json` contract as every read-only verb — see M9); human noise is routed
  to stderr. It is the *only* output on stdout under `--json`.
- **Exit codes** (the load-bearing part for agents):
  - `0` — verdict is `ok` (or `ok-with-warnings` when `--strict` is *off*).
  - `1` — verdict is `error` (one or more *required* checks failed).
  - `2` — usage error (no arg, directory, unreadable file).

### `--strict` (optional, default off)

Treat *warnings* as fatal. Without it, `0/0`-with-warning is still `0`; with
it, any `warning` escalates to `1`. Lets a CI gate run `kb verify --strict`
while a human just runs `kb verify`.

## The checks (a → e), in order, short-circuiting into a single verdict

Each check is independent and reported; a failure in one does **not** stop
the others (we want the full list of problems in one pass). All are pure
over the file text + the corpus.

### a. Is it a Markdown file
- Extension `.md` / `.markdown` / `.mdx` (case-insensitive). A non-`.md`
  file is itself a finding: `verdict.error` + a warning
  (the corpus only indexes `.md` + per-root `--ext` add-ons, so a non-`.md`
  file has *no page to resolve against* by design — this flags it explicitly
  rather than letting it fail downstream).
- Unreadable / no such file → `EX_USAGE` (2), not a link-check error.

### b. Is it a *well-formed* kb page (frontmatter framing)
Grounded in `kb-core::frontmatter::parse` (the file must open with a `---`
line; the block ends at the next `---`):
- **Framing:** line 1 is `---`, and a matching closing `---` exists. No
  opening `---` → `no-frontmatter` (the file is *not* a kb page: body = whole
  file, no `type:`). No closing `---` → `unterminated-frontmatter`.
- The frontmatter block is *parseable* (the parser is lenient — it never
  throws; it records problems as *fields*). `kb verify` reads those fields
  and reports them.

### c. Are the **required** frontmatter fields present / valid
Grounded in `kb-core::meta` (`type:` is **required** to index; slug must be
resolvable per `identity::resolve_slug`). Report each as a finding:
- **`type:` present** — required. Missing → `missing-type` (the indexer skips
  + warns today; here it's an explicit finding with the skip consequence).
- **`type:` is a known kind** — `label` | `wiki` | `note`
  (`meta::NoteType::parse`). Unknown → `unknown-type` (raw value surfaced so
  a typo like `labbel` is self-evident).
- **A slug resolves** — `identity::resolve_slug(title, H1, filename)` is
  never empty. The parser drops empties; `verify` reports which candidate won
  (title / H1 / filename) so an agent knows *why* the page has its id.
- **`description:` ≤ 255** — optional, but if present and over the limit
  → `description-too-long` (the `description_too_long` field). Optional
  fields that are absent are **not** findings (they're optional).

### d. Are all the page's links resolved? (dead / broken links)
Grounded in `kb::main::annotated_links` over the page's body + the loaded
corpus (the **same** function `kb links --list` and the TUI picker use — so
`verify` and the picker can never disagree). For every `AnnotatedLink`:
- **`resolves_to = Some`** → healthy; count it.
- **`resolves_to = None` + `candidates` empty** → **dead link** (the §4
  "Empty" outcome: no page matches any waterfall stage). Reported with
  position (`line`, `col`), the raw `target`, the `style`, and a suggestion
  (an *almost-match* if `kb-core::resolve`'s substring stage was close, else
  "no page in the corpus named/prefixing this").
- **`resolves_to = None` + `candidates > 1`** → **ambiguous, not dead** (the §4
  collision). Distinct finding: the author must disambiguate with a
  `type:slug` or a more specific slug. *Not* counted as dead.
- **external `<url>`** → informational (never resolved by design, see
  `DESIGN-links.md`); shown, not flagged.

### e. `type: label` — what does it point at?
Grounded in `kb-core::labels::classify(body)` (the deterministic
files / folder decision, §6). For a `label` page, report:
- **`## files` block** (case 1, wins) → the listed refs, each with line
  numbers, and a per-ref **exists-on-disk** check (the pure classifier keeps
  the refs; the *existence* check is IO and lives here in the shell).
  Missing files are reported with their relative path (a proxy note that
  names a deleted file is a real finding).
- **leading contiguous run** (case 2) → same treatment as case 1.
- **folder-wide** (case 3) → the label's own directory + a non-recursive
  enumeration count (so the agent sees "this applies to N entries under
  `<dir>`").
- Non-`label` pages show `label: n/a (type: wiki|note)`.

## Rendered output

### Human (stderr, default)
```
kb verify note/gpt-4.md
page:  note/gpt-4.md
ok:    markdown (file)
ok:    frontmatter framing (--- … ---)
ok:    type: note (required)
ok:    slug: 'GPT-4' ← frontmatter `title`
ok:    description: present, 64 chars (≤ 255)
ok:    4 links: 3 resolved, 1 external, 0 dead, 0 ambiguous
warn:  dead link L12 [ wikilink ] '[[Transformer arch]]' → no page named 'transformer arch'
       ·  suggestion:  'wiki:Transformer (architecture)'
label: n/a (type: note)

verdict: ok-with-warnings (1 warning)    exit 0
```
`verdict` is the last line; `exit` is the exit code the shell would see — shown
so an interactive user sees the *meaning* of the code, not just a number.

### `--json` (stdout, exactly one document)
```json
{
  "file": "note/gpt-4.md",
  "verdict": "ok-with-warnings",
  "exit": 0,
  "checks": {
    "markdown":     { "pass": true },
    "frontmatter":  { "pass": true, "framing": "ok" },
    "required": {
      "pass": true,
      "type":   { "present": true, "value": "note", "known": true },
      "slug":   { "resolved": true, "source": "title", "value": "GPT-4" },
      "description": { "present": true, "chars": 64, "over_limit": false }
    },
    "links": {
      "total": 4,
      "resolved": 3,
      "external": 1,
      "dead": [
        { "line": 12, "col": 0, "style": "wikilink",
          "target": "Transformer arch",
          "suggestion": "wiki:Transformer (architecture)" }
      ],
      "ambiguous": [
        { "line": 30, "target": "Attention",
          "candidates": ["/root/wiki/Attention2.md", "/root/wiki/attention.md"] }
      ]
    },
    "label": { "type": "note", "applies": "n/a" }
  }
}
```

## Verdict + exit mapping (the contract)

| Verdict string | Meaning | `0` | `1` | `--strict →1` |
|---|---|---|---|---|
| `ok` | all checks clean | ✔ | | ✔ |
| `ok-with-warnings` | ≥1 warning (dead/ambiguous link, missing label file, description-too-long) | ✔ | | ✔ |
| `error` | ≥1 required failure (non-`.md`, no/unknown `type:`, no slug, broken framing) | | ✔ | ✔ |

`--strict` only *upgrades* `ok-with-warnings` to a non-zero exit; it never
creates new findings.

## Reuse map (what `verify` is *not* allowed to reimplement)

| Concern | Reuses | Don't |
|---|---|---|
| frontmatter framing + fields | `kb-core::frontmatter::parse` (the lenient, warning-as-fields parser) | a bespoke `---` scanner |
| `type:` / slug validity | `kb-core::meta::NoteType::parse`, `kb-core::identity::resolve_slug` | a re-derivation of the slug rule |
| link extraction + resolution | `kb::main::annotated_links` + `kb-core::resolve` waterfall (the **same** fn the TUI picker + `kb links --list` use) | a new extractor — that would let `verify` and the picker disagree |
| label file/folder classification | `kb-core::labels::classify` (the deterministic 3-way) | a new `## files` reader |

Because `verify` is **shell** over the already-verified core (the §4 / §6 /
§9 obligations are already proven by `spec/links_spec.rs`,
`spec/labels_spec.rs`, `spec/resolve_spec.rs`, `spec/frontend_spec.rs` — not
re-proven here), it introduces **zero new Verus obligations**; `./verify.sh`
is unchanged; `bash gate.sh` must stay 7/7.

## Phase split (status: **DONE**)

**Phase 1 — checks a + b + c, `--json` + human render + exit codes.**
Pure read of one file (a/b/c are self-contained to the file text +
`meta`/`identity`; `type:` is a fixed three-value enum, so no DB needed).
**DONE — M10.1, gate-green, committed.**

**Phase 2 — check d (links) + e (label).**
Loads the corpus (`load_corpus`), runs `annotated_links` + `labels::classify`;
the per-label-file **exists-on-disk** + folder enumeration are the only IO the
whole command adds. Same shell layer, no core change. **DONE — M10.1,
gate-green, committed with Phase 1** (the `--db` arg from Phase 1 now feeds
the corpus load).

## Non-goals (this design)

- Verifying a *set* of pages / the whole corpus in one shot (that's
  `kb refresh --check`, a different verb — `verify` is *one file*, deliberate
  so the output stays small and the exit code stays meaningful).
- Fixing anything (`--fix`). `verify` is read-only; it reports, a human/agent
  decides.
- Lint rules beyond the kb page contract (no Prettier-level markdown style).
- Following transitive label→label chains (a label that points at another
  label: report it as a ref, don't recurse).

## Open / settled

- **Settled**: the command shape, the a→e check list, the verdict + exit
  contract, `--strict`, `--json` envelope, the reuse map, the phase split.
- **Open**: the *almost-match suggestion* for a dead `wikilink` (substring
  distance threshold? or just "no page with this slug"?). Default: **no**
  suggestion unless `candidates` is non-empty (i.e. only ambiguous links get
  a candidate list; plain-dead links get a flat "no page named …").