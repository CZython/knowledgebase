# `kb` — general codebase review

Date: 2026-10-24 · Branch: `kb-threat-verification`
Scope: everything *beyond* the hardened-variant pentest (see
`PENTEST_KB-HARDENED.md`). Concerns: oversized functions, hidden
`panic`/`unwrap`/index panics on the agent path, duplicated logic, error /
JSON / SQL consistency, and **searchability** (can a newcomer or agent find a
thing without already knowing where it lives?).

File sizes (LOC): `kb/main.rs` **3331** · `kb/tui/mod.rs` **4434** ·
`kb/tui/render.rs` 1308 · `kb/tui/input.rs` 717 · `kb/diff.rs` 527 ·
`kb-core/src/frontmatter.rs` 1481 · `kb-core/src/section.rs` 1144 ·
`kb-core/src/search_grammar.rs` 614 · `kb-db/src/refresh.rs` 654 ·
`kb-db/src/index.rs` 650.

Severity legend: **[H]** correctness / data-integrity · **[M]** maintainability
/ consistency (an agent or future dev will trip) · **[L]** hygiene.

---

## A. Large functions

The "CLI shell" (`kb/main.rs` + `kb/tui/*`) is where every non‑trivial
behaviour lives. The verified core (`kb-core`) and `kb-db` are well‑decomposed;
the problem is concentrated in the shell + TUI.

### A1 · `kb/main.rs` is 3,331 lines / 59 fns in one file — [H]

`kb/main.rs` is *the* shared body for **both** binaries. 59 functions mix, in a
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

### A2 · Individual oversized functions in the shell — [M]

Worst offenders (line counts from a brace‑depth scan; these are *single* fns):

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

These all share the same shape: **do a query, then hand‑format a giant
`format!`/`println!` JSON blob inline.** `cmd_search` is the clearest example —
~90 lines are pure WHERE‑clause construction + ~40 lines are the two render
branches. Suggested split (no behaviour change):
`build_search_sql(parsed) -> (sql, params)` and `render_search(rows|json)`.
That drops `cmd_search` to ~60 and makes the FTS/NOT/gate logic independently
testable (it currently is only testable by running the whole verb).

### A3 · The `run_editor` / `edit_in_place` pair — [M]

Two ~50‑line fns (`kb/main.rs:3060`, `3117`) that both "pick editor → spawn →
read result → clean temp". They differ only in launch vs. in‑place semantics.
A single `run_editor_in(buf_dir, path) -> Result<Vec<u8>, (..)>` with a
`in_place: bool` would remove a duplicate `$VISUAL`/`$EDITOR` read loop and
keep the two failure paths (`launch failed` vs. `non-zero exit`) in one place
(the current `edit_in_place` re‑implements the hint logic by hand, `3131-3145`).

---

## B. Hidden `panic!` / `unwrap()` / index‑panics on non‑test code paths

A "pentest / agent‑facing" read must be able to say *which* runtime lines can
abort *without* a test. Findings (all in non‑test code):

### B1 · `list_rewrite_upgrade::expect(...)` — [M]

`kb-core/src/frontmatter.rs:830-831`:

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

### B2 · `lines[idx]` on a `split_inclusive` vec — [L]

`kb-core/src/frontmatter.rs:832, 852`:

```rust
let idx = locate_key_line(data, key).unwrap_or(0);   // 851
let line = lines[idx].clone();                       // 852
```

If `locate_key_line` returns an index ≥ `lines.len()` (e.g. an empty document
where the key line has a BOM the split dropped), this panics. Guard with
`if idx >= lines.len() { return Err(..) }`.

### B3 · `l[1..]` and `l.as_bytes()[0]` in `diff.rs` — [L]

`kb/diff.rs:364-366`:

```rust
Some(b' ') | Some(b'-') | Some(b'+') => {
    body.push(PatchLine { kind: l.as_bytes()[0] as char, text: l[1..].to_string() }
```

Safe here (the arm pattern already guarantees non‑empty + valid prefix), but
it is a *second* index read of the same byte — a refactor that relaxes the
pattern to `Some(_) => break` (which already exists at line 369) while a
`Some(b'\t')` slips through would panic. Recommend `let (kind, text) =
match l.as_bytes()[0] { b' ' => (…, …), … }` or a single `l.split_at(1)`.

### B4 · `non_flag[0]` in the bare‑dispatch — [L]

`kb/main.rs:552-553`:

```rust
if non_flag.len() == 1 && !SUBCMDS.contains(&non_flag[0].as_str()) {
    let s = non_flag[0];
```

Safe (guarded by `len()==1`). Listed because it is the *only* place in the
file where an index is used for agent‑facing dispatch — a reader cannot tell
at a glance it is guarded. Consider `let Some(s) = non_flag.first() else
{ /* fall through to clap */ };` which makes the guard local.

### B5 · `run_editor::read(&tmp).unwrap_or_default()` — [M]

`kb/main.rs:3074`. Silently returns `0`‑bytes when the file read fails after a
successful editor exit. See pentest F8.

### B6 · `etag_of` duplicated in two crates — [M, not a panic but a maintenance risk]

`kb/main.rs:3191` and `kb-db/src/index.rs:97` both define the *same* FNV‑1a
64‑bit etag, and the db one even says "identical to the kb CLI's
`etag_of`" (`index.rs:96`). Two copies of a hash are a classic drift trap: if
one is corrected (e.g. `as u64` vs. `as u8` promotion) the *other* silently
emits a different 16‑hex for the same byte string, and the optimistic‑
concurrency `--if-unchanged` check stops matching. **One** definition should
live in `kb-core` and both should call it.

(There is also a **third** copy: `kb-core/src/lib.rs` — see
`grep -rn 'cbf2_9ce4' kb-core kb kb-db` for all three.)

### B7 · `unwrap_or(0)` on the `COUNT(*)` query — [L]

`kb/main.rs:2293, 2299` (`fwd_count`/`back_count`). `COUNT(*)` never errors and
never returns NULL, so `unwrap_or(0)` is belt‑and‑suspenders; it hides a real
bug if the query ever *does* fail (the agent gets `0 out / 0 in` with exit 0
instead of an error). Use `?`‑propagate.

---

## C. Consistency / error model

### C1 · **`Kb = Result<i32, (String, i32)>` is great; the *one* exception is `confirm`** — [L]

Every fallible step in the repo returns `(String, i32)` so `?` composes.
**Except** `confirm` (`kb/main.rs:3150-3168`) returns a bare `bool` and its
`false` branch is *re‑wrapped* by every caller:

```rust
if !confirm("apply?", assume_yes || json) {
    return Err(("aborted (not confirmed)".into(), EXIT_ERR));
}
```

That wrapper block appears identically in **at least** `cmd_section` (via
`commit`), `cmd_create` (`kb/main.rs:874`), `cmd_patch`, `cmd_replace`. If a
new verb forgets the wrap, its "aborted" outcome has exit code `0` with a
silent no‑apply. Change `confirm` to return `Result<() (String,i32)>` and the
wrap disappears.

### C2 · **No `#[rustfmt::skip]` / `rustc -C` anywhere; no `#![deny(unsafe_code)]`** — [L]

The codebase is `unsafe`‑free (verified: `grep -rn 'unsafe' kb/ kb-core
kb-db` → none in non‑test source). That is *worth pinning* with
`#![forbid(unsafe_code)]` at the top of each crate root (and at
`kb/main.rs`'s real crate entry, `kb-user/src/main.rs` /
`kb-hardened/src/main.rs`, since those are the *true* crate roots that
`include!` opens). A future `unsafe` block added by a TUI backend or a FFI
fix would then be a compile error unless deliberately allowed.

### C3 · **JSON emission is hand‑rolled and inconsistent** — [M]

`jstr` (`kb/main.rs:3204`) is a 19‑line JSON escaper with its own unit test —
good. But it is used in *five* places with different surrounding templates
(`commit` line 2945, `cmd_create` 855, `cmd_search` 1339, `cmd_tag` 1702/1655,
`cmd_info` 2306, `cmd_links` 2066, `CreateReport::to_json` 985, `cmd_browse`
1511, `cmd_roots` 1017, `cmd_stats` 2346). That is **≥12** different JSON
documents emitted from **≥9** different fns, none of which shares a single
"empty array" constant. The inconsistency is visible to the agent and was one
of the pentest findings (F6, `[{}]` vs. `[]` vs. `[ ]`).

Fix: introduce a `json::Array { items: Vec<String> }` and
`json::Object { … }` mini‑library (30 LOC) so:

* `empty_array().render() == "[]"` everywhere,
* every object is `{ "k": v, … }` with one escape owner,
* the ~12 `format!("{{…}}", …)` call sites become data, not strings.

This is the highest‑ROI *code‑quality* change in the repo after the file
split (A1).

### C4 · **SQL literal injection in the IN‑list (already escaped, but by hand)** — [L]

`kb/main.rs:1260-1267`:

```rust
.map(|t| format!("'{}'", t.to_lowercase().replace('\'', "''")))
```

Correct for SQLite (doubling the quote is the escape), and both call sites
(`cmd_search` 1262 and `cmd_tag` 1669) do exactly this. But it is in *two*
places with two different variable names (`t.to_lowercase().replace` and the
same). A small `sql_quote_lower(s: &str) -> String` kills the duplication and
the "did we escape?" question on re‑read.

### C5 · **`load_corpus` silently drops `title`→`""` when NULL** — [L]

`kb/main.rs:2163`:

```rust
slug: slug.unwrap_or_default(),
```

A `pages.title` row of `NULL` (which is *allowed* — `pages.title TEXT` no
`NOT NULL`) becomes an *empty* slug in the resolver. Two pages with `title:
NULL` and the same filename then both resolve to slug `""` (which is what
`identity::resolve_slug` falls back to if title is `Some` but blank —
`identity.rs:206-224` shows the blank handling, but the *fallback on the
core side* is only when `fm.title` is `None`). Net: the resolver's stage‑2 /
stage‑4 exact match is weaker than intended when a stored title is NULL.
Use a real `Option<String>` slug in `resolver::Page`, or `filter`‑out
`slug.is_empty()` during the stage‑2 pass *before* treating it as a match.

### C6 · **Error message shape `"{what}: {e}"` is consistent — good.** — [OK]

Verified across `open_db`, `cmd_*`, `refresh_all`, etc. Every `map_err`
follows `format!("<verb>: <e>")`. This is a model of good error
consistency; keep it.

---

## D. Searchability

A newcomer or an LLM agent has to *find* things without already knowing
where they live. Measured: grep‑ability, naming collisions, doc‑comment
coverage, and the "obvious first file" for a concept.

### D1 · `search_required_phrase` — the *same* field name in four places with two spellings — [M]

`grep -rln search_required_phrase` returns files in **four** places:

* `kb/main.rs` — *SQL column + CLI flag name* (the canonical form).
* `kb-core/src/lib.rs`
* `kb-core/tests/frontmatter_smoke.rs`
* `kb-core/examples/frontmatter/note.example.md`
* `kb/tui/palette.rs`, `kb/tui/render.rs`, `kb/tui/mod.rs`, `kb/tui/input.rs`,
  `kb/tui/palettes/{default,colorblind}.toml` — *the TUI's role name* (a label
  shown in the TUI legend).

Two *different* concepts use the same identifier: a **corpus field** and a
**TUI palette-role / legend label**. A search for
"change the colour of the search-required-phrase label in the TUI" leads the
agent to the TUI files *or* the SQL column, depending on which phrase they
remembered. Pick one of:

* rename the TUI role (it is a *palette* concept; call it something like
  `gate_term_label` or `gate_phrase_role`), or
* add a one‑line `/// "the TUI role that renders <code>search_required
  _phrase<\/code> as a legend label — the DB column itself is owned by
  `kb-core::frontmatter`. See `kb/main.rs:2262`.` to the TUI usage and a
  matching pointer on the SQL column.

### D2 · `pages.search_required_phrase` SQL column name appears **in 3+ queries as a literal** — [L]

`kb/main.rs:1288-1290`, `1297-1299`, `2271`, and (in `kb-db`) `kb-db/src/
migrate.rs:46`. The *column name* is a magic string in ≥4 places. A schema
rename is a 5‑file edit (4 queries + 1 CREATE TABLE) — manageable, but the
`refresh` + `add_root` + `search` + `info` + `browse` + `tag` verbs that
`SELECT` are *not* centrally documented. Add a `struct PagesCols` (or a
`const SQL_PAGES_SELECT: &str`) in `kb-db/src/` and use it from `kb/main.rs`
so the column list is defined *once*.

### D3 · `cmd_search`, `cmd_browse`, `cmd_tag`, `cmd_tag_alias`, `cmd_links`, `cmd_info`, `cmd_stats`, `cmd_resolve`, `cmd_tag` are all **top‑level** in one file — [M → covered by A1]

After the A1 split this becomes: `kb/cli/verbs/search.rs`,
`kb/cli/verbs/browse.rs`, …, and the "which verb does X?" question is answered
by the file name.

### D4 · The `Kb` type alias is the *one* error abstraction but is only named in one place — [M]

`kb/main.rs:61` defines `type Kb = Result<i32, (String, i32)>;` and **no other
file** mentions it. Every verb handler's return type is `Kb` *by name* (so
`grep -rn '-> Kb'` is a complete index of fallible verbs — **great**), but a
new contributor adding a fn in a *new* file (post A1) has no `use` to write
because the alias is file‑local. Promote it to `kb/cli/error.rs` (or
`kb_core` if the core is to own it) so the name travels with the code.

### D5 · `SECURITY_REPORT.md` references (`SECURITY_REPORT 3.1`, `3.3`, `3.4`, `3.5`, `3.6`, `3.7`, `3.16`, `3.17`) — [M]

`grep -rn 'SECURITY_REPORT'` returns ~25 call‑site comments pointing to a file
*outside* the repo (`SECURITY_REPORT.md`, 38 KB, at the root). Each call site
carries a *numbered* reference that is *only* meaningful if you open the
separate file:

```
kb/main.rs     : "SECURITY_REPORT 3.1 mitigation: …"
kb/main.rs     : "SECURITY_REPORT 3.16: cap the DP table size. …"
kb/main.rs     : "SECURITY_REPORT 3.17: hoist the alias + distinct-tag loads …"
kb-core/src/frontmatter.rs : "SECURITY_REPORT 3.7 — same `from_utf8_lossy` fix."
kb-core/src/aliases.rs     : "SECURITY_REPORT 3.6 …"
kb-db/src/refresh.rs       : "SECURITY_REPORT 3.3 data-loss window"
kb-db/src/walk_snapshot.rs  : …
```

That is a *lot* of cross‑file pointer indirection. Recommend: copy the
relevant sentence *from* the SECURITY_REPORT *into* the comment (the
"why‑this-line" is what the reader needs), and keep the `SECURITY_REPORT
NN` tail as a *provenance* tag (e.g. "SECURITY_REPORT:3.17"). The current
call sites do this well (the inline text is already a paraphrase of the
finding), but the *number* is the most fragile part — it is a *report
section number* with no link, no anchor, no URL, and the report is a
hand‑maintained `.md`. Add `https://#sec-3-17`‑style anchors to the
SECURITY_REPORT (or at least a table of contents), or drop the numbers in
favour of a short slug that grep‑matches in both files.

### D6 · `PLAN.md` is 160 KB / §13f / §13p / §9 / §10 — [L]

`grep -rn '§13'` returns ~30 call sites. Same problem as D5, but at a larger
scale (the PLAN is the whole design, the SECURITY_REPORT is the bug
catalogue). The `PLAN §9` / `PLAN §10` references are the **load‑bearing**
ones (they define the *assumed* vs. *verified* boundary) and they should at
least carry a link to the section in question or a 1‑line inline summary.

### D7 · The "which `kb` binary is this?" question is answered by a compile flag — [M]

`kb/main.rs:73-82` selects the CLI identity on `cfg!(feature = "editor")`.
The *two packages* are the honest answer (`kb-user` or `kb-hardened`), but
the *shared body* does not carry the package name — it carries the binary
name, which depends on the *caller's feature choice* (see pentest F1/F2). A
`include!`‑side constant like `const VARIANT: &str = cfg!(feature="editor") ?
"user" : "hardened";` at the top of `kb/main.rs` would make every call site
that needs "am I the agent build?" a one‑line check.

### D8 · `tests/` is a directory containing only `fixtures/` — [L]

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

### D9 · `kb/testdata/` vs. `kb-user/testdata/` vs. `tests/fixtures/` — [M]

`grep -rn 'testdata' kb kb-user kb-core kb-db tests` returns *three* different
fixture locations. The diff‑parser golden (`standard.diff`) is in
`kb/testdata/` (`kb/diff.rs:491`), the integration tests look in
`kb/testdata/` *by relative path from the package dir* (`kb-user/tests/`), and
the orphan `tests/fixtures/` has what, currently, no consumer. A single
`testdata/` at the repo root, or one per *package* (with the root one removed),
would make "where's the fixture?" one grep.

### D10 · `gate.sh`, `verify.sh`, `SECURITY_REPORT.md`, `PLAN.md`, `HARDENED_BUILD.md`, `TODO-palette.md` — [L]

Five root‑level `.md` + two `.sh` scripts. That is normal for a 0.1 project.
Noted only for *searchability*: `README.md`'s "Getting started" section should
enumerate *all seven* root‑level docs as "further reading" with a one‑line
purpose each — right now a newcomer has to open each to find out whether it
is "how to build", "how to prove", or "the bug catalogue".

---

## E. Miscellaneous

### E1 · `rust-version = "1.98.1"` in the *workspace* — [L]
`Cargo.toml:23`. Pinned to a very recent toolchain; that is fine for a
private project, but it is an *upper* bound (a 1.98 toolchain won't build a
`rust-version` of 1.98+1). There is no `rust-toolchain.toml` pin to check
against:

```
$ cat rust-toolchain.toml
```

confirm it matches `1.98.1` exactly (or use `channel = "nightly-…"` if
`vstd` requires nightly — it does not, since Verus runs as a *separate*
binary).

### E2 · `profile.release.panic = "abort"` — [L]
`Cargo.toml:37`. Combined with the codebase's *no‑unsafe* + hand‑rolled
error, this is a good default. Combined with `lto = "fat"` +
`codegen-units = 1` it is a *deliberate* perf/size choice. Good.

### E3 · `.cargo/config.toml` aliases `user` / `hardened` / `kb-user` / `kb-hardened` — [L]
`cargo user` works. Documented in `HARDENED_BUILD.md`. Good.

### E4 · The two `kb-user`/`kb-hardened` packages are each a **9‑line
`src/main.rs` + a one‑line `#[command(name="…")]`** — [L]

Clean. The *real* code is in `kb/main.rs`. That is the A1/D7 split
already.

### E5 · `vstd = "=0.0.0-2026-09-06-0133"` is an exact pin — [L]
`Cargo.toml:53`. Documented as load‑bearing in the verus‑fork
compatibility. Fine. The *comment* explains the why — good.

### E6 · `rusqlite = "0.32"` + `bundled` — [L]
`Cargo.toml:58`. A *single* SQLite in the tree. Good. The comment
explains the why.

---

## Summary of fixes ranked by impact

1. **(A1+C3)** Split `kb/main.rs` by concern *and* build a tiny
   `json::Object/Array` — the single highest‑leverage pair of refactors.
2. **(B1+B5)** Turn the two `panic`/`unwrap_or_default` call sites in
   frontmatter / editor into `Err((msg, code))`.
3. **(B6)** Promote `etag_of` to one definition in `kb-core`, remove
   both `kb-db::index::etag_of` and `kb::etag_of`.
4. **(D1+D5+D6)** One inline‑summary + one provenance tag for each
   `SECURITY_REPORT N.M` / `PLAN §X` comment; rename *or* cross‑link the
   TUI role that shares a name with the DB column.
5. **(C1)** Make `confirm` return `Kb` (or `(String,i32)`) and kill the
   five identical "wrap the `false` to a `Kb`" call sites.
6. **(D8+D9)** Consolidate fixture locations: one `testdata/` or one
   `tests/` directory with a real `Cargo.toml`.
7. **(C2)** Add `#![forbid(unsafe_code)]` to the three binary/crate roots.
