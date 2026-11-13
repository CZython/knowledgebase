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
