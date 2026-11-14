use crate::EXIT_ERR;
use kb_db::sql::params;
use std::path::{Path, PathBuf};

// A1: per-verb CLI modules.
// Every item is `pub(crate)` in its verb file; re-exported here so the
// crate root (`use crate::cli::*;`) and any sibling module can reach them.
pub(crate) mod browse;
pub(crate) mod cmd_add;
pub(crate) mod create;
#[cfg(feature = "editor")]
pub(crate) mod editor;
// D4: the `Kb` alias (the CLI's error model) lives here, not in kb/main.rs.
pub(crate) mod error;
pub(crate) mod info;
pub(crate) mod json;
pub(crate) mod links;
pub(crate) mod roots;
pub(crate) mod search;
pub(crate) mod section;
pub(crate) mod show;
pub(crate) mod tag;
pub(crate) mod textsrc;
pub(crate) mod verify;
pub(crate) mod writeops;

/// Word-count metrics (lines, chars, words) of a whole text — the one owner
/// so `info` (`wc`), `show` (`lines`/`bytes`), and `toc` never drift.
///
/// * `lines` — `str::lines().count()`; matches `Doc::total_lines` exactly
///   (both count line-starts; a trailing `\n` adds no phantom line) and is
///   the same number `kbn show` reports as `lines`.
/// * `chars` — Unicode scalar count (what `wc -m` and a human mean by
///   "chars"); `Doc` reports `bytes`, but `info` promises chars, so we keep
///   the scalar semantics here. Callers that need bytes can `len()` the text.
/// * `words` — whitespace-separated token count, same as `wc -w`.
pub(crate) fn text_wc(s: &str) -> (usize, usize, usize) {
    (
        s.lines().count(),
        s.chars().count(),
        s.split_whitespace().count(),
    )
}

// ---------------------------------------------------------------
//  Stable page uuid (16 hex) — stored in `pages.uuid`, resolved by index.
// ---------------------------------------------------------------
//
// The full form is `uuid_of(id, abs_path)` = FNV-1a 64 of `id \0 abs_path`
// (see `kb_core::uuid`); it is salted by `abs_path` (the `pages` PK), so a
// full 16-hex uuid is *always* unique in the corpus. A *short* form (5–15
// hex) is the shortest prefix of that string that still names exactly one
// row, so two pages sharing the same `type:slug` in different folders get
// distinct short forms without any collision in the DB.
//
// Every lookup below is an indexed `WHERE uuid LIKE ? || '%' LIMIT 2`
// probe — at most 2 rows materialised, no full-table scans, no `COUNT(*)`.
// The `uuid` column is populated by the indexer on write (see
// `kb_db::index::upsert_page`) and backfilled once for existing rows
// (see `kb_db::index::backfill_uuid`).

/// Probe the `uuid` index for a prefix. Returns exactly the matched
/// `abs_path`(s), bounded to 2 rows (0/1/≥2 semantics — we never load more
/// than the 2 that prove "not unique").
fn uuid_prefix_abs_paths(db: &kb_db::Db, prefix: &str) -> Result<Vec<String>, (String, i32)> {
    let like = format!("{prefix}%");
    let stmt = db
        .conn
        .prepare("SELECT abs_path FROM pages WHERE uuid LIKE ?1 LIMIT 2")
        .map_err(|e| (format!("prepare uuid prefix: {e}"), EXIT_ERR))?;
    let rows = stmt
        .query_map(params![like], |r| r.get::<_, String>(0))
        .map_err(|e| (format!("uuid prefix rows: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("uuid prefix rows: {e}"), EXIT_ERR))?;
    Ok(rows)
}

/// Given a page's full uuid and its own `abs_path`, return the *shortest*
/// prefix (≥ `kb_core::uuid::FLOOR`) that names exactly that page alone.
///
/// Walks `FLOOR..=16`; at each length `k` it does an indexed `LIMIT 2` probe
/// and a row is "qualifying" iff exactly one row matches and that row is the
/// page *itself* (so a prefix shared with a *different* page keeps us
/// walking). Each probe materialises ≤2 rows; the walk is ≤12 probes —
/// bounded, no full-table scan, no `COUNT(*)`.
///
/// The `self_abs` argument resolves the one-row match to "is it me?": the
/// full uuid is salted by `abs_path`, so the *full* 16-hex probe is always
/// unique. By the time we reach `k = 16` at least one page (ours) matches,
/// and since we are looking for our own uuid, that one row is always us —
/// so the walk always terminates on `Some`.
pub(crate) fn uuid_short_for(db: &kb_db::Db, full_uuid: &str, self_abs: &str) -> Result<String, (String, i32)> {
    let full_uuid = full_uuid.to_ascii_lowercase();
    let n = full_uuid.len();
    let lo = kb_core::uuid::FLOOR.min(n);
    for k in lo..=n {
        let prefix = &full_uuid[..k];
        let rows = uuid_prefix_abs_paths(db, prefix)?;
        if rows.len() == 1 && rows[0] == self_abs {
            return Ok(prefix.to_string());
        }
    }
    // Unreachable: at k = n the full uuid (salted by self_abs) is unique, so
    // the 1-row match is always self_abs. Kept as a total return for the
    // case `self_abs` is not actually in the DB (deleted mid-call).
    Ok(full_uuid)
}

/// Resolve a user-supplied token to an `abs_path`, in this priority order:
///   1. **Absolute / relative path** (any token containing the platform path
///      separator, or starting with `/`, `~`, or `…`-like) → use verbatim.
///   2. **Display id** (`type:slug`, contains `:`) → `pages.id = token`.
///   3. **Short uuid prefix** (5–16 hex, all digits/letters) → prefix-match
///      against all pages' `etag_of(id)`. Exactly one match → use it; zero →
///      fall-through to step 4; ≥2 → error ("extend the prefix").
///   4. **Bare relative path** → use verbatim (existing behaviour).
///
/// Used by every path-arg verb (show / toc / section / replace / patch /
/// info / verify) so an agent can copy a short "uuid" out of a `search` or
/// `info` result and paste it back as a page reference, without typing the
/// full path.
///
/// `dbpath` defaults to the DB the caller opened (the `--db` flag). The
/// resolver opens the DB only if the token *looks like* a uuid prefix or a
/// display id — path tokens never trigger a DB open (single-file verbs stay
/// DB-free for their pure byte-op path).
pub(crate) fn resolve_file_or_uuid(
    dbpath: Option<&Path>,
    tok: &str,
) -> Result<PathBuf, (String, i32)> {
    use crate::EXIT_ERR;
    // 1. Path signals — no DB needed.
    if tok.starts_with('/')
        || tok.starts_with('~')
        || tok.starts_with("./")
        || tok.starts_with("../")
        || tok.contains('/')
        || tok.contains('\\')
    {
        return Ok(PathBuf::from(tok));
    }
    // 2. Display id (`type:slug`) → exact lookup.
    if tok.contains(':') {
        let db = crate::open_db(dbpath)?;
        let abs: String = db
            .conn
            .query_row(
                "SELECT abs_path FROM pages WHERE id = ?1",
                params![tok],
                |r| r.get::<_, String>(0),
            )
            .map_err(|e| (format!("no page with id {tok:?} ({e})"), EXIT_ERR))?;
        return Ok(PathBuf::from(abs));
    }
    // 3. Short uuid prefix (5–16 hex) → indexed `WHERE uuid LIKE ? '%' LIMIT 2`.
    //    A full 16-hex token is always a unique match (salted by `abs_path`).
    //    A shorter token may be 0/1/≥2 — we bound the materialised set to 2
    //    rows so the "≥2" arm is a true "not unique" without a table scan.
    if kb_core::uuid::is_uuid_token(tok) {
        let db = crate::open_db(dbpath)?;
        let matches = uuid_prefix_abs_paths(&db, tok)?;
        match matches.len() {
            1 => return Ok(PathBuf::from(matches.into_iter().next().unwrap())),
            0 => {
                // Fall through to step 4 (a short hex-named file, or the DB is
                // empty — either way, treat the token as a path).
            }
            n => {
                let list: Vec<_> = matches
                    .iter()
                    .map(|p| p.rsplit('/').next().unwrap_or(p))
                    .collect();
                return Err((
                    format!(
                        "uuid prefix {tok} is ambiguous ({n} pages: {}) — extend the prefix",
                        list.join(", ")
                    ),
                    EXIT_ERR,
                ));
            }
        }
    }
    // 4. Bare relative path.
    Ok(PathBuf::from(tok))
}

/// Render one numbered page row the way `search` and `browse` present results
/// in human (non-JSON) mode, in a single shared shape so the two surfaces
/// never drift:
///
///   line 1 — `<#>  <uuid_short>  <id>  <title>  [rel_path]  [extra…]`
///   line 2 — an optional, indented description (only on search; browse rows
///            have no description field).
///
/// `uuid_short` is the shortest-unique 5–16 hex token the caller resolves
/// (fall back to the full 16-hex when a short form can't be computed — that's
/// still a valid, copy-back-able token). `extra` holds the *variable* per-verb
/// fields (browse: `tags:` / `gates:` / `links:`; search: `gates:`), which are
/// appended space-separated after `rel_path` and skipped when empty. Keeping
/// the fixed prefix uniform across both verbs is what lets an agent read
/// either as "row = one page, first three columns are always number / uuid /
/// id".
///
/// Prints to `stdout`. Returns nothing — the caller owns the per-row
/// follow-on rendering (e.g. `browse` prints a `→` child line per label ref
/// before moving on to the next row).
pub(crate) fn print_page(
    n: usize,
    uuid_short: &str,
    id: &str,
    title: Option<&str>,
    rel_path: &str,
    extra: &[(String, String)],
    description: Option<&str>,
) {
    let title = title.unwrap_or("");
    let mut line = format!("{:>3}  {:<10} {}  {}", n, uuid_short, id, title);
    if !rel_path.is_empty() {
        line.push_str(&format!("  {rel_path}"));
    }
    for (k, v) in extra {
        line.push_str(&format!("  {k}:{v}"));
    }
    println!("{line}");
    if let Some(d) = description {
        if !d.is_empty() {
            println!("      {d}");
        }
    }
}

pub(crate) use browse::*;
pub(crate) use cmd_add::*;
pub(crate) use create::*;
#[cfg(feature = "editor")]
pub(crate) use editor::*;
pub(crate) use error::*;
pub(crate) use info::*;
pub(crate) use json::*;
pub(crate) use links::*;
pub(crate) use roots::*;
pub(crate) use search::*;
pub(crate) use section::*;
pub(crate) use show::*;
pub(crate) use tag::*;
pub(crate) use textsrc::*;
pub(crate) use verify::*;
pub(crate) use writeops::*;
