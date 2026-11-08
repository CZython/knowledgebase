// # kb — the shared CLI body. See PLAN.md §11 / §13f.
//
// NOTE: this file is the single shared body included into two crates
// (`user` and `hardened`). It must not begin with `//!` crate-level doc
// comments — those are only allowed at the start of the *crate*, and each
// crate's real `src/main.rs` provides its own leading doc block.
//
// M6 lands the **single-file namespace**: section-aware read / edit of one
// markdown file, backed by the pure, verified core (`kb-core::section`), with
// a zero-dependency standard-`diff -u` renderer + parser (no `diff` / `git`
// fork-exec), optimistic concurrency via an etag, atomic writes, and an
// editor flow.
//
// The **multi-file namespace** (DB-backed: `add` / `drop-root` / `search` /
// `browse` / `tag` / `link` / …) lands in M5/M7 — these commands are
// declared (so `kb --help` is complete) but return a not-implemented message.
//
// ## Agent contract (locked, PLAN §13f)
// * **Read** is scoped + minimal: `kb toc <file>` (outline only),
//   `kb show <file> <ref> [--body|--full] [--json] [--etag]`.
// * **Write** takes the *complete new section text* (3 sources:
//   `--text | --from-file | --stdin`), never a patch. Diff is **output for
//   review** (standard `diff -u` shape, parseable by any tool), never input.
// * **Concurrency**: `--if-unchanged <etag>` = FNV-1a 64-bit (16 hex) over the
//   target section's bytes; mismatch → exit code 3.
// * **Exit codes**: `0` ok · `1` generic · `2` section/resolve · `3` etag.

mod diff;

use clap::{Args, Parser, Subcommand};
use kb_core::aliases as kbalias;
use kb_core::frontmatter as fm;
use kb_core::labels;
use kb_core::links;
use kb_core::meta::NoteType;
use kb_core::resolve as resolver;
use kb_core::search_grammar;
use kb_core::section::{splice, Doc, SectionPlacement};
use kb_core::tags as kbtags;
// The TUI is an assumed, interactive-only shell (M8). Agents drive the plain
// CLI verbs instead, so the whole module is behind the `tui` feature (default
// on) — a hardened build compiles without it and its deps:
// `cargo build -p kb-hardened` (that package turns the `tui` feature off; the
// `kb-user` package — the full build — keeps it on, mirroring the `editor` gate).
#[cfg(feature = "tui")]
mod tui;
use rusqlite::{params, OptionalExtension};
use kb_db::refresh as kb_refresh;
use kb_db::Db;
// D2: the gate-phrase column name now comes from one owner (kb-db), not a
// hand-typed literal at each query site.
use kb_db::migrate::COL_SEARCH_REQUIRED_PHRASE as GATE_COL;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

mod cli;
use crate::cli::*;


pub(crate) const EXIT_OK: i32 = 0;
pub(crate) const EXIT_ERR: i32 = 1;
pub(crate) const EXIT_SECTION: i32 = 2;
pub(crate) const EXIT_ETAG: i32 = 3;

/// D7: the *package* this body was compiled into, as a value the code can
/// branch on. `kb-user` (the full build) compiles with `editor` ON;
/// `kb-hardened` compiles with it OFF (see the `Cli` doc block). A single
/// `cfg!` here is the one place the answer is decided, so every call site
/// that needs "am I the agent build?" can compare `VARIANT == "hardened"`
/// instead of re-evaluating the feature by hand.
pub const VARIANT: &str = if cfg!(feature = "editor") { "user" } else { "hardened" };

// D4: the `Kb` alias is now declared in `kb/cli/error.rs` (the CLI's error
// model module) and re-exported into the crate via `use crate::cli::*;` above,
// so a new verb file can find it without knowing it used to be in main.rs.

/// The shared CLI body is compiled into two packages (`kb-user`,
/// `kb-hardened`), each producing a separate binary (`kb`, `kb-hardened`).
/// The `editor` feature is the single proxy for "which package is this?": it
/// is ON for `kb-user` (default) and OFF for `kb-hardened` (default), so we
/// pick name + version + about via `cfg!(feature = "editor")` at compile
/// time — both packages compile the same file, get the right identity, and
/// share all the other verbs verbatim.
///
/// `kb`          — the full build: `editor` + `tui` ON by default
/// `kb-hardened` — the agent build: neither ON by default, ` [hardened]` suffix
#[derive(Parser)]
#[command(
    name = if VARIANT == "hardened" { "kb-hardened" } else { "kb" },
    version = if VARIANT == "hardened" {
        concat!(env!("CARGO_PKG_VERSION"), " [hardened]")
    } else {
        env!("CARGO_PKG_VERSION")
    },
    about = "A single-binary Markdown knowledge base over embedded SQLite + FTS5 —\nindex a tree of notes, search them, and edit them by section.",
    // F3: the about/long_about text was identical in both builds, so a
    // hardened agent reading `kb-hardened --help` saw a promise ("ships a TUI
    // plus an in-editor flow") the build does not keep. Now variant-aware, the
    // same way `name`/`version` above are — `VARIANT` is the single place the
    // "which build is this?" decision lives, so the help text can never drift
    // from the feature flags.
    long_about = if VARIANT == "hardened" {
        concat!(
            "kb is a single-binary Markdown knowledge base backed by an embedded\n",
            "SQLite + FTS5 index. Register roots of note files, search them (with\n",
            "`type:`/`tag:` filters, OR/NOT, phrases, and tag-alias expansion), and\n",
            "correctly-shaped edits that take a whole new section's bytes in place of\n",
            "a diff, or apply external `diff -u` / `git diff` patches. All of the\n",
            "edit / resolve / alias / index logic is in a Verus-verified pure-Rust\n",
            "crate; the DB shell is an assumed boundary. This is the hardened\n",
            "build: headless for agents — no TUI, no in-editor flow."
        )
    } else {
        "kb is a single-binary Markdown knowledge base backed by an embedded\nSQLite + FTS5 index. Register roots of note files, search them (with\n`type:`/`tag:` filters, OR/NOT, phrases, and tag-alias expansion), and\ncorrectly-shaped edits that take a whole new section's bytes in place of\na diff, or apply external `diff -u` / `git diff` patches. All of the\nedit / resolve / alias / index logic is in a Verus-verified pure-Rust\ncrate; the DB shell is an assumed boundary. This is the full build: it\nships a TUI plus an in-editor flow; the hardened variant is headless for\nagents."
    },
)]
pub(crate) struct Cli {
    /// Path to the SQLite DB (default: the per-user data dir —
    /// `~/.local/share/knowledgebase/kb.db` on POSIX,
    /// `%LOCALAPPDATA%\knowledgebase\kb.db` on Windows).
    #[arg(long, value_name = "PATH", global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

/// Flags shared by every mutating command.
#[derive(Args, Clone)]
pub(crate) struct WriteFlags {
    /// Show the unified diff before applying (always shown on a write).
    #[arg(long)]
    diff: bool,
    /// Compute + show the diff, but do not write.
    #[arg(long)]
    dry_run: bool,
    /// Skip the confirmation prompt.
    #[arg(long)]
    yes: bool,
    /// Abort unless the target section's etag still equals this value.
    #[arg(long)]
    if_unchanged: Option<String>,
    /// Machine-readable output: the result (op / bytes / diff / applied) as a
    /// JSON object on stdout. Forcing `--json` also suppresses the
    /// interactive diff-to-stdout and the "apply?" prompt (a JSON result must
    /// be the only thing on stdout, and there is no confirm to auto-approve).
    #[arg(long)]
    json: bool,
}

/// Flags for `kb create` (PLAN §11).
#[derive(Args, Clone)]
pub(crate) struct CreateArgs {
    /// Directory to create the page in (default: the cwd). Missing
    /// parent directories are created.
    #[arg(long, value_name = "PATH")]
    path: Option<PathBuf>,
    /// Page type (default: `wiki`). Case-insensitive; one of
    /// wiki | note | label (PLAN §4).
    #[arg(long = "type", value_name = "TYPE", default_value = "wiki")]
    type_: String,
    /// Open the new page in $VISUAL/$EDITOR (fallback `vi`) after it is
    /// written; the saved bytes are what gets indexed.
    /// SECURITY_REPORT 3.1: only exists when the `editor` feature is on;
    /// in a hardened build (`--no-default-features`) `-e` is an
    /// *unexpected argument* — clap rejects it before any work happens.
    #[cfg(feature = "editor")]
    #[arg(short = 'e', long)]
    edit: bool,
    /// Show the new-file diff, then stop: nothing is written, nothing
    /// is indexed.
    #[arg(long)]
    dry_run: bool,
    /// Skip the confirmation prompt.
    #[arg(long)]
    yes: bool,
    /// Emit the result as a single JSON object on stdout.
    #[arg(long)]
    json: bool,
}

/// The complete new text, from exactly one of three sources.
#[derive(Args, Clone, Default)]
pub(crate) struct TextSource {
    /// Literal text.
    #[arg(long)]
    text: Option<String>,
    /// Read text from a file (`-` = stdin).
    #[arg(long = "from-file")]
    from_file: Option<PathBuf>,
    /// Read text from stdin.
    #[arg(long)]
    stdin: bool,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    // ==================== single-file namespace (M6) ====================
    /// Outline of a file: the section tree (ref / level / title), agent-friendly.
    Toc {
        file: PathBuf,
        /// Emit a JSON envelope instead of the text outline.
        #[arg(long)]
        json: bool,
    },
    /// Show the whole file (no `ref`), or a single section's content (with `ref`).
    Show {
        file: PathBuf,
        /// Section ref: dotted number (`1.2`), `A > B > C` title path, or title.
        #[arg(value_name = "REF")]
        ref_: Option<String>,
        /// Own body: heading through first subsection (default with a ref).
        #[arg(long)]
        body: bool,
        /// Full span: this section including all subsections.
        #[arg(long)]
        full: bool,
        /// Emit a JSON envelope instead of raw text.
        #[arg(long)]
        json: bool,
        /// Print just the section's etag (FNV-1a 64-bit, 16 hex chars).
        #[arg(long)]
        etag: bool,
    },
    /// Section-level write ops: `set` / `append` / `insert` / `replace`.
    Section {
        /// Target file (required).
        #[arg(short, long, required = true)]
        file: PathBuf,
        #[command(subcommand)]
        op: SectionOp,
    },
    /// Substring find/replace across a file, or within one section (`--scope`).
    Replace {
        file: PathBuf,
        #[arg(long)]
        find: String,
        #[arg(long)]
        new: String,
        /// Restrict to a section (default: the whole file).
        #[arg(long)]
        scope: Option<String>,
        /// Replace at most N occurrences (default: all).
        #[arg(long)]
        count: Option<usize>,
        #[command(flatten)]
        flags: WriteFlags,
    },
    /// Apply an *external* unified diff (from `diff -u`, `git diff`, or
    /// another tool) to a file. The patch is required as exactly one of
    /// three sources: `--patch-file` (a file path, or `-` for stdin) —
    /// `--patch-stdin` (`-` alone also works); `--patch-text` (a literal
    /// patch). The same write-path contract as `section` / `replace`
    /// applies (`--dry-run` / `--yes` / `--if-unchanged` / `--json`).
    ///
    /// Complementary to the *generate* direction (every write verb already
    /// emits standard `diff -u` output when `--diff` is given); `patch`
    /// closes the loop so an agent (or a human with an existing `.patch`)
    /// can *import* a diff instead of re-deriving it through the section
    /// verbs.
    Patch {
        /// Target file to mutate.
        file: PathBuf,
        /// Read the patch from this file (`-` = stdin).
        #[arg(long = "patch-file", value_name = "PATCHFILE")]
        patch_file: Option<PathBuf>,
        /// Read the patch from stdin.
        #[arg(long = "patch-stdin")]
        patch_stdin: bool,
        /// Inline patch (literal text). Patch files conventionally begin
        /// with `--- `, so this flag explicitly *allows* a leading dash in
        /// the value (otherwise clap reads it as a second flag).
        #[arg(long = "patch-text", value_name = "PATCH", allow_hyphen_values = true)]
        patch_text: Option<String>,
        #[command(flatten)]
        flags: WriteFlags,
    },
    /// Scaffold a new page (frontmatter + H1) and index it.
    Create {
        /// Page name. `.md` is appended when no extension is given; only a
        /// `.md` target is accepted (PLAN §3 Extensions).
        #[arg(value_name = "NAME")]
        filename: String,
        #[command(flatten)]
        args: CreateArgs,
    },
    /// Page lint + link check: is this a well-formed kb page? (DESIGN-verify.md)
    /// Phase 1: checks (a) markdown ext, (b) frontmatter framing, (c) required
    /// fields. Phase 2 adds (d) links and (e) label files.
    Verify {
        /// One file to check (a directory is a usage error).
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// Treat warnings as fatal (escalates `ok-with-warnings` → exit 1).
        #[arg(long)]
        strict: bool,
        /// Emit the single JSON verdict document on stdout.
        #[arg(long)]
        json: bool,
    },

    // ==================== multi-file namespace (M7) ====================
    /// Register a root and (re)index it. `--ext` adds extensions to the
    /// indexable set; `--include-hidden` also walks dot-directories.
    Add {
        path: PathBuf,
        /// Extra extensions to index (repeatable). `.md` is always indexed.
        #[arg(long, value_name = "EXT")]
        ext: Vec<String>,
        /// Also walk dot-directories.
        #[arg(long)]
        include_hidden: bool,
        /// Emit the result (root registered + refresh summary) as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List registered roots (with their stored walk options).
    Roots {
        /// Emit a JSON array instead of the text listing.
        #[arg(long)]
        json: bool,
    },
    /// Unregister a root (and its walk options). Pages still under another
    /// registered root survive; the rest are GC'd by the refresh that
    /// follows.
    DropRoot {
        path: PathBuf,
        /// Emit the result (root dropped + refresh summary) as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Reconcile all roots by `(mtime_ns, content_hash)`. `--full` wipes and
    /// re-indexes.
    Refresh {
        /// Wipe and re-index every registered root from scratch.
        #[arg(long)]
        full: bool,
        /// Emit the refresh summary as a JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Wipe and re-index every registered root (alias of `refresh --full`).
    FullRebuild {
        /// Emit the refresh summary as a JSON object.
        #[arg(long)]
        json: bool,
    },
    /// FTS search over indexed pages: `type:/language:` column filters,
    /// `tag:` (alias-expanded) filters, `~a ~b` OR, `-c` NOT, phrases —
    /// plus the `search_required_phrase` gate rule (gated pages hidden unless the
    /// query contains the full gate entry).
    Search {
        /// The raw query (one argument; quote it if it contains spaces).
        query: String,
        /// Emit a JSON envelope instead of the text listing.
        #[arg(long)]
        json: bool,
        /// At most N results (default 20).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Numbered per-page list under `[path]` (default: all roots): location,
    /// title, tags, search_required_phrase, link count; label pages expand to their
    /// referenced files/folder.
    Browse {
        /// Restrict to pages under this directory (relative to a root, or
        /// absolute). Default: everything registered.
        path: Option<PathBuf>,
        /// Emit a JSON array of page objects.
        #[arg(long)]
        json: bool,
    },
    /// Find every page tagged `<tag>` (alias-expanded). Label pages render
    /// as their referenced files/folder, not as themselves.
    Tag {
        tag: String,
        /// Emit a JSON array of page objects.
        #[arg(long)]
        json: bool,
    },
    /// Manage the tag-alias map: `--add <alias> --canonical <tag>`,
    /// `--rm <alias>`, `--list`, or the recovery pathway
    /// `--export`/`--import` (`--file`, else stdout/stdin).
    TagAlias {
        /// The alias to add (with `--canonical`).
        #[arg(long)]
        add: Option<String>,
        /// The canonical tag an alias maps to.
        #[arg(long)]
        canonical: Option<String>,
        /// The alias to remove.
        #[arg(long)]
        rm: Option<String>,
        /// List all aliases.
        #[arg(long)]
        list: bool,
        /// Export the alias map to a file (recovery backup; `--file` or stdout).
        #[arg(long)]
        export: bool,
        /// Import an alias map (wipe-safe recovery re-add): from `--file`, or
        /// from **stdin** when no file is given (so a redirect into the CLI
        /// works: `kb tag-alias --import < backup.tsv`).
        #[arg(long)]
        import: bool,
        /// The export/import file. `--export` with no `--file` writes to
        /// stdout; `--import` with no `--file` reads stdin.
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// Emit machine-readable JSON for the add/rm/list/import result.
        /// `--export` is unaffected (it is already a portable TSV payload).
        #[arg(long)]
        json: bool,
    },
    /// Per-occurrence link list for one page (default), or the dedup'd
    /// edge set + backlinks with `--dedup`.
    ///
    /// Default: the *occurrence set* — one numbered row per link occurrence
    /// in the page's body, in document order, each with its position
    /// (`line`/`col`), surrounding context, and its §4 resolution
    /// (`resolves_to` + `candidates`). This is what the TUI's link-picker
    /// and the agent's "follow the link next to my text" workflow consume;
    /// it is the per-occurrence view the dedup'd edge set cannot express.
    /// `--dedup`: the legacy *edge set* — one row per dedup'd
    /// `(target, style)` pair, with raw backlinks (the pre-rewire default).
    Links {
        /// A page path (absolute) or display id (`wiki:slug`).
        target: String,
        /// The legacy edge-set view: one row per dedup'd `(target, style)`
        /// pair, plus backlinks. Without it, the per-occurrence list (the
        /// default; the old `--list`).
        #[arg(long)]
        dedup: bool,
        /// Emit a JSON object (the link list, or forward links + backlinks
        /// with `--dedup`).
        #[arg(long)]
        json: bool,
    },
    /// Disambiguate a page reference into a candidate list (no error on
    /// collision).
    Resolve {
        query: String,
        /// Emit a JSON object (outcome + candidates).
        #[arg(long)]
        json: bool,
    },
    /// Show one page's record: id, type, title, description, tags,
    /// search_required_phrase, links, mtime.
    Info {
        path: PathBuf,
        /// Emit the page record as a JSON object (the full union, incl.
        /// links in/out, mtime, created).
        #[arg(long)]
        json: bool,
    },
    /// Per-root page counts, totals, DB size, last-refresh time.
    Stats {
        /// Emit a JSON object (roots + totals + db size).
        #[arg(long)]
        json: bool,
    },
    /// Print a paste-ready cron line for a scheduled refresh. With `--json`,
    /// the paste-ready line(s) are emitted as a JSON envelope (fields:
    /// `command`, `db`, `line`) so a script can consume them directly.
    Cron {
        /// Emit the refresh command as a JSON object instead of a shell line.
        #[arg(long)]
        json: bool,
    },
    /// Open the interactive TUI (M8). Browses the corpus, opens a page to
    /// read it, edits frontmatter (title / description / tags / search_required_phrase)
    /// with autocomplete, and manages tag aliases + roots. In-process —
    /// shares the same `Db` handle and the same `kb-core` + `kb-db` as every
    /// other verb.
    ///
    /// The user faces this when interactive navigation is needed; agents
    /// keep using the plain CLI verbs (which are scriptable and pipe-able).
    ///
    /// Gated behind the `tui` feature (default on): a hardened build omits
    /// this subcommand entirely and rejects `kb tui` as an unknown command.
    #[cfg(feature = "tui")]
    Tui {
         /// Color palette: a built-in name (`default` / `colorblind`, listed
         /// by `--list-palettes`) or a **path** to a TOML file (e.g.
         /// `~/.kb_config/default.toml`). When omitted, the default palette
         /// (the colorful out-of-the-box look) is used.
        /// Resolution happens before the TUI enters raw mode, so a typo /
        /// missing file / bad TOML prints a clean error and exits non-zero.
        #[arg(long, value_name = "NAME_OR_PATH")]
        palette: Option<String>,
        /// Swap the inline-code (`…`) span's color/box. A TUI can't change the
        /// terminal's *typeface* (it renders one monospace font), so this
        /// recolors the code role. Value syntax is the `[md_code]` TOML color
        /// spec: a single color name (foreground, no box) or `FG,BG` (a code
        /// box). E.g. `black,yellow` or `magenta`. Also settable via the
        /// `KB_TUI_CODE_STYLE` env var (this flag wins when both are given).
        /// A bad value prints a clean error and exits non-zero before raw mode.
        #[arg(long, value_name = "COLOR_OR_FG,BG")]
        code_style: Option<String>,
        /// List the built-in palettes (one per line: `<name>\t<description>`)
        /// and exit without entering the TUI or opening the DB.
        #[arg(long)]
        list_palettes: bool,
    },
    /// Generate a shell completion script for <shell> (`bash` | `zsh`).
    ///
    /// Self-contained: `kb complete <shell>` prints a completion script for
    /// `kb` itself, subcommand completions included. Install with
    /// `source <(kb complete bash)` (bash) or
    /// `fpath=(~/.local/share/kb $fpath); rehash` after
    /// `kb complete zsh > ~/.local/share/kb/_kb` (zsh). No external
    /// `completion` package required.
    ///
    /// Gated behind the `completions` feature (ON by default in `kb-user`,
    /// OFF by default in `kb-hardened`): the hardened build rejects the
    /// subcommand as unknown, mirroring the `tui` gate.
    #[cfg(feature = "completions")]
    Complete {
        /// Target shell. Exactly one of `bash`, `zsh`.
        #[arg(value_name = "SHELL")]
        shell: String,
        /// Write the script to this file instead of stdout.
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
    },
}

/// The section-level write verbs (all bind to the pure `kb-core::section` ops).
#[derive(Subcommand)]
pub(crate) enum SectionOp {
    /// Replace a section's full content (heading + body, incl. any subsections).
    /// For `fm` / `fm.<scalar_key>` refs (e.g. `fm.title`), `--value` (or the
    /// text source) is written as the scalar value of that key.
    Set {
        #[arg(value_name = "REF")]
        ref_: String,
        #[command(flatten)]
        text: TextSource,
        /// Keep the existing heading line; `text` is body-only.
        #[arg(long)]
        keep_heading: bool,
        /// For `fm` refs: the single value to write.
        #[arg(long, value_name = "VALUE")]
        value: Option<String>,
        #[command(flatten)]
        flags: WriteFlags,
    },
    /// Append text at the end of a section (before the next sibling heading).
    /// For `fm.<list_key>` refs (e.g. `fm.tags`), `--value` (or the text
    /// source) is appended as a list element.
    Append {
        #[arg(value_name = "REF")]
        ref_: String,
        #[command(flatten)]
        text: TextSource,
        /// For `fm.<list>` refs: the single value to append.
        #[arg(long, value_name = "VALUE")]
        value: Option<String>,
        #[command(flatten)]
        flags: WriteFlags,
    },
    /// Insert a brand-new section (heading + body) at a placement.
    Insert {
        /// Target for `before` / `after` / `child` (omit for `top` / `end`).
        #[arg(long, value_name = "REF")]
        ref_: Option<String>,
        /// New section's title (heading text).
        #[arg(long)]
        title: String,
        /// before | after | child | top | end (default: child).
        #[arg(long, default_value = "child")]
        placement: String,
        /// Heading level 1..6 (default: derived from placement + target).
        #[arg(long)]
        level: Option<u8>,
        /// New section's body.
        #[command(flatten)]
        text: TextSource,
        #[command(flatten)]
        flags: WriteFlags,
    },
    /// Replace a section's body, keeping its heading (the common case).
    Replace {
        #[arg(value_name = "REF")]
        ref_: String,
        #[command(flatten)]
        text: TextSource,
        #[command(flatten)]
        flags: WriteFlags,
    },
    /// Edit a section in $VISUAL/$EDITOR (fallback `vi`), then diff + confirm.
    /// SECURITY_REPORT 3.1 mitigation: gated behind the `editor` feature
    /// (default on). A hardened build compiles without this verb:
    /// `cargo build --no-default-features`.
    #[cfg(feature = "editor")]
    Edit {
        #[arg(value_name = "REF")]
        ref_: String,
        #[command(flatten)]
        flags: WriteFlags,
    },
    /// Remove a frontmatter field entirely (`rm fm.<key>`), or remove one
    /// element from a list field (`rm fm.<key> <value>`). `rm` on a plain
    /// section ref is not yet supported.
    Rm {
        #[arg(value_name = "REF")]
        ref_: String,
        /// For `fm.<list>` refs: the specific element to remove. Omit to
        /// delete the entire field (scalar or list).
        #[arg(value_name = "VALUE")]
        value: Option<String>,
        #[command(flatten)]
        flags: WriteFlags,
    },
}

pub(crate) fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Bare-string dispatch (PLAN §11). When exactly one non-flag argument is
    // given, it is not a subcommand: an existing `.md` file → `info`,
    // everything else → `search` (multi-token strings with spaces would have
    // been captured earlier by the shell as one argument and would also
    // trigger this path — treat as search).
    // A token is "non-flag" iff it does not start with `-`. (We deliberately do
    // **not** filter on `=`: a bare token such as `E=mc2` is a legitimate
    // single search term, and it is always reconstructed as `kb search "<token>"`
    // — one argument, never a `--flag=val` — so keeping it does not break clap
    // parsing. The real flags are still split off by the separate `starts_with('-')`
    // predicate below and re-appended to the reconstructed command.)
    let non_flag: Vec<&String> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .collect();
    // A bare search/info arg is a single non-flag token that is *not* itself a
    // known subcommand (those route normally, e.g. `kb roots`, `kb stats`).
    //
    // This list is a *name detector* only — it answers "is this token a real
    // subcommand (route to clap) or a bare string (route to info/search)?".
    // `tui` and `edit` are listed **unconditionally**: in the `user` build both
    // parse and run as normal; in the `hardened` build their `Cmd` variants
    // are `#[cfg(feature = ...)]`-compiled out, so clap rejects them itself
    // with `unrecognized subcommand` (stderr, exit 2) — rather than letting
    // this shortcut fall through to a stray "0 results" search.
    static SUBCMDS: &[&str] = &[
        "toc", "show", "section", "replace", "patch", "add", "roots",
        "drop-root", "refresh", "full-rebuild", "search", "browse", "tag",
        "tag-alias", "links", "resolve", "info", "stats", "cron",
        "tui",
        "create",
        "verify",
        // `complete` is only present when the `completions` feature is on
        // (ON by default in `kb-user`; OFF in `kb-hardened`). Listing it
        // unconditionally mirrors the `tui`/`edit` entries: in the hardened
        // build the variant is `cfg`-compiled out and clap rejects the
        // subcommand itself, rather than the bare-token shortcut falling
        // through to a stray "0 results" search.
        "complete",
        // section ops (also reachable bare in principle)
        "set", "append", "insert",
        "edit",
        "rm",
    ];
    if non_flag.len() == 1 {
        let s = non_flag
            .first()
            .expect("len()==1 guarantees non-empty");
        if !SUBCMDS.contains(&s.as_str()) {
        // Preserve any flags the caller passed (e.g. `--db PATH`), so the
        // reconstructed command runs against the same DB as an explicit one.
        let flags: Vec<String> = args
            .iter()
            .filter(|a| a.starts_with('-'))
            .cloned()
            .collect();
        let make = |verb: &str, tok: &String| -> Vec<String> {
            let mut v = vec!["kb".to_string(), verb.to_string(), tok.clone()];
            v.extend(flags.iter().cloned());
            v
        };
        let p = Path::new(s);
        // PENTEST_HARDENED F4 (option b). The `.md`→`info` shortcut is an
        // *interactive* convenience and — via the CWD-relative `p.exists()`
        // probe — the only state-dependent branch in this whole entry point.
        // For the hardened/agent build we drop it so a bare token *always*
        // routes to `search`: the agent surface is deterministic (agents that
        // want file details can say `kb info <file>` explicitly). The user
        // build keeps the shortcut: `kb notes/foo.md` opens that file. We
        // branch on `VARIANT`, the same "am I the agent build?" switch the
        // `name`/`version`/`long_about` above already use.
        let info_like = VARIANT == "user"
            && p.extension().map(|e| e == "md").unwrap_or(false)
            && p.exists();
        let cli = if info_like {
            Cli::parse_from(make("info", s))
        } else {
            Cli::parse_from(make("search", s))
        };
        let code = run(&cli);
        std::process::exit(code);
        }
    }
    let cli = Cli::parse();
    let code = run(&cli);
    std::process::exit(code);
}

pub(crate) fn run(cli: &Cli) -> i32 {
    let res: Kb = match &cli.cmd {
        Cmd::Toc { file, json } => cmd_toc(file, *json),
        Cmd::Show {
            file,
            ref_,
            body,
            full,
            json,
            etag,
        } => cmd_show(file, ref_.as_deref(), *body, *full, *json, *etag),
        Cmd::Section { file, op } => cmd_section(cli.db.as_deref(), file, op),
        Cmd::Replace {
            file,
            find,
            new,
            scope,
            count,
            flags,
        } => cmd_replace(cli.db.as_deref(), file, find, new, scope.as_deref(), *count, flags),
        Cmd::Patch {
            file,
            patch_file,
            patch_stdin,
            patch_text,
            flags,
        } => cmd_patch(cli.db.as_deref(), file, patch_file.as_deref(), *patch_stdin, patch_text.as_deref(), flags),
        Cmd::Create { filename, args } => cmd_create(cli.db.as_deref(), filename, args),
        Cmd::Verify { file, strict, json } => {
            cmd_verify(file, *strict, *json, cli.db.as_deref())
        }
        // ---- multi-file namespace (M7) ----
        Cmd::Add { path, ext, include_hidden, json } =>
            cmd_add(cli.db.as_deref(), path, ext, *include_hidden, *json),
        Cmd::Roots { json } => cmd_roots(cli.db.as_deref(), *json),
        Cmd::DropRoot { path, json } => cmd_drop_root(cli.db.as_deref(), path, *json),
        Cmd::Refresh { full, json } => cmd_refresh(cli.db.as_deref(), *full, *json),
        Cmd::FullRebuild { json } => cmd_refresh(cli.db.as_deref(), true, *json),
        Cmd::Search { query, json, limit } =>
            cmd_search(cli.db.as_deref(), query, *json, limit.unwrap_or(20)),
        Cmd::Browse { path, json } => cmd_browse(cli.db.as_deref(), path.as_deref(), *json),
        Cmd::Tag { tag, json } => cmd_tag(cli.db.as_deref(), tag, *json),
        Cmd::TagAlias { add, canonical, rm, list, export, import, file, json } => cmd_tag_alias(
            cli.db.as_deref(),
            &TagAliasOpts {
                add: add.clone(),
                canonical: canonical.clone(),
                rm: rm.clone(),
                list: *list,
                export: *export,
                import: *import,
                file: file.clone(),
            },
            *json,
        ),
        Cmd::Links { target, dedup, json } => cmd_links(cli.db.as_deref(), target, *dedup, *json),
        Cmd::Resolve { query, json } => cmd_resolve(cli.db.as_deref(), query, *json),
        Cmd::Info { path, json } => cmd_info(cli.db.as_deref(), path, *json),
        Cmd::Stats { json } => cmd_stats(cli.db.as_deref(), *json),
        // `tui` only exists when the `tui` feature is on (a hardened build
        // rejects the subcommand at parse time, so this arm is unreachable).
        #[cfg(feature = "tui")]
        Cmd::Tui { palette, code_style, list_palettes } => {
            let code = tui::run_tui(
                cli.db.as_deref(),
                palette.as_deref(),
                code_style.as_deref(),
                *list_palettes,
            );
            match code {
                EXIT_OK => Ok(EXIT_OK),
                c => Err(("tui exited unsuccessfully".into(), c)),
            }
        }
        #[allow(unused_variables)]
        #[cfg(feature = "completions")]
        Cmd::Complete { shell, file } => cmd_complete(shell, file.as_deref()),
        Cmd::Cron { json } => {
            let bin = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "kb".into());
            let dbp = cli
                .db
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(default_db_display);
            let q = shell_quote(&bin);
            let dq = shell_quote(&dbp);
            // The platform scheduler differs: POSIX uses `crontab`, Windows
            // uses Task Scheduler. Emit the matching invocation so the printed
            // line is paste-ready for the OS the binary was built for (W1).
            let (command, line, how) = if cfg!(windows) {
                (
                    format!("{q} {dq} refresh"),
                    format!(
                        "schtasks /create /tn \"kb-refresh\" /tr \"{q} {dq} refresh\" /sc hourly"
                    ),
                    "paste into `schtasks` (hourly)",
                )
            } else {
                (
                    format!("{q} {dq} refresh"),
                    format!("0 * * * * {q} {dq} refresh >> ~/.kb-cron.log 2>&1"),
                    "paste into `crontab -e` (hourly)",
                )
            };
            if *json {
                let mut o = JObject::new();
                o.s("command", &command);
                o.s("db", &dbp);
                o.s("line", &line);
                println!("{}", o.render());
            } else {
                println!("{command}");
                println!("# {how}:");
                println!("{line}");
            }
            Ok(EXIT_OK)
        }
    };
    match res {
        Ok(c) => c,
        Err((m, c)) => {
            eprintln!("kb: {m}");
            c
        }
    }
}

// ========================== M7 multi-file namespace ==========================

/// The clap command **for the `kb complete` surface**. Starts from the
/// derive-built CLI (so `name` / `version` / every subcommand + flag are
/// identical) and grafts a `PossibleValuesParser` on the `search <query>`
/// positional so the generated script completes the canonical filter keys
/// plus the `Null` absence sentinel.
///
/// This is a **separate instance** from the one `Cli::parse()` uses at
/// runtime: the runtime parser stays free-text (a possible-values
/// restriction would reject every query outside the allow-list, e.g.
/// `language:en` or an arbitrary search term). Splitting completion
/// decoration from parse behavior is the minimum-diff way to keep both
/// intact — the runtime surface is never constricted by the completion
/// hints.
#[cfg(feature = "completions")]
pub(crate) fn cli_command() -> clap::Command {
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    let Some(search) = cmd.find_subcommand_mut("search") else {
        return cmd;
    };
    let candidates = [
        "tag:Null",
        "type:Null",
        "language:Null",
        "type:wiki",
        "type:note",
        "type:label",
    ];
    // `mut_arg` is consuming (`mut self`), so we hand it an owned clone of
    // the `search` subcommand, graft the possible-values parser on `query`,
    // and write the result back. The runtime parse path keeps its free-text
    // parser (the derive-built `Cli::parse()` instance is untouched).
    *search = search
        .clone()
        .mut_arg("query", |a| {
            a.value_parser(clap::builder::PossibleValuesParser::new(candidates))
        });
    cmd
}

/// Implement `kb complete bash|zsh` (the `completions` feature).
/// Self-contained: no external `completion` package required — the
/// script is generated from the *same* clap command instance that
/// parses `kb`, so the completion surface can never drift from the
/// runtime surface.
#[cfg(feature = "completions")]
fn cmd_complete(shell: &str, file: Option<&Path>) -> Result<i32, (String, i32)> {
    use clap_complete::{generate, Shell};
    let target: Shell = match shell {
        "bash" => Shell::Bash,
        "zsh" => Shell::Zsh,
        other => {
            return Err((
                format!("complete: `bash` or `zsh` accepted, got `{other}`"),
                EXIT_ERR,
            ));
        }
    };
    let bin_name = std::env::args()
        .next()
        .and_then(|p| {
            // The binary path is `.../kb` or `.../kb-hardened`; take its
            // file stem. We use the *actual* binary name so the emitted
            // completion function names the right command.
            std::path::Path::new(&p)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "kb".to_string());
    let mut buf: Vec<u8> = Vec::new();
    generate(
        target,
        &mut cli_command(),
        bin_name,
        &mut buf,
    );
    // `generate` panics on write error; catch the panic-free path.
    match file {
        Some(p) => std::fs::write(p, &buf)
            .map_err(|e| (format!("complete: cannot write {}: {e}", p.display()), EXIT_ERR))?,
        None => {
            use std::io::Write;
            std::io::stdout().lock().write_all(&buf).map_err(|e| (format!("write: {e}"), EXIT_ERR))?;
        }
    }
    Ok(EXIT_OK)
}


pub(crate) fn open_db(path: Option<&Path>) -> Result<Db, (String, i32)> {
    let db = match path {
        Some(p) => {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| (format!("create {}: {e}", parent.display()), EXIT_ERR))?;
                }
            }
            Db::open(p).map_err(|e| (format!("open {p:?}: {e}"), EXIT_ERR))?
        }
        None => Db::open_default().map_err(|e| (format!("open default DB: {e}"), EXIT_ERR))?,
    };
    db.migrate().map_err(|e| (format!("migrate: {e}"), EXIT_ERR))?;
    Ok(db)
}

pub(crate) fn refresh_all(db: &Db, full: bool, json: bool) -> Kb {
    let summary = if full {
        kb_refresh::full_rebuild(db).map_err(|e| (format!("full-rebuild: {e}"), EXIT_ERR))?
    } else {
        kb_refresh::refresh(db).map_err(|e| (format!("refresh: {e}"), EXIT_ERR))?
    };
    let note = summary.note.clone();
    if json {
        // The machine-readable summary (counts) — the human `note` is omitted so
        // stdout is one clean JSON document.
        println!("{}", refresh_summary_json(full, &summary));
    } else {
        println!("{note}");
    }
    Ok(EXIT_OK)
}

/// `(path, seq)` for every registered root — the `winning_root` pick set
/// (PLAN §4: innermost, then most-recent, wins).
/// Re-index a single page after a successful write, so the index, FTS, and
/// `links` table stay consistent with disk (T6: auto-refresh after every
/// edit pathway). Mirrors `create.rs::index_created` — opens the DB, finds
/// the winning root, `build_page` re-reads the freshly-written file, and
/// `upsert_page` writes the row in one transaction.
///
/// Best-effort: a re-index failure (no roots, not indexable, no slug) is a
/// warning, not an error — disk is the source of truth and `kb refresh`
/// repairs later. Under `--json` the warning goes to stderr to keep stdout
/// a single clean JSON document.
fn reindex_after_write(dbpath: Option<&Path>, path: &Path, json: bool) -> Kb {
    let abs = match std::fs::canonicalize(path) {
        Ok(a) => a,
        Err(_) => return Ok(EXIT_OK),
    };
    let abs_str = abs.to_string_lossy().to_string();
    let db = match open_db(dbpath) { Ok(d) => d, Err(_) => return Ok(EXIT_OK) };
    let roots = match roots_with_seq(&db) { Ok(r) => r, Err(_) => return Ok(EXIT_OK) };
    if roots.is_empty() {
        if !json { eprintln!("kb: note: no roots registered — skipped re-index (run `kb add <dir>` first)"); }
        return Ok(EXIT_OK);
    }
    match kb_db::walk_snapshot::winning_root(&roots, &abs_str) {
        None => {
            if !json { eprintln!("kb: note: {abs_str} is not under any registered root — skipped re-index"); }
            Ok(EXIT_OK)
        }
        Some((root, rel)) => {
            match kb_db::index::build_page(&abs, &root, &rel, true) {
                Ok(rec) => match kb_db::index::upsert_page(&db, &rec) {
                    Ok(_) => Ok(EXIT_OK),
                    Err(e) => {
                        if !json { eprintln!("kb: note: re-index upsert failed: {e} (disk is authoritative; `kb refresh` repairs)"); }
                        Ok(EXIT_OK)
                    }
                },
                Err(e) => {
                    if !json { eprintln!("kb: note: page not indexable: {e} (disk is authoritative; `kb refresh` repairs)"); }
                    Ok(EXIT_OK)
                }
            }
        }
    }
}

pub(crate) fn roots_with_seq(db: &Db) -> Result<Vec<(String, i64)>, (String, i32)> {
    let mut stmt = db
        .conn
        .prepare("SELECT path, seq FROM roots ORDER BY seq")
        .map_err(|e| (format!("prepare roots: {e}"), EXIT_ERR))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| (format!("query roots: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("read roots: {e}"), EXIT_ERR))?;
    Ok(rows)
}

/// The `RefreshSummary` as a JSON object (or `None` if the refresh failed — the
/// caller maps the error before this is invoked).
pub(crate) fn refresh_summary_json(full: bool, s: &kb_db::refresh::RefreshSummary) -> String {
    let mut o = JObject::new();
    o.b("full", full);
    o.i("indexed", s.indexed as i64);
    o.i("skipped_no_type", s.skipped_no_type as i64);
    o.i("skipped_missing_slug", s.skipped_missing_slug as i64);
    o.i("gc", s.gc as i64);
    o.i("roots_walked", s.roots_walked as i64);
    o.render()
}

// ---- search ----

pub(crate) fn jstr_opt(s: &Option<String>) -> String {
    match s {
        Some(v) => jstr(v),
        None => "null".into(),
    }
}

/// A JSON boolean literal (no serde dep — `jstr`/`jstr_opt` above are the
/// only other helpers; literals like `true`/`false`/numbers are inlined).
pub(crate) fn jbool(b: bool) -> String {
    if b { "true".into() } else { "false".into() }
}

/// Load the identity projection of `pages` into `resolver::Page` rows:
/// the absolute path, the resolved slug (stored in `title`), the `type`, and
/// the root-relative path. Exactly the columns the §4 resolver reads — nothing
/// more — so a future schema addition that the resolver should ignore is
/// already filtered out here (and a column the resolver *needs* being absent
/// is an obvious prepare error, not a silent wrong-resolution).
///
/// C5: `pages.title` is `TEXT` (nullable in the DDL), but the indexer
/// guarantees every row that reaches `pages` has a non-NULL title — a page
/// whose `title → H1 → filename` chain can't produce a slug is *skipped* at
/// index time (`IndexError::MissingSlug`, see `kb-db/src/index.rs:188` and
/// `skipped_missing_slug` on the refresh summary). So a NULL title observed
/// at read-boundary is a schema/contract violation, not a valid state; we
/// surface it as an error (naming the offending page) rather than silently
/// coercing it to `""` and letting the §4 resolver's stage-2 bare-slug match
/// treat `""` as a real slug.
pub(crate) fn load_corpus(db: &Db) -> Result<Vec<resolver::Page>, (String, i32)> {
    // `body` is carried on `Page` for the T8 section-level gate (`section_present`
    // on the target page). `COALESCE(body,'')` mirrors the established caller
    // pattern in `links.rs` / `tui/mod.rs` so the row type stays `String`.
    let mut stmt = db.conn
        .prepare("SELECT abs_path, title, type, rel_path, COALESCE(body, '') FROM pages")
        .map_err(|e| (format!("prepare corpus: {e}"), EXIT_ERR))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| (format!("corpus query: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("corpus rows: {e}"), EXIT_ERR))?;
    let mut pages = Vec::with_capacity(rows.len());
    for (abs_path, slug, typ, rel_path, body) in rows {
        let slug = slug.ok_or_else(|| {
            (
                format!(
                    "corpus page '{}' has a NULL title (schema violation — the \
                     indexer skips slug-less pages before they reach `pages`)",
                    abs_path
                ),
                EXIT_ERR,
            )
        })?;
        pages.push(resolver::Page {
            abs_path,
            slug,
            typ,
            rel_path,
            body,
        });
    }
    Ok(pages)
}

/// Resolve a single raw link target to the `abs_path` of the page it points
/// at, or `None` if no page matches. A collision (two or more pages) picks
/// the first candidate (smallest `abs_path` after sort) — the §4 "no hard
/// error" contract, but for *link annotation* (the `links` view) we only
/// surface the single unambiguous page. The caller can check
/// `resolver::resolve` directly if it wants the full candidate set.
///
/// Intentional cross-surface difference: `resolve_target` (used by the
/// edge-set / graph view — `links.style` edge semantics, M7b) coerces
/// `Candidates → first` because a graph edge can only reference ONE target;
/// the deterministic `abs_path`-sort order makes the choice reproducible.
/// `annotated_links` (used by `kb links --list` human + JSON, and the TUI
/// picker) keeps the `candidates` field so that the user (or agent) can see
/// the *full* candidate set and choose — the picker opens the inline
/// candidate sub-state for exactly this case (see `LinksState.candidates`).
/// Both paths call the same `resolver::resolve`, so they never disagree on
/// *what* the candidate set is — only on how much of it they surface.
pub(crate) fn resolve_target(pages: &[resolver::Page], target: &str) -> Option<String> {
    match resolver::resolve(target, pages) {
        resolver::Resolution::Unique(abs) => Some(abs),
        resolver::Resolution::Candidates(v) => v.into_iter().next(),
        resolver::Resolution::Empty => None,
    }
}

/// A single reference extracted from a page body, **annotated** with its
/// position and resolution. Unlike `kb_core::links::Link` (an unpositioned
/// `{target, style}`), this carries:
///
/// - `line` / `col` — the *raw* body coordinate (0-based line index + byte
///   offset into the line where the **reference** starts, including its
///   open bracket). This is the `link_anchor` the TUI uses to pre-select
///   "the link nearest my find hit" (the find hit's `(line_idx, byte_offset)`
///   lives in the same row/col space, so the two are comparable for
///   nearest-neighbor).
/// - `context` — the containing raw line, trimmed. Kept because a page often
///   repeats a target (`[[LLM]]` in three sections); the context is what lets
///   a user (or agent) pick the occurrence they want when targets collide.
/// - `target` — the raw written target (inner text of `[[…]]`, the
///   parenthesized part of `[…](…)`, the bracketed text of `[…]`, URI inner
///   text of `<…>`). The *written* form, not the post-classification text.
/// - `kind` — the *classification* of the target (Db / Path / External /
///   Current), decided by `kb_core::links::classify_target`. Drives the
///   resolver behaviour (skip for External/Current, resolve for Db/Path) and
///   the render (`style_str()` → palette + `links.style` column).
/// - `label` — the optional display text (the inline label). `None` for
///   `[[X]]`, `[X]`, `<url>`; `Some(…)` for `[label](…)`. The renderer
///   uses this verbatim as the visible text on a *resolved* link.
/// - `section` — the `#section` (if any) on the target. Orthogonal to the
///   kind — a db target can have a section, a path target can have one,
///   and a *current-page* anchor is `kind=Current` + a section. Renderer
///   renders it as `  #  <section>` after the visible text.
/// - `resolves_to` — the page `abs_path` the target points at (`None` when
///   it is unresolved or ambiguous, or for `External`/`Current`).
/// - `candidates` — the collision set (`§4` "a collision is legal, never a
///   hard error"), sorted + dedup'd, when two or more pages match.
///
/// Pure over `body` + `pages` (no IO, no SQL): the *assumed* TUI, the CLI
/// `kb links --list`, and any agent surface all resolve a link through this
/// one function, so the TUI's "open the link I clicked" and the CLI's
/// "show me the link list" can never disagree.
#[derive(Debug, Clone)]
pub struct AnnotatedLink {
    /// 0-based index of the raw body line the reference starts on.
    pub line: usize,
    /// Byte offset within that line where the reference (including its
    /// wrappers) starts.
    pub col: usize,
    /// Byte offset within that line where the reference (including its
    /// wrappers) ends (exclusive). Used by the TUI to paint the whole
    /// reference (e.g. `[[wiki/target]]`) rather than just the inner
    /// target substring.
    pub end: usize,
    /// The containing raw line, trimmed — one-line surrounding context.
    pub context: String,
    /// The raw written target (per the form it was written as).
    pub target: String,
    /// The link **as written** in the line — `line[col..end]` (the full
    /// reference span: `[[target]]`, `[label](target)`, `<url>`). Used by
    /// the TUI to paint Dead / Ambiguous links exactly as the user typed
    /// them (normal text, no styling change).
    pub raw_text: String,
    /// The classification of the target (decides how it resolves + renders).
    pub kind: links::LinkKind,
    /// Optional display text (the inline label).
    pub label: Option<String>,
    /// Optional `#section` on the target.
    pub section: Option<String>,
    /// The page it resolves to, when a unique match exists.
    pub resolves_to: Option<String>,
    /// The candidate `abs_path`s (sorted) when the target is ambiguous.
    pub candidates: Vec<String>,
    /// `Some((typ, slug))` of the resolved page, for display / navigation.
    pub resolved_id: Option<(String, String)>,
}

/// Extract every **written link** in `body`, in document order, each
/// annotated with its position + resolution against `pages`.
///
/// The *scanner* is `kb_core::links::detect_refs` (syntax only: `[[…]]`,
/// `[…]`, `[…](…)`, `<…>`). The *classifier* is
/// `kb_core::links::classify_target` (the target's *kind* — Db / Path /
/// External / Current — plus an `#section` if one). Both are pure, and both
/// are already unit-tested + spec-proved in kb-core.
///
/// `pages` is the full corpus (every resolved page, e.g. from
/// [`load_corpus`]); it is read-only. A bare path token (a `notes/todo.md`
/// sitting in prose) is **not** a link under the new grammar — it is the
/// v1 heuristic, retired as part of the rework (DESIGN-links.md).
pub fn annotated_links(body: &str, pages: &[resolver::Page]) -> Vec<AnnotatedLink> {
    let mut out: Vec<AnnotatedLink> = Vec::new();
    for (li, line) in body.lines().enumerate() {
        for raw in links::detect_refs(line) {
            push_annotated(&mut out, li, line, raw.start, raw.end, &raw, pages);
        }
    }
    out
}

/// Classify a raw detected reference, resolve the page-part of the target,
/// and record one `AnnotatedLink`. `External` / `Current` skip the resolver
/// (they have no page to look up); `Db` / `Path` funnel through the *same*
/// `resolver::resolve` waterfall (the §4 contract) so a bare-title link and
/// a path link that both hit the same page cannot disagree.
fn push_annotated(
    out: &mut Vec<AnnotatedLink>,
    line: usize,
    line_text: &str,
    col: usize,
    end: usize,
    raw: &links::RawLink,
    pages: &[resolver::Page],
) {
    let cls = links::classify_target(raw);
    let base = AnnotatedLink {
        line,
        col,
        end,
        context: line_text.trim().to_string(),
        target: cls.target.clone(),
        raw_text: line_text[col..end].to_string(),
        kind: cls.kind,
        label: cls.label.clone(),
        section: cls.section.clone(),
        resolves_to: None,
        candidates: Vec::new(),
        resolved_id: None,
    };
    // `External` / `Current` are informational (the bare URI, or the page's
    // own `#section`); there is no page to resolve.
    if cls.kind == links::LinkKind::External
        || cls.kind == links::LinkKind::Current
    {
        out.push(base);
        return;
    }
    // The resolver consumes the *page-part* of the target: any `#section`
    // suffix is split off first (classify_target already split on the *first*
    // `#`, so the `#<section>` tail is an exact suffix of the written
    // target), and then — for the `[X]` inline form — the *inner* brackets
    // are unwrapped (classify_target kept the brackets on `target` as the
    // raw written form, but the resolver matches on slug/path; the brackets
    // are a *wrapper*, not part of the target).
    let pre_section = match &cls.section {
        Some(sec) => {
            let marker = format!("#{sec}");
            cls.target
                .strip_suffix(&marker)
                .unwrap_or(cls.target.as_str())
                .to_string()
        }
        None => cls.target.clone(),
    };
    let page_part = {
        let t = pre_section.trim();
        if t.len() >= 2 && t.starts_with('[') && t.ends_with(']') {
            t[1..t.len() - 1].trim().to_string()
        } else {
            t.to_string()
        }
    };
    // T8 — when the link has a `#section` tail and the page resolves *uniquely*,
    // the target page must *actually* contain that section. A missing (or
    // ambiguous) `#section` demotes `Unique` → a 1-candidate `Candidates` set so
    // the caller treats it as "needs disambiguation" (the page resolves; the
    // *section* does not). Links with no `#section` are unaffected.
    let resolution = resolver::resolve(&page_part, pages);
    let (resolves_to, candidates, resolved_id) = match resolution {
        resolver::Resolution::Unique(abs) => {
            let id = pages
                .iter()
                .find(|p| p.abs_path == abs)
                .map(|p| (p.typ.clone(), p.slug.clone()));
            let demote = match &cls.section {
                Some(sec) => {
                    let body = pages
                        .iter()
                        .find(|p| p.abs_path == abs)
                        .map(|p| p.body.as_str())
                        .unwrap_or("");
                    !resolver::section_present(body, sec)
                }
                None => false, // no `#section` tail — fully resolved
            };
            if demote {
                (None, vec![abs], None) // page found, section missing → ambiguous
            } else {
                (Some(abs), Vec::new(), id)
            }
        }
        resolver::Resolution::Candidates(v) => (None, v, None),
        resolver::Resolution::Empty => (None, Vec::new(), None),
    };
    out.push(AnnotatedLink {
        resolves_to,
        candidates,
        resolved_id,
        ..base
    });
}

/// Index of the link in `links` **nearest** the anchor `(anchor_line,
/// anchor_col)`, where "nearest" is `|line - anchor_line|` (primary) then
/// `|col - anchor_col|` (tiebreak). Returns `None` for an empty list.
///
/// Shared by the TUI Link-picker (pre-select the link closest to the last
/// find hit, so the `f` → type → `l` → `Enter` "follow this fact" path
/// starts at the right place) and any CLI consumer of a positioned link
/// list. Keyed on `(line, col)` pairs (not a concrete link type) so it is
/// generic over the `AnnotatedLink` / `LinksEntry` shapes.
pub fn nearest_link_index(
    links: &[(usize, usize)],
    anchor_line: usize,
    anchor_col: usize,
) -> Option<usize> {
    links
        .iter()
        .enumerate()
        .min_by_key(|(_i, &(l, c))| (l.abs_diff(anchor_line), c.abs_diff(anchor_col)))
        .map(|(i, _)| i)
}

pub(crate) fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return if cfg!(windows) { "\"\"".into() } else { "''".into() };
    }
    if cfg!(windows) {
        // W2: the scheduling shell is cmd.exe, which quotes with double quotes
        // (and a backslash escapes an inner quote). Single-quoting is a no-op
        // in cmd, so use the POSIX scheme only off-Windows.
        let needs_quote = s.chars().any(|c| c == ' ' || c == '"');
        if needs_quote {
            let inner = s.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{inner}\"")
        } else {
            s.to_string()
        }
    } else {
        let safe = s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.' || c == ':');
        if safe {
            s.to_string()
        } else {
            format!("'{}'", s.replace('\'', "'\\''"))
        }
    }
}

/// The display string for the default DB location. Prefers the real
/// (cross-platform) path via `dirs::data_local_dir()` (XDG_DATA_HOME on
/// POSIX, `%LOCALAPPDATA%` on Windows); only if that is unset does it fall
/// back to a `~` / `%VAR%` placeholder a human can read. One owner, so the
/// `cron` verb, `info`, and `Db::default_db_path()` all agree on *which file*
/// the tool would open by default, and the answer is right for the OS the
/// binary was built for (W8 — previously the fallback string was hard-coded
/// to a POSIX path and would have been a lie on Windows).
pub(crate) fn default_db_display() -> String {
    if let Some(d) = dirs::data_local_dir() {
        return d.join("knowledgebase").join("kb.db").display().to_string();
    }
    if cfg!(windows) {
        "%LOCALAPPDATA%\\knowledgebase\\kb.db".to_string()
    } else {
        "~/.local/share/knowledgebase/kb.db".to_string()
    }
} 

// ========================== M6 single-file namespace ==========================

/// Show the diff, confirm, re-verify the file, then write atomically. When
/// `json` is true: every human-readable thing (diff, "applied" / "no changes"
/// notes) goes to **stderr** so stdout carries exactly one JSON result object,
/// and the "apply?" prompt is suppressed (a JSON result is non-interactive —
/// the write is applied, or `--dry-run` holds it). The JSON result is
/// `{ "file", "applied", "changed", "dry_run", "bytes_before", "bytes_after",
/// "diff" }` — uniform across every section/replace/fm op.
pub(crate) fn commit(dbpath: Option<&Path>, path: &Path, orig: Vec<u8>, new: Vec<u8>, dry_run: bool, assume_yes: bool, json: bool) -> Kb {
    let old_s = String::from_utf8_lossy(&orig).into_owned();
    let new_s = String::from_utf8_lossy(&new).into_owned();
    let d = diff::unified_diff(&old_s, &new_s, &path.display().to_string(), "proposed");
    let changed = new != orig;

    let emit_json = |applied: bool| {
        let mut o = JObject::new();
        o.s("file", &path.display().to_string());
        o.b("changed", changed);
        o.b("dry_run", dry_run);
        o.b("applied", applied);
        o.i("bytes_before", orig.len() as i64);
        o.i("bytes_after", new.len() as i64);
        o.s("diff", &d);
        println!("{}", o.render());
    };

    if !changed {
        if json { emit_json(false); } else { println!("{}: no changes", path.display()); }
        return Ok(EXIT_OK);
    }

    // Human render: the diff to stdout (or stderr under --json).
    if !d.is_empty() {
        if json { eprint!("{d}"); } else { print!("{d}"); }
    }

    if dry_run {
        if json {
            eprintln!("kb: dry-run — {} left unchanged ({} → {} bytes)", path.display(), old_s.len(), new_s.len());
            emit_json(false);
        } else {
            eprintln!("kb: dry-run — {} left unchanged ({} → {} bytes)", path.display(), old_s.len(), new_s.len());
        }
        return Ok(EXIT_OK);
    }

    // `--json` implies an applied, non-interactive write.
    confirm("apply?", assume_yes || json)?;
    // Hash guard: the file on disk must still be exactly what we read —
    // i.e. nobody else wrote to it between our read and this write.
    let cur = std::fs::read(path).map_err(|e| (format!("re-read {}: {e}", path.display()), EXIT_ERR))?;
    if cur != orig {
        return Err((
            format!("{} changed on disk since we read it — re-run to retry", path.display()),
            EXIT_ERR,
        ));
    }
    atomic_write(path, &new).map_err(|e| (format!("write {}: {e}", path.display()), EXIT_ERR))?;
    // T6: auto-refresh — re-index the single page now that disk is updated,
    // so `links`/FTS/backlinks are consistent without a manual `kb refresh`.
    // `--dry-run` and no-change paths returned earlier, so this only fires on
    // a real write. Best-effort (a re-index miss is a note, not an error).
    reindex_after_write(dbpath, path, json)?;
    if json {
        emit_json(true);
    } else {
        println!("kb: wrote {} ({} bytes)", path.display(), new.len());
    }
    Ok(EXIT_OK)
}

// ============================== helpers (M6) ===============================

pub(crate) fn read_file(p: &Path) -> Result<Vec<u8>, (String, i32)> {
    std::fs::read(p).map_err(|e| (format!("read {}: {e}", p.display()), EXIT_ERR))
}

/// Resolve the 3-way text source to a `String`.
pub(crate) fn read_text(src: &TextSource, file: &Path) -> Result<String, (String, i32)> {
    let from_stdin = || {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| (format!("read stdin: {e}"), EXIT_ERR))?;
        Ok(s)
    };
    if src.stdin {
        from_stdin()
    } else if let Some(p) = &src.from_file {
        if p == Path::new("-") {
            from_stdin()
        } else {
            std::fs::read_to_string(p).map_err(|e| (format!("read {}: {e}", p.display()), EXIT_ERR))
        }
    } else if let Some(t) = &src.text {
        Ok(t.clone())
    } else {
        Err((
            format!(
                "one of --text / --from-file / --stdin is required (operation on {})",
                file.display()
            ),
            EXIT_ERR,
        ))
    }
}

pub(crate) fn parse_placement(s: &str) -> Result<SectionPlacement, (String, i32)> {
    use SectionPlacement::*;
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "before" => Before,
        "after" => After,
        "child" => Child,
        "top" => Top,
        "end" => End,
        other => {
            return Err((
                format!("--placement must be before|after|child|top|end (got {other:?})"),
                EXIT_ERR,
            ))
        }
    })
}

// SECURITY_REPORT 3.1 mitigation: the two helpers below are the entire
// $VISUAL/$EDITOR forking attack surface (an env var names an executable
// we then fork). They are compiled in only when the `editor` feature is on;
// a hardened build strips them at compile time:
//   cargo build -p kb-hardened   → binary with no editor calls

// Monotonic counter for the per-invocation editor buffer file name (pid alone
// is not unique: a competing `kb` on the same host, or a `chmod` race on a
// shared `/tmp`, can swap the buffer the editor edits). Combined with pid +
// nanos it yields a name no other process can guess (F8).
/// Interactive `y/N` prompt. Non-interactive stdin (piped / no TTY) never
/// auto-approves — the caller must pass `--yes`. On "no" (or a read failure)
/// returns the abort as a normal `(String, i32)` error, so a caller writes
/// `confirm(prompt, assume_yes)?;` instead of re-wrapping a `bool` — the
/// "aborted (not confirmed)" outcome and its `EXIT_ERR` live in exactly one
/// place (C1).
pub(crate) fn confirm(prompt: &str, assume_yes: bool) -> Result<(), (String, i32)> {
    if assume_yes {
        return Ok(());
    }
    use std::io::IsTerminal;
    let approved = std::io::stdin().is_terminal()
        && {
            print!("{prompt} [y/N]? ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map(|_| {
                    matches!(
                        line.trim().to_ascii_lowercase().as_str(),
                        "y" | "yes"
                    )
                })
                .unwrap_or(false)
        };
    if approved {
        Ok(())
    } else {
        Err(("aborted (not confirmed)".into(), EXIT_ERR))
    }
}

/// Write `bytes` to `path` atomically: same-directory temp file + rename.
///
/// SECURITY_REPORT 3.19 / CODEBASE_REVIEW F2: the temp name now comes from
/// `unique_tmp_in` (pid+seq+nanos) instead of a fixed, guessable
/// `.{name}.kb~tmp` opened with `O_CREAT|O_TRUNC`. In a hostile multi-user
/// directory a rival uid could pre-plant a symlink at the fixed name and
/// have the `rename` replace that symlink's target inode with our temp —
/// the classic `mktemp` race (the 3.2 editor pattern, now the write path).
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kb.out".to_string());
    let tmp = unique_tmp_in(dir, &name);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// FNV-1a 64-bit hash, rendered as 16 lowercase hex chars (the `etag`).
/// Thin wrapper over the single canonical implementation in `kb-core` so the
/// CLI (7 call sites) and the `kb-db` indexer share one definition — a fix to
/// the hash can no longer desync the `--if-unchanged` check across the two.
pub(crate) fn etag_of(b: &[u8]) -> String {
    kb_core::etag::of(b)
}

/// Lowercase a value and render it as a single-quoted SQLite literal for a
/// hand-built `IN (…)` list (SQLite's escape is doubling the quote). One
/// owner so the "did we escape?" and "did we case-fold?" questions are the
/// same answer at every call site — SECURITY_REPORT 3.4 (C4).
pub(crate) fn sql_quote_lower(s: &str) -> String {
    format!("'{}'", s.to_lowercase().replace('\'', "''"))
}

/// Minimal JSON string encoder (no external dep — `kb` stays a single
/// static binary).
pub(crate) fn jstr(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

#[cfg(test)]
mod json_tests {
    use super::*;

    /// Every escape branch of `jstr` must produce the canonical JSON
    /// sequence, and nothing else. This is the load-bearing bit of "we emit
    /// valid JSON without serde" — if any of these regresses, an agent
    /// `json.loads` will either fail or silently mis-parse.
    #[test]
    fn jstr_escapes_are_json_canonical() {
        // Quoting
        assert_eq!(jstr(""), "\"\"");
        assert_eq!(jstr("a\"b"), r#""a\"b""#);
        assert_eq!(jstr(r#"a\b"#), r#""a\\b""#);
        // Whitespace
        assert_eq!(jstr("a\nb"), r#""a\nb""#);
        assert_eq!(jstr("a\rb"), r#""a\rb""#);
        assert_eq!(jstr("a\tb"), r#""a\tb""#);
        // Control chars -> \uXXXX (lowercase hex, 4 digits, zero-padded)
        assert_eq!(jstr("\u{01}"), r#""\u0001""#);
        assert_eq!(jstr("\u{0a}"), r#""\n""#); // \n is the canonical short form
        assert_eq!(jstr("\u{0b}"), r#""\u000b""#);
        assert_eq!(jstr("\u{1f}"), r#""\u001f""#);
        assert_eq!(jstr("\u{0041}"), "\"A\""); // 0x41 is printable — not escaped
        // Unicode passes through verbatim (JSON permits raw non-ASCII)
        assert_eq!(jstr("π ∞ 日本語 é"), "\"π ∞ 日本語 é\"");
        // Multi-byte sequences don't break the surrounding quotes
        assert_eq!(jstr("hé \"quoted\""), "\"hé \\\"quoted\\\"\"");
    }

    /// `jbool` — the two literal states, total.
    #[test]
    fn jbool_is_total_and_canonical() {
        assert_eq!(jbool(true), "true");
        assert_eq!(jbool(false), "false");
    }

    /// A full `PageJson` with the nastiest realistic field values must render
    /// to a JSON object that (a) is well-formed (parses) and (b) round-trips
    /// every special char through `python3 json.loads` with the values intact.
    #[test]
    fn page_json_with_nasty_fields_is_well_formed() {
        let p = PageJson {
            id: "note:with \"quotes\" and a\\backslash".into(),
            pt: "note".into(),
            title: Some("line-one\nline-two\ttabbed".into()),
            abs: "/tmp/nasty/a \"a\".md".into(),
            rel: "a \"a\".md".into(),
            tags: vec!["with\"quote".into(), "back\\slash".into(), "uni-é".into()],
            gates: "two\\back\ttabs".into(),
            links: Some(3),
            refs: Some(vec!["x\ny".into()]),
            description: Some("a \"q\" and a \\ and \u{01}".into()),
        };
        let rendered = p.render();
        // -- exact-escape pinning (the byte-for-byte JSON output is the
        //    contract; a JSON parser alone would accept an *unescaped* quote
        //    and still parse, so we also check the raw bytes).
        let expect = [
            (r#""id": "note:with \"quotes\" and a\\backslash""#, "id"),
            (r#""title": "line-one\nline-two\ttabbed""#, "title"),
            (r#""description": "a \"q\" and a \\ and \u0001""#, "description"),
            (r#""tags": ["with\"quote","back\\slash","uni-é"]"#, "tags"),
            (r#""links": 3"#, "links"),
            (r#""refs": ["x\ny"]"#, "refs"),
        ];
        for (needle, key) in expect {
            assert!(
                rendered.contains(needle),
                "render must contain {needle:?} (key {key:?});\nrendered:\n{rendered}",
            );
        }
        // -- parse + field round-trip (the JSON spec authority).
        // The Python program reads the rendered element, parses it, and
        // asserts the decoded values match the original strings verbatim.
        let prog = r#"
import json, sys
d = json.loads('[' + sys.stdin.read().strip() + ']')[0]
assert d['id']          == 'note:with "quotes" and a\\backslash', d['id']
assert d['title']       == 'line-one\nline-two\ttabbed', d['title']
assert d['description'] == 'a "q" and a \\ and \x01', d['description'].encode()
assert d['tags']        == ['with"quote', 'back\\slash', 'uni-é'], d['tags']
assert d['search_required_phrase']== 'two\\back\ttabs', d['search_required_phrase'].encode()
assert d['links']       == 3, d['links']
assert d['refs']        == ['x\ny'], d['refs']
print('round-trip-ok')
"#;
        let mut cmd = std::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
        cmd.arg("-c").arg(prog);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let mut ch = cmd.spawn().unwrap();
        {
            use std::io::Write;
            ch.stdin.as_mut().unwrap().write_all(rendered.as_bytes()).unwrap();
        }
        let res = ch.wait_with_output().unwrap();
        assert!(
            res.status.success(),
            "PageJson with nasty fields must parse + round-trip via python3 json;\n\
             stdout: {}\nstderr: {}\nrendered:\n{}",
            String::from_utf8_lossy(&res.stdout),
            String::from_utf8_lossy(&res.stderr),
            rendered,
        );
        let out = String::from_utf8_lossy(&res.stdout);
        assert!(out.contains("round-trip-ok"), "python must print round-trip-ok; got: {out}");
    }
}

#[cfg(test)]
mod links_tests {
    use super::*;

    fn page(abs: &str, slug: &str, typ: &str) -> resolver::Page {
        page_with_body(abs, slug, typ, "")
    }

    fn page_with_body(abs: &str, slug: &str, typ: &str, body: &str) -> resolver::Page {
        resolver::Page {
            abs_path: abs.to_string(),
            slug: slug.to_string(),
            typ: typ.to_string(),
            rel_path: abs.rsplit('/').next().unwrap_or(abs).to_string(),
            body: body.to_string(),
        }
    }

    #[test]
    fn annotated_links_empty_body_is_empty() {
        assert!(annotated_links("", &[]).is_empty());
    }

    #[test]
    fn annotated_links_extracts_wikilink_with_position() {
        let pages = vec![page("/data/wiki/attention.md", "attention", "wiki")];
        let body = "The [[attention]] pattern is core.";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 1, "must extract exactly one wikilink");
        let l = &links[0];
        assert_eq!(l.line, 0, "single line → index 0");
        assert_eq!(l.col, 4, "col is where the *reference* (the `[[`) begins");
        assert_eq!(l.target, "attention");
        assert_eq!(l.resolves_to.as_deref(), Some("/data/wiki/attention.md"));
        assert!(l.candidates.is_empty(), "unique hit → no candidate set");
        assert_eq!(l.context, body, "single-line body → whole line is the context");
        assert!(matches!(l.kind, links::LinkKind::Db), "wikilink classifies as a Db ref");
        assert_eq!(l.kind.style_str(&l.target), "wikilink");
    }

    #[test]
    fn annotated_links_bare_path_token_is_not_a_link() {
        // The v1 heuristic (`is_pathlike`: `/`-in-token or known ext) is
        // retired — a bare path in prose is not a reference under the
        // DESIGN-links.md grammar. (DESIGN-links.md §non-goals: `` `…` `` /
        // bare tokens are inline code / prose, not links.)
        let pages = vec![page("/data/notes/todo.md", "todo", "note")];
        let body = "See notes/todo.md for the next step.";
        let links = annotated_links(body, &pages);
        assert!(links.is_empty(), "bare path token must not be a link");
    }

    #[test]
    fn annotated_links_plain_bracket_is_path() {
        let pages = vec![page("/data/a/b/c.md", "c", "note")];
        let body = "Full: [ /data/a/b/c.md ] end.";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 1);
        let l = &links[0];
        assert_eq!(l.target, "/data/a/b/c.md");
        assert!(matches!(l.kind, links::LinkKind::Path), "bracketed path classifies as Path");
        assert_eq!(l.kind.style_str(&l.target), "absolute");
        assert_eq!(l.resolves_to.as_deref(), Some("/data/a/b/c.md"));
    }

    #[test]
    fn annotated_links_inline_labeled_path() {
        let pages = vec![page("/data/a/b/c.md", "c", "note")];
        let body = "Full: [ the c page ]( /data/a/b/c.md ) end.";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 1);
        let l = &links[0];
        assert_eq!(l.target, "/data/a/b/c.md");
        let label = l.label.as_deref().expect("inline form must carry the label");
        assert_eq!(label, "the c page");
        assert!(matches!(l.kind, links::LinkKind::Path));
        assert_eq!(l.resolves_to.as_deref(), Some("/data/a/b/c.md"));
    }

    #[test]
    fn annotated_links_inline_labeled_db() {
        let pages = vec![page("/data/wiki/attention.md", "attention", "wiki")];
        let body = "see [ the note ]( [ attention ] ) for detail";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 1);
        assert!(matches!(links[0].kind, links::LinkKind::Db));
        assert_eq!(links[0].label.as_deref(), Some("the note"));
        assert_eq!(links[0].resolves_to.as_deref(), Some("/data/wiki/attention.md"));
    }

    #[test]
    fn annotated_links_external_uri_is_not_resolved() {
        let pages = vec![page("/data/wiki/attention.md", "attention", "wiki")];
        let body = "paper: < https://arxiv.org/abs/1706.03762 > is a real URI";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 1);
        assert!(matches!(links[0].kind, links::LinkKind::External));
        assert!(links[0].resolves_to.is_none(), "external is informational");
        assert!(links[0].candidates.is_empty());
    }

    #[test]
    fn annotated_links_current_section() {
        let body = "see [ # Cost / result ] for detail";
        let links = annotated_links(body, &[]);
        assert_eq!(links.len(), 1);
        assert!(matches!(links[0].kind, links::LinkKind::Current));
        assert_eq!(links[0].section.as_deref(), Some("Cost / result"));
    }

    #[test]
    fn annotated_links_other_page_with_section_splits_and_resolves() {
        // The target page's body *must* contain the `Decoder` and `Encoder`
        // sections, otherwise T8 demotes each link from Unique → ambiguous.
        let pages = vec![page_with_body(
            "/data/wiki/attention.md",
            "attention",
            "wiki",
            "# Attention\n\n## Decoder\ndecode details\n\n## Encoder\nencode details\n",
        )];
        let body = "see [[attention#Decoder]] and [x](/data/wiki/attention.md#Encoder)";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 2);
        let a = &links[0];
        assert_eq!(a.section.as_deref(), Some("Decoder"));
        assert_eq!(
            a.resolves_to.as_deref(),
            Some("/data/wiki/attention.md"),
            "page-part must resolve (section present)"
        );
        let b = &links[1];
        assert_eq!(b.section.as_deref(), Some("Encoder"));
        assert_eq!(
            b.resolves_to.as_deref(),
            Some("/data/wiki/attention.md"),
            "path+section link resolves when the section is present"
        );
    }

    #[test]
    fn annotated_links_multiple_on_same_line() {
        let pages = vec![
            page("/data/a.md", "a", "note"),
            page("/data/b.md", "b", "note"),
        ];
        let body = "[[a]] and [[b]] are both here.";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 2, "two distinct wikilinks on one line");
        assert!(links[0].col < links[1].col, "document order preserved");
        assert_eq!(links[0].line, links[1].line, "same line index");
    }

    #[test]
    fn annotated_links_multiple_lines() {
        let pages = vec![
            page("/data/a.md", "a", "note"),
            page("/data/b.md", "b", "note"),
        ];
        let body = "[[a]] on line one.\n\n[[b]] on line three.";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].line, 0, "first link on line 0 (0-indexed)");
        assert_eq!(links[1].line, 2, "second link on line 2 (0-indexed)");
    }

    #[test]
    fn annotated_links_unresolved_target() {
        let body = "Points at [[nonexistent-page]].";
        let links = annotated_links(body, &[]);
        assert_eq!(links.len(), 1);
        assert!(links[0].resolves_to.is_none(), "no corpus → must be unresolved");
        assert!(links[0].candidates.is_empty(), "no candidates (Empty, not Candidates)");
    }

    #[test]
    fn annotated_links_ambiguous_target_lists_candidates() {
        let pages = vec![
            page("/data/a/ambig.md", "ambig", "note"),
            page("/data/b/ambig.md", "ambig", "wiki"),
        ];
        let body = "Ambiguous: [[ambig]]";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 1);
        assert!(links[0].resolves_to.is_none(), "two matches → not unique");
        assert_eq!(links[0].candidates.len(), 2, "two candidate abs_paths");
    }

    // ---- T8: page resolves but `#section` is missing → ambiguous (1 candidate) ----

    #[test]
    fn t8_link_with_missing_section_demotes_to_ambiguous() {
        // target page "B" exists but does NOT have a section called "Foo"
        let target_page = page_with_body("/data/wiki/b.md", "b", "wiki", "# B\nbody only\n");
        let pages = vec![target_page];
        let body = "See [[b#Foo]] for details.";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 1);
        assert!(
            links[0].resolves_to.is_none(),
            "unique page + missing section → must NOT be resolves_to"
        );
        assert_eq!(
            links[0].candidates.len(),
            1,
            "demoted to a 1-element candidate set (the page it points at)"
        );
        assert_eq!(links[0].candidates[0], "/data/wiki/b.md");
        assert!(links[0].resolved_id.is_none(), "no resolved_id for a demoted link");
    }

    #[test]
    fn t8_link_with_present_section_stays_resolved() {
        // target page "B" DOES contain a section called "Foo"
        let target_page = page_with_body("/data/wiki/b.md", "b", "wiki", "# B\n\n## Foo\ncontent\n");
        let pages = vec![target_page];
        let body = "See [[b#Foo]] for details.";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].resolves_to.as_deref(),
            Some("/data/wiki/b.md"),
            "page + section both found → fully resolved"
        );
        assert!(links[0].candidates.is_empty(), "no candidates for a resolved link");
        assert!(links[0].resolved_id.is_some(), "resolved_id present");
    }

    #[test]
    fn t8_link_without_section_unaffected() {
        // no `#section` tail → the T8 section gate never fires
        let target_page = page_with_body("/data/wiki/b.md", "b", "wiki", "# B\nbody\n");
        let pages = vec![target_page];
        let body = "See [[b]] for details.";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].resolves_to.as_deref(), Some("/data/wiki/b.md"));
        assert!(links[0].candidates.is_empty());
        assert!(links[0].resolved_id.is_some());
    }

    #[test]
    fn t8_link_to_missing_page_stays_empty() {
        // page itself not found → Empty (no candidates), section gate is moot
        let pages: Vec<resolver::Page> = vec![];
        let body = "See [[b#Foo]] for details.";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 1);
        assert!(links[0].resolves_to.is_none());
        assert!(
            links[0].candidates.is_empty(),
            "missing page → Empty, not a 1-candidate demotion"
        );
        assert!(links[0].resolved_id.is_none());
    }

    #[test]
    fn t8_link_ambiguous_section_also_demotes() {
        // target page "B" has TWO sections named "Dup" → section itself is
        // ambiguous → section_lookup is `Err(Ambiguous)` → `section_present =
        // false` → same demotion as a missing section.
        let target_page = page_with_body(
            "/data/wiki/b.md",
            "b",
            "wiki",
            "# B\n\n## Dup\nx\n\n## Dup\ny\n",
        );
        let pages = vec![target_page];
        let body = "See [[b#Dup]] for details.";
        let links = annotated_links(body, &pages);
        assert_eq!(links.len(), 1);
        assert!(
            links[0].resolves_to.is_none(),
            "unique page + ambiguous section → must NOT be resolves_to"
        );
        assert_eq!(links[0].candidates.len(), 1, "demoted to 1 candidate");
    }

    #[test]
    fn nearest_link_index_empty_is_none() {
        assert_eq!(nearest_link_index(&[], 5, 5), None);
    }

    #[test]
    fn nearest_link_index_single() {
        assert_eq!(nearest_link_index(&[(0, 0)], 99, 99), Some(0));
    }

    #[test]
    fn nearest_link_index_picks_closest_line() {
        // lines 10, 20, 30 — anchor at 25: line-20 is 5 away, line-30 is 5 away;
        // tiebreak → both at the same |line-anchor|, take whichever min_by_key
        // sees first (stable by enumeration order).
        let links = vec![(10, 0), (20, 5), (30, 5)];
        let nearest = nearest_link_index(&links, 25, 0).unwrap();
        // Tie (5,_) vs (5,_) → first one wins (stable sort). That's index 1.
        assert!(nearest == 1 || nearest == 2, "closest must be row 1 or 2 (tie on line-dist)");
    }

    #[test]
    fn nearest_link_index_strong_line_preference() {
        // A "close" column match on a far line must LOSE to a "far" column on
        // a near line. The primary distance is |line - anchor_line|.
        let links = vec![(3, 5), (0, 1000)];
        let nearest = nearest_link_index(&links, 5, 15).unwrap();
        assert_eq!(nearest, 0, "line-3 is closer than line-0 regardless of column");
    }

    #[test]
    fn nearest_link_index_column_tiebreak() {
        let links = vec![(5, 0), (5, 50)];
        let nearest = nearest_link_index(&links, 5, 48).unwrap();
        assert_eq!(nearest, 1, "same line, col 50 → closer than col 0");
    }
}
