# `kb verify <FILE>` — page lint + link check

A single, scriptable command that answers — for one supplied file/path —
*is this a valid, well-formed kb page, are all its links resolvable, and (if
it's a `type: label`) does it still name real files?* — so a user or agent can
decide, in one shot, whether a page is "done" before committing or trusting
it. It reads one target file (not the corpus at large), reports a full list
of problems in one pass, and returns a meaningful exit code.

## Command

```
kb verify <FILE> [--db PATH] [--strict] [--json]
```

- `<FILE>` — one path. A directory is *not* a valid target (use `kb verify`
  per file); passing one is a clean `EX_USAGE` with a suggestion.
- `--db PATH` — the same global `--db` as every other verb (corpus for link +
  label resolution). Absent → default (~/.local/share/knowledgebase/kb.db).
- `--strict` — treat *warnings* as fatal (any `warning` escalates exit
  `ok-with-warnings` → `1`). Default off.
- `--json` — emits exactly one JSON document on stdout (the same `--json`
  contract as every read-only verb); human noise is routed to stderr.

### Exit codes (the load-bearing part for agents)

| Verdict string | Meaning | `0` | `1` | `--strict →1` |
|---|---|---|---|---|
| `ok` | all checks clean | ✔ | | ✔ |
| `ok-with-warnings` | ≥1 warning (dead/ambiguous link, missing label file, description-too-long) | ✔ | | ✔ |
| `error` | ≥1 required failure (non-`.md`, no/unknown `type:`, no slug, broken framing) | | ✔ | ✔ |

`2` is a usage error (no arg, directory, unreadable file). `--strict` only
*upgrades* `ok-with-warnings` → non-zero; it never creates new findings.

## The checks (a → e), in order

Each check is independent and reported; a failure in one does **not** stop
the others (we want the full list of problems in one pass). All are pure
over the file text + the corpus.

### a. Is it a Markdown file
`.md` / `.markdown` / `.mdx` (case-insensitive). A non-`.md` file is itself a
finding: `verdict.error` + a warning (the corpus only indexes `.md` + per-root
`--ext` add-ons, so a non-`.md` file has *no page to resolve against* by
design). Unreadable / no such file → `EX_USAGE` (2).

### b. Is it a *well-formed* kb page (frontmatter framing)
Grounded in `kb-core::frontmatter::parse` (the file must open with a `---`
line; the block ends at the next `---`):
- **Framing:** line 1 is `---`, and a matching closing `---` exists. No
  opening `---` → `no-frontmatter` (body = whole file, no `type:`). No closing
  `---` → `unterminated-frontmatter`.
- The block is *parseable* (the parser is lenient — it never throws; it
  records problems as *fields*); `kb verify` reads those fields and reports
  them.

### c. Are the **required** frontmatter fields present / valid
Grounded in `kb-core::meta` (`type:` is **required** to index; slug must be
resolvable per `identity::resolve_slug`). Report each as a finding:
- **`type:` present** — required. Missing → `missing-type` (the indexer skips
  + warns today; here it's an explicit finding with the skip consequence).
- **`type:` is a known kind** — `label` | `wiki` | `note`
  (`meta::NoteType::parse`). Unknown → `unknown-type` (raw value surfaced so
  a typo like `labbel` is self-evident).
- **A slug resolves** — `identity::resolve_slug(title, H1, filename)` is
  never empty. `verify` reports which candidate won (title / H1 / filename)
  so an agent knows *why* the page has its id.
- **`description:` ≤ 255** — optional, but if present and over the limit →
  `description-too-long`. Optional fields that are absent are **not** findings.

### d. Are all the page's links resolved? (dead / broken links)
Grounded in `kb::main::annotated_links` over the page's body + the loaded
corpus (the **same** function `kb links` and the TUI picker use — so `verify`
and the picker can never disagree). For every `AnnotatedLink`:
- **`resolves_to = Some`** → healthy; count it.
- **`resolves_to = None` + `candidates` empty** → **dead link** (no page
  matches any waterfall stage). Reported with position (`line`, `col`), the
  raw `target`, the `style`, and a suggestion when `candidates` is non-empty
  (an *almost-match*).
- **`resolves_to = None` + `candidates > 1`** → **ambiguous, not dead** (a
  collision). Distinct finding — the author must disambiguate with a
  `type:slug` or a more specific slug. *Not* counted as dead.
- **external `<url>`** → informational (never resolved by design); shown, not
  flagged.
- **current (`#section` on the page itself)** → counted, never dead.

### e. `type: label` — what does it point at?
Grounded in `kb-core::labels::classify(body)`. For a `label` page, report:
- **`## files` block** (case 1, wins) → the listed refs, each with line
  numbers, and a per-ref **exists-on-disk** check (the pure classifier keeps
  the refs; the *existence* check is IO and lives in the shell). Missing
  files are reported with their relative path.
- **leading contiguous path run** (case 2) → same treatment.
- **folder-wide** (case 3) → the label's own directory + a non-recursive
  enumeration count ("applies to N entries under `<dir>`").
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
      "total": 4, "resolved": 3, "external": 1,
      "dead":      [ { "line": 12, "col": 0, "style": "wikilink", "target": "Transformer arch", "suggestion": "wiki:Transformer (architecture)" } ],
      "ambiguous": [ { "line": 30, "target": "Attention", "candidates": ["/root/wiki/Attention2.md", "/root/wiki/attention.md"] } ]
    },
    "label": { "type": "note", "applies": "n/a" }
  }
}
```

## Reuse map (what `verify` is *not* allowed to reimplement)

| Concern | Reuses | Don't |
|---|---|---|
| frontmatter framing + fields | `kb-core::frontmatter::parse` (lenient, warning-as-fields) | a bespoke `---` scanner |
| `type:` / slug validity | `kb-core::meta::NoteType::parse`, `kb-core::identity::resolve_slug` | a re-derivation of the slug rule |
| link extraction + resolution | `kb::main::annotated_links` + `kb-core::resolve` waterfall (the **same** fn the TUI picker + `kb links` use) | a new extractor — that would let `verify` and the picker disagree |
| label file/folder classification | `kb-core::labels::classify` (the deterministic 3-way) | a new `## files` reader |

Because `verify` is a shell over the already-verified core, it introduces
**zero new Verus obligations**; `./verify.sh` is unchanged; `bash gate.sh`
stays 7/7.

## Non-goals

- Verifying a *set* of pages / the whole corpus in one shot (that's
  `kb refresh --check`, a different verb — `verify` is *one file*, so the
  output stays small and the exit code stays meaningful).
- Fixing anything (`--fix`). `verify` is read-only; it reports, a human/agent
  decides.
- Lint rules beyond the kb page contract (no Prettier-level markdown style).
- Following transitive label→label chains (a label that points at another
  label: report it as a ref, don't recurse).
