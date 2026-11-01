# `kb` — general codebase review

Date: 2026-10-24 (re‑reviewed 2026‑11‑01) · Branch: `kb-threat-verification`
Scope: everything *beyond* the hardened-variant pentest (see
`PENTEST_HARDENED.md`). Concerns: oversized functions, hidden
`panic`/`unwrap`/index panics on the agent path, duplicated logic, error /
JSON / SQL consistency, and **searchability** (can a newcomer or agent find a
thing without already knowing where it lives?).

File sizes (LOC) *as of the 2026-10-24 review*: `kb/main.rs` **3331** ·
`kb/tui/mod.rs` **4434** · `kb/tui/render.rs` 1308 · `kb/tui/input.rs` 717 ·
`kb/diff.rs` 527 · `kb-core/src/frontmatter.rs` 1481 · `kb-core/src/section.rs` 1144 ·
`kb-core/src/search_grammar.rs` 614 · `kb-db/src/refresh.rs` 654 ·
`kb-db/src/index.rs` 650. The `kb/main.rs` figure is pre‑A1: the split in
finding A1 moved the verb / JSON / diff / edit code into `kb/cli/*`, taking it
down to ~1240 lines — the numbers below are the *original* measurement, kept
for before/after.

Severity legend: **[H]** correctness / data-integrity · **[M]** maintainability
/ consistency (an agent or future dev will trip) · **[L]** hygiene.

---

## A. Large functions

The "CLI shell" (`kb/main.rs` + `kb/tui/*`) is where every non‑trivial
behaviour lives. The verified core (`kb-core`) and `kb-db` are well‑decomposed;
the problem is concentrated in the shell + TUI.

### A1 · `kb/main.rs` is 3,331 lines / 59 fns in one file — [H] **RESOLVED**

**Resolved.** `kb/main.rs` went from 3,331 lines to 1,224; the verb
handlers, JSON emitters, and the diff/edit/path/etag helpers now live in
`kb/cli/{search,links,section,info,tag,create,browse,cmd_add,editor,json,
roots,show,textsrc,writeops}.rs` (15 files), with `kb/main.rs` remaining
the `include!` root that declares the module tree and the clap schema.
The TUI (`kb/tui/mod.rs`) was already split into `palette.rs` / `render.rs`
/ `input.rs` during the M8 series (see `PLAN.md §13l–§13o`).

**Original observation.** `kb/main.rs` is *the* shared body for **both** binaries. 59 functions mix, in a
single file, the clap schema, ~18 verb handlers, SQL builders, the JSON
encoders, the diff glue, the `editor` fork, path / etag / confirm helpers, *and
a ~100‑line `#[cfg(test)]` (lines ~3222‑3331)*. This is the single most
searchability‑damaging fact in the repo:

* To change one verb you scroll past ten unrelated ones.
* `grep -n 'fn cmd_' kb/main.rs` returns a wall of results with no module
  boundaries to anchor on.
* The `#[cfg(test)]` block at the bottom is interleaved‑feeling and is actually
  a separate module (`mod json_tests`; `kb/main.rs:3222`) — easy to miss that it
  is the *only* test in the file.

The TUI has the same shape: `kb/tui/mod.rs` is **4,434 lines** with the `App`
state, all input handling, all mode‑specific logic, and its tests in one file
(longest non‑trivial fns: `plan` ~115, `refresh_edit_state_on_disk` ~113, both
at `kb/tui/mod.rs:293`/`1328`).

**Recommendation.** Split by concern, *keeping the two‑package `include!`
constraint* (the body must stay includable). Concretely:

```
kb/
  cli/            # new crate-internal module tree (still `include!`d)
    mod.rs        # pub mod verbs;  pub mod identity...
    verbs/search.rs   cmd_search     (kb/main.rs:1147 … now ~220 LOC)
    verbs/links.rs    cmd_links     (kb/main.rs:1961 … 187+ LOC)
    verbs/section.rs  cmd_section/replace/patch/commit
    verbs/info.rs     cmd_info/stats/roots/drop_root
    verbs/tag.rs      cmd_tag/tag_alias/read_all_aliases
    verbs/create.rs   cmd_create / CreateReport / to_json
    json.rs           jstr/jstr_opt/PageJson/CreateReport::to_json (single owner)
    textsrc.rs        TextSource / read_text / patch sources
    editor.rs         run_editor/pick_editor/edit_in_place (already cfg‑gated)
    dispatch.rs       the main() bare-string rewrite  (F4 in the pentest)
  diff.rs         unchanged
  tui/            unchanged, but split mod.rs → state.rs/input.rs/render already
```

`kb/main.rs` then becomes the thin `include!` root that declares
`mod cli; mod diff; #[cfg(feature="tui")] mod tui;` + the clap schema. That is
a **searchability** win (each verb is now a 1:1 grep‑able file) *and* a
cognitive‑load win with zero behaviour change. This is the highest‑leverage
single refactor in the repo.

### A2 · Individual oversized functions in the shell — [M] **RESOLVED**

**Resolved** as a consequence of the A1 split: the verb handlers now live in
their own files under `kb/cli/`, so each of the previously oversized fns is
one file's worth of context rather than a scroll-past-ten-unrelated-ones
exercise. The specific line counts in the table below predate the split.

_Pre‑split worst offenders (line counts from a brace‑depth scan; these are
single fns):_

| LOC | fn | location |
|----:|----|----|
| 230 | `CreateReport::to_json` | `kb/main.rs:985` |
| **223** | `cmd_search` | `kb/main.rs:1147` |
| 187 | `cmd_links` | `kb/main.rs:1961` |
| 182 | `load_corpus`→ (next boundary) | `kb/main.rs:2143` |
| 123 | `read_all_aliases` | `kb/main.rs:1629` |
| 119 | `cmd_replace` | `kb/main.rs:2837` |
| 118 | `cmd_tag` | `kb/main.rs:1511` |
| 134 | `apply_key` (core) | `kb-core/src/frontmatter.rs:136` |

_These all shared the same shape pre-split: do a query, then hand‑format a
giant `format!`/`println!` JSON blob inline. `cmd_search` was the clearest
example — ~90 lines of pure WHERE‑clause construction + ~40 lines of the two
render branches. The split into `kb/cli/*.rs` (A1) is what made each of them
short and independently testable._

### A3 · The `run_editor` / `edit_in_place` pair — [M] **RESOLVED**

**Resolved.** Both public entrypoints now delegate to a single shared body,
`run_editor_in(initial, in_place: Option<&Path>)` (`kb/cli/editor.rs:68`),
which is exactly the shape the finding proposed. The unified launch /
hint / cleanup logic lives in one place; the two failure modes (*launch
failed* vs. *non‑zero exit*) are branched in that one body.

---

## B. Hidden `panic!` / `unwrap()` / index‑panics on non‑test code paths

A "pentest / agent‑facing" read must be able to say *which* runtime lines can
abort *without* a test. Findings (all in non‑test code):

### B1 · `list_rewrite_upgrade::expect(...)` — [M] **RESOLVED**

**Resolved.** The `expect` at the old `frontmatter.rs:830` is now a
`match locate_key_line(data, key) { Some(i) => i, None => return
Err(FmError::NotScalar) }` — a caller-contract panic turned into an
`Err((_, rc 2))` the agent surfaces as a clean failure.

**Original observation.** At `kb-core/src/frontmatter.rs:830-831`:

```rust
let idx = locate_key_line(data, key)
    .expect("scalar_value found a scalar with this key, so the line must exist");
```

The invariant *depends on the caller having called `scalar_value` first*. That
is a classic caller‑contract panic: any future refactor that calls
`list_rewrite_upgrade` directly on a list‑shaped (or absent) key aborts
mid‑write. The module's own doc comment (`frontmatter.rs:445`) claims
"I4 none of the functions call `.unwrap()` / `.expect()`" — this line is a
counter‑example inside the *impl* body. Make it `match … { Some(i)=>…,
None=>err }` and return `Err` so the agent gets a clean `set_scalar`/`append`
failure (rc 2) instead of a stack trace.

### B2 · `lines[idx]` on a `split_inclusive` vec — [L] **RESOLVED**

**Resolved.** The old `.unwrap_or(0)` (the silent-zero trap) is now a
bound-checked guard: `let idx = locate_key_line(data, key).ok_or(FmError::NotScalar)?;
if idx >= lines.len() { return Err(FmError::NotScalar); }` at
`kb-core/src/frontmatter.rs:857-860`. See the inline "B2:" comment right
above it.

_Pre‑fix, `kb-core/src/frontmatter.rs:851‑852` read `let idx =
locate_key_line(data, key).unwrap_or(0); let line = lines[idx].clone();`.
If `locate_key_line` returned an index ≥ `lines.len()` (e.g. an empty document
where the key line had a BOM the split dropped), the second line panicked._

### B3 · `l[1..]` and `l.as_bytes()[0]` in `diff.rs` — [L] **RESOLVED**

**Resolved.** The double index read is now a single `l.split_at(1)` at
`kb/diff.rs:366`, which is exactly the second option the finding proposed. The
arm binds `kind @ (b' ' | b'-' | b'+')` so the prefix is known to be an ASCII
byte, making offset 1 a char boundary; `split_at` peels it off exactly once and
the `Some(_) => break` arm that used to be the panic risk now just stops
parsing.

**Original observation.** Pre‑fix, `kb/diff.rs:364‑366`:

```rust
Some(b' ') | Some(b'-') | Some(b'+') => {
    body.push(PatchLine { kind: l.as_bytes()[0] as char, text: l[1..].to_string() }
```

Safe here (the arm pattern already guarantees non‑empty + valid prefix), but
it is a *second* index read of the same byte — a refactor that relaxes the
pattern to `Some(_) => break` (which already exists at line 369) while a
`Some(b'\t')` slips through would panic. Recommend `let (kind, text) =
match l.as_bytes()[0] { b' ' => (…, …), … }` or a single `l.split_at(1)`.

### B4 · `non_flag[0]` in the bare‑dispatch — [L] **RESOLVED**

**Resolved.** The bare-dispatch now uses
`non_flag.first().expect("len()==1 guarantees non-empty")` at
`kb/main.rs:575-577` — the guard is local to the expression and a reader
can see at a glance why the index is safe.

_Pre‑fix, `kb/main.rs` read `if non_flag.len() == 1 && … { let s =
non_flag[0]; … }` — safe (guarded by `len()==1`), but it was the only place in
the file where an index was used for agent‑facing dispatch and a reader
could not tell at a glance that it was guarded._

### B5 · `run_editor::read(&tmp).unwrap_or_default()` — [M] **RESOLVED**

**Resolved.** The read-back now surfaces failure instead of returning
`[]`: `std::fs::read(t).map_err(|e| (format!("read back edit buffer {}: {e}", t.display()), EXIT_ERR))?`
at `kb/cli/editor.rs:107-110` (the A3 unification point). See the "B5: this
used to be swallowed to `[]`" note in that file's doc comment.

_The original bug was at `kb/main.rs:3074`: it silently returned `0` bytes
when the file read failed after a successful editor exit (pentest F8)._

### B6 · `etag_of` duplicated in two crates — [M, not a panic but a maintenance risk] **RESOLVED**

**Resolved.** One canonical implementation, `kb_core::etag::of`
(`kb-core/src/etag.rs:14`). The two old definitions are now thin delegating
wrappers that call it: `kb-db/src/index.rs:98-99` and `kb/main.rs:1083-1084`.
Drift is impossible — there is only one place the hash is computed.

_Previously `kb/main.rs:3191` and `kb-db/src/index.rs:97` both defined the
same FNV‑1a 64‑bit etag; the db copy even said "identical to the kb CLI's
`etag_of`" (`index.rs:96`). Two copies of a hash were a classic drift trap —
if one was corrected (e.g. `as u64` vs. `as u8` promotion) the other would
silently emit a different 16‑hex for the same byte string, and the
optimistic‑concurrency `--if-unchanged` check would stop matching. There was
also a third copy in `kb-core/src/lib.rs` — see `grep -rn 'cbf2_9ce4'
kb-core kb kb-db` for all three._

### B7 · `unwrap_or(0)` on the `COUNT(*)` query — [L] **RESOLVED**

**Resolved.** The original `fwd_count`/`back_count` sites
(`kb/main.rs:2293, 2299`) already became `?`‑propagating in the A1 split
(`kb/cli/info.rs:41-54` — `cmd_info`). The `cmd_stats` verb — the *same*
belt‑and‑suspenders pattern, on the per‑root page count and the four
total counts — is now `?`‑propagating too, with a distinct context per
query so a `stats` failure names the offending table / root. The two
*legitimate* `unwrap_or(0)` calls (the db file's byte size and the
`COALESCE(MAX)` timestamp that is genuinely `NULL` when no pages exist)
are left as optional.

_The original: `kb/main.rs:2293, 2299` (`fwd_count`/`back_count`).
`COUNT(*)` never errors and never returns NULL, so `unwrap_or(0)` is
belt‑and‑suspenders; it hides a real bug if the query ever *does* fail (the
agent gets `0 out / 0 in` with exit 0 instead of an error). Use `?`‑propagate._

---

## C. Consistency / error model

### C1 · **`Kb = Result<i32, (String, i32)>` is great; the *one* exception is `confirm`** — [L] **RESOLVED**

**Resolved.** `confirm` is now `pub(crate) fn confirm(prompt: &str, assume_yes: bool) -> Result<(), (String, i32)>`
at `kb/main.rs:1032` — it returns `Ok(())` or an `Err((_ , code))` directly,
and callers propagate it with `?`. The five identical re-wrap blocks are
gone.

_Pre‑fix, `confirm` returned a bare `bool` and its `false` branch was
re‑wrapped by every caller (`if !confirm("apply?", assume_yes || json) {
return Err(("aborted (not confirmed)".into(), EXIT_ERR)); }`). That wrapper
block appeared identically in `cmd_section` (via `commit`), `cmd_create`,
`cmd_patch`, and `cmd_replace`; a new verb that forgot the wrap would have
had its "aborted" outcome exit `0` with a silent no‑apply._

### C2 · **No `#[rustfmt::skip]` / `rustc -C` anywhere; no `#![deny(unsafe_code)]`** — [L] **RESOLVED**

**Resolved.** `unsafe_code = "forbid"` is now a workspace lint at
`Cargo.toml:25`, inherited by all three crates via `[lints] workspace =
true`. (Declared as a workspace lint rather than `#![forbid]` in each crate
root so a new crate that joins the workspace is covered automatically; the
`[allow(unsafe_code)]` + reason opt‑out the finding anticipated is already
described in the comment above the lint.) The codebase is unsafe‑free
(verified: `grep -rn 'unsafe' kb/ kb-core kb-db` → none in non‑test source).

### C3 · **JSON emission is hand‑rolled and inconsistent** — [M] **RESOLVED**

**Resolved.** The proposed `json::Object`/mini‑library now exists as
`kb/cli/json.rs` — `JObject` (an ordered `{ "k": v, … }` with a single
`render`) plus a pre-rendered-value escape path. Every string value goes
through one escape owner, empty containers render as `[]`/`{}` consistently,
and the verb handlers build JSON documents as data rather than as
`format!("{{…}}", …)` string templates. This is the other half of the ranked
#1 (A1+C3) pair.

_Pre‑fix, `jstr` (then `kb/main.rs:3204`) was a 19‑line JSON escaper used in
≥9 different fns across ≥12 differently-shaped templates (`commit`,
`cmd_create`, `cmd_search`, `cmd_tag`, `cmd_info`, `cmd_links`,
`CreateReport::to_json`, `cmd_browse`, `cmd_roots`, `cmd_stats`), none
sharing a single "empty array" constant. The inconsistency was visible to the
agent and was a pentest finding (F6, `[{}]` vs. `[]` vs. `[ ]`)._

### C4 · **SQL literal injection in the IN‑list (already escaped, but by hand)** — [L] **RESOLVED**

**Resolved.** The duplicated inline escape is now one helper,
`pub(crate) fn sql_quote_lower(s: &str) -> String` at `kb/main.rs:1091`, used
by both call sites: `kb/cli/search.rs:100` and `kb/cli/tag.rs:43`. The
"did we escape?" question on re‑read is answered by a single lookup.

**Original observation.** Pre‑fix, `kb/main.rs:1260‑1267`:

```rust
.map(|t| format!("'{}'", t.to_lowercase().replace('\'', "''")))
```

Correct for SQLite (doubling the quote is the escape), and both call sites
(`cmd_search` 1262 and `cmd_tag` 1669) do exactly this. But it is in *two*
places with two different variable names (`t.to_lowercase().replace` and the
same). A small `sql_quote_lower(s: &str) -> String` kills the duplication and
the "did we escape?" question on re‑read.

### C5 · **`load_corpus` silently drops `title`→`""` when NULL** — [L] **RESOLVED**

**Resolved** at the read boundary. `load_corpus` (`kb/main.rs:810`) now
surfaces a `pages.title` of `NULL` as a typed error *naming the offending
page* rather than coercing it to `""`. The `resolver::Page` struct keeps its
`slug: String` non‑empty contract — the two alternative fixes proposed in the
original (an `Option<String>` slug in the *core*, or a stage‑2 filter) would
have touched Verus‑proven `kb-core` code for a case the indexer already
forbids: `kb-db/src/index.rs:188` (`resolve_slug … .ok_or(MissingSlug)?`)
*skips* a page at index time when `title → H1 → filestem` all come up
blank, so a row reaching `pages` with `title NULL` is a schema/contract
violation that the shell should report, not paper over.

_Original: `kb/main.rs:2163` —_

```rust
slug: slug.unwrap_or_default(),
```

_A `pages.title` row of `NULL` (allowed in the DDL: `pages.title TEXT` no
`NOT NULL`) became an empty slug in the resolver. Two pages with `title: NULL`
and the same filename both resolved to slug `""` — the resolver's stage‑2 /
stage‑4 exact match was weaker than intended. Use a real `Option<String>` slug
in `resolver::Page`, or `filter`‑out `slug.is_empty()` during the stage‑2 pass
*before* treating it as a match._

### C6 · **Error message shape `"{what}: {e}"` is consistent — good.** — [OK] **CONFORMS**

Verified across `open_db`, `cmd_*`, `refresh_all`, etc. Every `map_err`
follows `format!("<verb>: <e>")`. This is a model of good error
consistency; keep it.

---

## D. Searchability

A newcomer or an LLM agent has to *find* things without already knowing
where they live. Measured: grep‑ability, naming collisions, doc‑comment
coverage, and the "obvious first file" for a concept.

### D1 · `search_required_phrase` — the *same* field name in four places with two spellings — [M] **RESOLVED**

**Resolved** by taking the cross‑linking option (not a rename): both sides now
carry an explicit `D1 note` pointer. On the SQL side, the `SearchRow` field in
`kb/cli/search.rs:18-24` documents that `search_required_phrase` is the
*corpus field* (owned by `kb-core::frontmatter`) and is "not to be confused
with the *TUI palette role* `gated` in `kb/tui/palette.rs`". On the TUI side,
`kb/tui/palette.rs:31-38` and `kb/tui/mod.rs:258-262` carry the matching
`D1 note`: the palette role is this file's name for "a page with a gate," not
the SQL column, the two are "linked by convention only," and the SQL column is
owned by `kb-core::frontmatter`. A search now resolves unambiguously from
either side.

**Original observation.** `grep -rln search_required_phrase` returned files in
four places (`kb/main.rs` as the SQL column + CLI flag name in canonical form;
`kb-core/src/lib.rs`, `kb-core/tests/frontmatter_smoke.rs`,
`kb-core/examples/frontmatter/note.example.md`; and the TUI's role name across
`kb/tui/{palette,render,mod,input}.rs` + `kb/tui/palettes/{default,
colorblind}.toml`). Two different concepts used the same identifier — a
**corpus field** and a **TUI palette‑role / legend label** — so a search for
"change the colour of the search‑required‑phrase label" led the agent to the
TUI files *or* the SQL column depending on which phrase they remembered. The
two options were to rename the TUI role, or to cross‑link both sides.

### D2 · `pages.search_required_phrase` SQL column name appears **in 3+ queries as a literal** — [L] **RESOLVED**

**Resolved.** The magic string is now a single `pub const
COL_SEARCH_REQUIRED_PHRASE: &str` in `kb-db/src/migrate.rs:33`, imported into
the shared body as `GATE_COL` (`kb/main.rs:51`) and used in every query that
references it: `kb/cli/search.rs:128,140`, `kb/cli/browse.rs:35,52`,
`kb/cli/info.rs:21`, and the `kb-db` indexer (`kb-db/src/index.rs:373`). A
schema rename is now a one‑line change to the `const`.

**Original observation.** Pre‑fix, the column name was a literal in
`kb/main.rs:1288‑1290`, `1297‑1299`, `2271`, and `kb-db/src/migrate.rs:46` —
the same string in ≥4 places; a rename meant editing the `SELECT` in
`refresh`, `add_root`, `search`, `info`, `browse`, and `tag`.

### D3 · `cmd_search`, `cmd_browse`, `cmd_tag`, `cmd_tag_alias`, `cmd_links`, `cmd_info`, `cmd_stats`, `cmd_resolve`, `cmd_tag` are all **top‑level** in one file — [M → covered by A1] **RESOLVED**

**Resolved** by the A1 split: each verb handler now lives in its own file —
`kb/cli/search.rs`, `kb/cli/browse.rs`, `kb/cli/tag.rs`, `kb/cli/links.rs`,
`kb/cli/info.rs`, `kb/cli/roots.rs`, `kb/cli/show.rs`, `kb/cli/cmd_add.rs`,
etc. The "which verb does X?" question is answered by the file name.
(The finding's proposed `kb/cli/verbs/*.rs` nesting was flattened during the
split to one level, which is equivalent for this purpose.)

### D4 · The `Kb` type alias is the *one* error abstraction but is only named in one place — [M] **RESOLVED**

**Resolved.** `Kb` now lives in `kb/cli/error.rs` (the CLI's error‑model
module) as `pub(crate) type Kb = Result<i32, (String, i32)>`, re‑exported via
`kb/cli/mod.rs` (`pub(crate) use error::*;`) into the crate through the
existing `use crate::cli::*;`. New verb files find it alongside everything
else the CLI shares (`JObject`, exit codes) — not one specific file deeper in
`kb/main.rs`, which now carries a short comment pointing readers to the
canonical location. We deliberately did **not** move it to `kb‑core`: the
left side of the `Result` (a process exit code) is a CLI concern, not a
pure‑Rust one (`kb-core` is Verus‑proven and keeps `std::process` out of it).

_Original: `kb/main.rs:61` defined the alias and no other file named it.
`grep -rn '-> Kb'` is a complete index of fallible verbs (great!), but a new
contributor adding a fn in a new file (post A1) had no `use` to write because
the alias was file‑local. Promote it to `kb/cli/error.rs` (or `kb_core` if the
core is to own it) so the name travels with the code._

### D5 · `SECURITY_REPORT.md` references (`SECURITY_REPORT 3.1`, `3.3`, `3.4`, `3.5`, `3.6`, `3.7`, `3.16`, `3.17`) — [M] **RESOLVED**

**Resolved** by adding a **findings index (number → summary)** table to
`SECURITY_REPORT.md` under §3 — one place to resolve any `SECURITY_REPORT NN`
cross‑ref, with each row a `[3.17](#317-…)` anchor link to that section. The
~25 call‑site comments keep their inline "why‑this-line" paraphrase *and* the
`SECURITY_REPORT NN` provenance tag, and the number now resolves to a stable
GitHub anchor in both files (the recommendation's "add `#sec‑3‑17`‑style
anchors, or at least a table of contents" option).

**Original observation.** `grep -rn 'SECURITY_REPORT'` returned ~25 call‑site
comments pointing to `SECURITY_REPORT.md` (38 KB, at the repo root). Each call
site carried a numbered reference only meaningful if you opened the separate
file (e.g. `kb/main.rs`, `kb-core/src/frontmatter.rs`, `kb-core/src/aliases.rs`,
`kb-db/src/refresh.rs`, `kb-db/src/walk_snapshot.rs`). That was a lot of
pointer indirection — the number was the fragile part: a section number with no
link, no anchor, no URL, in a hand‑maintained `.md`.

### D6 · `PLAN.md` is 160 KB / §13f / §13p / §9 / §10 — [L] **RESOLVED**

**Resolved** the same way as D5: `PLAN.md` now opens with a **Contents**
table under `## Contents` (one `| § | Title |` table where every row is a
`[13f](#13f-…)` anchor to that section, including the load‑bearing §9 / §10).
A `grep -rn '§13'` cross‑ref now resolves to a stable anchor in both files.

**Original observation.** `grep -rn '§13'` returned ~30 call sites. The
`PLAN §9` / `PLAN §10` references were the load‑bearing ones — they define the
*assumed* vs. *verified* boundary — and should at least carry a link to the
section in question or a 1‑line inline summary.

### D7 · The "which `kb` binary is this?" question is answered by a compile flag — [M] **RESOLVED**

**Resolved.** `pub const VARIANT: &str = if cfg!(feature = "editor") { "user" }
else { "hardened" };` at `kb/main.rs:70` — exactly the shape the finding
proposed. Every call site that needs "am I the agent build?" now does
`VARIANT == "hardened"`; the CLI name (`kb-hardened 0.1.0 [hardened]`) is
derived from it at `kb/main.rs:86-89`.

**Original observation.** Pre‑fix, `kb/main.rs:73‑82` selected the CLI
identity on `cfg!(feature = "editor")`. The two packages are the honest answer
(`kb-user` / `kb-hardened`), but the shared body did not carry the package
name — it carried the binary name, which depends on the caller's feature
choice (see pentest F1/F2).

### D8 · `tests/` is a directory containing only `fixtures/` — [L] **RESOLVED**

The root `tests/` dir is gone: its sole content, `fixtures/`, moved to
`kb-core/testdata/` alongside the package that owns it (D9 below). The
workspace-root `tests/` no longer exists.

```
$ ls tests/
fixtures
```

It is not a workspace member (no `Cargo.toml`), not in `members = […]`, and
not referenced by any `[[test]]` target. The integration tests actually
live in `kb-user/tests/*`. Either move `tests/fixtures/` into
`kb-user/testdata/` (where the diff fixtures *already* live —
`kb/testdata/standard.diff`) or promote `tests/` to a real workspace member
so `cargo test --workspace` includes it. Right now it is a dead directory and
a newcomer will look there first for integration fixtures and find only a
`fixtures/` sub‑folder.

### D9 · `kb/testdata/` vs. `kb-user/testdata/` vs. `tests/fixtures/` — [M] **RESOLVED**

**Resolved** by taking the "one per package (with the root one removed)"
option: `tests/fixtures/` → `kb-core/testdata/` (its only code consumer is
`kb-core/tests/golden_fixtures.rs`, now joined as `.join("testdata")`);
`kb/testdata/` stays as the shared-body diff-parser golden (reached by
`kb/diff.rs` + `kb-user/tests/section_cli.rs`). The root `tests/` dir no
longer exists, and "where's the fixture?" is now a single `grep -rn testdata`
that returns exactly the two package-owned locations.

_Previously `grep -rn 'testdata' kb kb-user kb-core kb-db tests` returned
three different fixture locations. The diff‑parser golden
(`standard.diff`) was in `kb/testdata/` (`kb/diff.rs:491`); the
integration tests looked in `kb/testdata/` by relative path from the package
dir (`kb-user/tests/`); the orphan `tests/fixtures/` had what, then, no
consumer at the workspace root. A single `testdata/` at the repo root, or one
per package (with the root one removed), would make "where's the fixture?" one
grep._

### D10 · `gate.sh`, `verify.sh`, `SECURITY_REPORT.md`, `PLAN.md`, `HARDENED_BUILD.md`, `TODO-palette.md` — [L] **RESOLVED**

**Resolved.** `README.md` now has a **Further reading** table right before the
Contributing section that enumerates *every* root‑level doc (PLAN,
CONTRIBUTING, HARDENED_BUILD, SECURITY_REPORT, PENTEST_HARDENED,
CODEBASE_REVIEW, TODO‑palette, CHANGELOG, LICENSE) plus the two entry‑point
scripts (`gate.sh`, `verify.sh`) — one line each — so a newcomer can pick the
right doc without opening all of them.

The original note: five root‑level `.md` + two `.sh` scripts is normal for a
0.1 project; the ask was only that `README.md` enumerate all of them as
"further reading" with a one‑line purpose each, so a newcomer need not open
each to find out whether it is "how to build", "how to prove", or "the bug
catalogue".

---

## E. Miscellaneous

### E1 · `rust-version = "1.98.1"` in the *workspace* — [L] **CONFORMS**

**Confirmed.** The concern was that `rust-version = "1.98.1"` is an *upper*
bound with no pin to build against. There *is* a pin: `rust-toolchain.toml`
with `channel = "1.98.1"`, which matches `rust-version` exactly — so a
contributor's `rustup` resolves the identical toolchain the ARIA Verus fork
and the bundled‑SQLite compile were verified against (see the file's comment:
deliberately host‑agnostic, no target triple). No `nightly` needed — Verus
runs as a *separate* binary. Marked **CONFORMS** (a "fine, no action" — see E2)
rather than *RESOLVED* because it is a positive confirmation, not a defect
fix; the toolchain pin was already in place.

_Original: `Cargo.toml`. Pinned to a very recent toolchain; that is fine for a
private project, but it is an *upper* bound (a 1.98 toolchain won't build a
`rust-version` of 1.98+1). Confirm `rust-toolchain.toml` matches `1.98.1`
exactly (or use `channel = "nightly-…"` if `vstd` requires nightly — it does
not)._

### E2 · `profile.release.panic = "abort"` — [L] **CONFORMS**
`Cargo.toml:37`. Combined with the codebase's *no‑unsafe* + hand‑rolled
error, this is a good default. Combined with `lto = "fat"` +
`codegen-units = 1` it is a *deliberate* perf/size choice. Good.

### E3 · `.cargo/config.toml` aliases `user` / `hardened` / `kb-user` / `kb-hardened` — [L] **CONFORMS**
`cargo user` works. Documented in `HARDENED_BUILD.md`. Good.

### E4 · The two `kb-user`/`kb-hardened` packages are each a **9‑line
`src/main.rs` + a one‑line `#[command(name="…")]`** — [L] **CONFORMS**

Clean. The *real* code is in `kb/main.rs`. That is the A1/D7 split
already.

### E5 · `vstd = "=0.0.0-2026-09-06-0133"` is an exact pin — [L] **CONFORMS**
`Cargo.toml:53`. Documented as load‑bearing in the verus‑fork
compatibility. Fine. The *comment* explains the why — good.

### E6 · `rusqlite = "0.32"` + `bundled` — [L] **CONFORMS**
`Cargo.toml:58`. A *single* SQLite in the tree. Good. The comment
explains the why.

### E7 · Supply-chain pins (2026‑11‑01) — [OK] **CONFORMS / noted**

Re‑checked `Cargo.lock` on 2026‑11‑01 (cargo‑audit is not installed on this
box, so this is a manual pin‑audit of the load‑bearing deps):

| dep | version | note |
|-----|---------|------|
| `rusqlite` | 0.32.1 | bundled; the one SQLite in the tree (E6) |
| `libsqlite3-sys` | 0.30.1 | the bundled SQLite C source — the only *native* surface |
| `clap` | 4.6.7 | |
| `ratatui` | 0.29.0 | TUI; compiled out of `kb-hardened` |
| `crossterm` | 0.28.1 | |
| `serde` / `serde_derive` | 1.0.229 | |
| `toml` / `toml_edit` / `toml_datetime` | 0.8.23 / 0.22.27 / 0.6.11 | palette deserialization, `deny_unknown_fields` (see below) |
| `dirs` / `dirs-sys` | 5.0.1 / 0.4.1 | `data_local_dir` default DB path |
| `vstd` | `=0.0.0-2026-09-06‑0133` | exact pinned (E5) |

- **Native surface is just `libsqlite3-sys@0.30.1`** (bundled). That is the
  single dep a C‑level CVE would matter in; no other crate compiles native
  code. No `unsafe` in any of our three crates (`workspace.lints` `forbid`,
  C2).
- `toml` deserialization (the one *external‑input* parser we run on file
  content) is **TUI‑only** (`kb/tui/palette.rs`) and is gated
  `#[serde(deny_unknown_fields)]` on the top table, so an unknown key is an
  error, not silently dropped. It is **not** in the `kb-hardened` / agent
  path at all (no TUI there).

_No open CVE found by manual pin‑audit; the only residual is the usual
"re‑run when cargo‑audit is available."_

---

## F. 2026‑11‑01 re‑review (code health vs. the new findings)

Three findings from the 2026‑11‑01 re‑pentest
(`SECURITY_REPORT.md` §3.18–§3.20) have code‑health / searchability
consequences that belong here rather than in the security doc:

### F1 · `needs_quoting` / `render_term` have no test pin for a `*` in the *content* — [M] **RESOLVED**

**Status: RESOLVED (2026-11-01).** A table-driven `render_term_composition_table` test now pins the *exact* rendered string over `plain` → `plain*`, `abc*` → `"abc*"*`, `žltá*` → `"žltá*"*`, `a"b` → `"a""b"*`, `a b` → `"a b"*`, and the phrase cases (`exact phrase` → `"exact phrase"`, `a"b` → `"a""b"`). The `*`-in-content case is a separate test (`star_in_content_is_quoted`) so it reads as the 3.18 regression, and a FTS5 engine probe (`kb-db/tests/fts5.rs` case 7) closes the string-vs-engine split (F3).

### F2 · `atomic_write` and `tag-alias --export` re‑implement the temp‑file discipline a third time — [M] **RESOLVED**

**Status: RESOLVED (2026-11-01).** The single owner `kb/cli/writeops.rs::unique_tmp_in` now exists and all three sites call it:
1. `kb/cli/editor.rs` — buffer name from `unique_tmp_in` (+ a `.md` extension so editors pick up Markdown syntax).
2. `kb/main.rs::atomic_write` — `let tmp = unique_tmp_in(dir, &name);` (replaces the fixed `.{name}.kb~tmp`).
3. `kb/cli/tag.rs` (alias `--export`) — `std::fs::write(p, text)` → `atomic_write(p, text.as_bytes())`.

The duplicated-logic-with-divergent-hardening smell is gone; the same recipe propagates to every caller if it ever needs to change.

### F3 · `search_grammar` render‑order contract is load‑bearing but only partly pinned by the FTS5 probe — [L] **RESOLVED (by F1)**

The FTS5 probe for `MATCH '"abc*"*'` now lives in `kb-db/tests/fts5.rs` (case 7) — the two pins (string-level in `search_grammar.rs` and engine-level in `fts5.rs`) stay consistent as the render-order contract says they should.

---

## Summary of fixes ranked by impact

All seven items landed (the "RESOLVED" tag on each points to the finding
above it for the before/after). The `[L]` findings that were not in this
ranked list — B2, B3, B4, B7, C4, C5, D2, D4, D7, D10 — were closed in the
same sweep; the E‑section + C6 are positive *observations* (confirmed
conforming, not defects), not open work.

1. **(A1+C3)** Split `kb/main.rs` by concern *and* build a tiny
   `json::Object/Array` — the single highest‑leverage pair of refactors.
   **RESOLVED** — verb/JSON/diff/edit code now in `kb/cli/*` (15 files),
   `JObject` in `kb/cli/json.rs`; `kb/main.rs` 3331→~1240.
2. **(B1+B5)** Turn the two `panic`/`unwrap_or_default` call sites in
   frontmatter / editor into `Err((msg, code))`. **RESOLVED** — B1
   `expect`→`Err(NotScalar)`; B5 read-back `.map_err` instead of `[]`.
3. **(B6)** Promote `etag_of` to one definition in `kb-core`, remove
   both `kb-db::index::etag_of` and `kb::etag_of`. **RESOLVED** — one
   canonical `kb_core::etag::of`; both old sites now thin delegating wrappers.
4. **(D1+D5+D6)** One inline‑summary + one provenance tag for each
   `SECURITY_REPORT N.M` / `PLAN §X` comment; rename *or* cross‑link the
   TUI role that shares a name with the DB column. **RESOLVED** — anchor‑index
   tables added to `SECURITY_REPORT.md` (§3) and `PLAN.md` (Contents);
   the shared name is cross‑linked in both directions (not renamed).
5. **(C1)** Make `confirm` return `Kb` (or `(String,i32)`) and kill the
   five identical "wrap the `false` to a `Kb`" call sites. **RESOLVED** —
   `confirm` now returns `Result<(), (String, i32)>`; the redundant call
   sites are gone.
6. **(D8+D9)** Consolidate fixture locations. **RESOLVED** by moving
   `tests/fixtures/` → `kb-core/testdata/` (its owner) and keeping
   `kb/testdata/` as the shared-body diff golden — the one-per-package option.
7. **(C2)** Add `#![forbid(unsafe_code)]` to the three binary/crate roots.
   **RESOLVED** — done as a *workspace* lint (`Cargo.toml` `[workspace.lints]`)
   inherited by all three crates, rather than three separate `#![forbid]`s.

_Closed in the same sweep but originally tagged `[L]` (not in the ranked
list): B2 (`lines[idx]` bound-guard), B3 (`split_at(1)` in `diff.rs`), B4
(`non_flag.first()` guard), B7 (`stats` `COUNT` `?`‑prop), C4 (`sql_quote_lower`),
C5 (`load_corpus` NULL‑title surfaced), D2 (`GATE_COL` const), D4 (`Kb` →
`kb/cli/error.rs`), D7 (`VARIANT` const), D10 (README "Further reading").
Confirmed‑conforming positive observations, no action: C6 (error‑shape
consistency), E2 (`panic=abort`), E3 (`cargo user` alias), E4 (9‑line
`main.rs` shells), E5 (`vstd` pin), E6 (`rusqlite` bundled)._

**2026‑11‑01 re‑review:** the three new items — F1 (`render_term`/`needs_quoting`
 composition test gap · SEC §3.18), F2 (temp‑file discipline split across 3 sites
 · SEC §3.19/§3.20), F3 (note: FTS5 probe + unit pin both needed) — are all
 **RESOLVED** in the same tree. The two code‑health asks (F1's table‑driven
 `render_term` pin and F2's single `unique_tmp_in` helper) landed *beside* the
 one‑line `needs_quoting` change, so the regression is tripped twice (string +
 engine), not once. E7 confirms supply‑chain pins are current.
