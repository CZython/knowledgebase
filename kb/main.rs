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

/// A success code, or `(message, exit-code)` for a typed failure. `?` over any
/// `Result<_, (String, i32)>` therefore unwinds the whole command with the
/// right exit code, so fallible steps compose cleanly.
pub(crate) type Kb = Result<i32, (String, i32)>;

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
    long_about = "kb is a single-binary Markdown knowledge base backed by an embedded\nSQLite + FTS5 index. Register roots of note files, search them (with\n`type:`/`tag:` filters, OR/NOT, phrases, and tag-alias expansion), and\ncorrectly-shaped edits that take a whole new section's bytes in place of\na diff, or apply external `diff -u` / `git diff` patches. All of the\nedit / resolve / alias / index logic is in a Verus-verified pure-Rust\ncrate; the DB shell is an assumed boundary. The full build ships a TUI\nplus an in-editor flow; the hardened build is headless for agents.",
)]
pub(crate) struct Cli {
    /// Path to the SQLite DB (default: `~/.local/share/knowledgebase/kb.db`).
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
    /// Forward links (raw targets + style) and backlinks for one page.
    Links {
        /// A page path (absolute) or display id (`wiki:slug`).
        target: String,
        /// Emit a JSON object (forward links + backlinks).
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
        /// List the built-in palettes (one per line: `<name>\t<description>`)
        /// and exit without entering the TUI or opening the DB.
        #[arg(long)]
        list_palettes: bool,
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
        if p.extension().map(|e| e == "md").unwrap_or(false) && p.exists() {
            let cli = Cli::parse_from(make("info", s));
            let code = run(&cli);
            std::process::exit(code);
        } else {
            let cli = Cli::parse_from(make("search", s));
            let code = run(&cli);
            std::process::exit(code);
        }
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
        Cmd::Section { file, op } => cmd_section(file, op),
        Cmd::Replace {
            file,
            find,
            new,
            scope,
            count,
            flags,
        } => cmd_replace(file, find, new, scope.as_deref(), *count, flags),
        Cmd::Patch {
            file,
            patch_file,
            patch_stdin,
            patch_text,
            flags,
        } => cmd_patch(file, patch_file.as_deref(), *patch_stdin, patch_text.as_deref(), flags),
        Cmd::Create { filename, args } => cmd_create(cli.db.as_deref(), filename, args),
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
        Cmd::Links { target, json } => cmd_links(cli.db.as_deref(), target, *json),
        Cmd::Resolve { query, json } => cmd_resolve(cli.db.as_deref(), query, *json),
        Cmd::Info { path, json } => cmd_info(cli.db.as_deref(), path, *json),
        Cmd::Stats { json } => cmd_stats(cli.db.as_deref(), *json),
        // `tui` only exists when the `tui` feature is on (a hardened build
        // rejects the subcommand at parse time, so this arm is unreachable).
        #[cfg(feature = "tui")]
        Cmd::Tui { palette, list_palettes } => {
            let code = tui::run_tui(cli.db.as_deref(), palette.as_deref(), *list_palettes);
            match code {
                EXIT_OK => Ok(EXIT_OK),
                c => Err(("tui exited unsuccessfully".into(), c)),
            }
        }
        Cmd::Cron { json } => {
            let bin = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "kb".into());
            let dbp = cli
                .db
                .as_ref()
                .map(|p| p.display().to_string())
                .or_else(|| Db::default_db_path().map(|p| p.display().to_string()))
                .unwrap_or_else(|| "~/.local/share/knowledgebase/kb.db".into());
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
                println!(
                    "{{\n  \"command\": {},\n  \"db\": {},\n  \"line\": {}\n}}",
                    jstr(&command),
                    jstr(&dbp),
                    jstr(&line),
                );
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
    format!(
        "{{\n  \"full\": {},\n  \"indexed\": {},\n  \"skipped_no_type\": {},\n  \"skipped_missing_slug\": {},\n  \"gc\": {},\n  \"roots_walked\": {}\n}}",
        jbool(full),
        s.indexed,
        s.skipped_no_type,
        s.skipped_missing_slug,
        s.gc,
        s.roots_walked,
    )
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
pub(crate) fn load_corpus(db: &Db) -> Result<Vec<resolver::Page>, (String, i32)> {
    let mut stmt = db.conn
        .prepare("SELECT abs_path, title, type, rel_path FROM pages")
        .map_err(|e| (format!("prepare corpus: {e}"), EXIT_ERR))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| (format!("corpus query: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("corpus rows: {e}"), EXIT_ERR))?;
    Ok(rows
        .into_iter()
        .map(|(abs_path, slug, typ, rel_path)| resolver::Page {
            abs_path,
            slug: slug.unwrap_or_default(),
            typ,
            rel_path,
        })
        .collect())
}

/// Resolve a single raw link target to the `abs_path` of the page it points
/// at, or `None` if no page matches. A collision (two or more pages) picks
/// the first candidate (smallest `abs_path` after sort) — the §4 "no hard
/// error" contract, but for *link annotation* (the `links` view) we only
/// surface the single unambiguous page. The caller can check
/// `resolver::resolve` directly if it wants the full candidate set.
pub(crate) fn resolve_target(pages: &[resolver::Page], target: &str) -> Option<String> {
    match resolver::resolve(target, pages) {
        resolver::Resolution::Unique(abs) => Some(abs),
        resolver::Resolution::Candidates(v) => v.into_iter().next(),
        resolver::Resolution::Empty => None,
    }
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

// ========================== M6 single-file namespace ==========================

/// Show the diff, confirm, re-verify the file, then write atomically. When
/// `json` is true: every human-readable thing (diff, "applied" / "no changes"
/// notes) goes to **stderr** so stdout carries exactly one JSON result object,
/// and the "apply?" prompt is suppressed (a JSON result is non-interactive —
/// the write is applied, or `--dry-run` holds it). The JSON result is
/// `{ "file", "applied", "changed", "dry_run", "bytes_before", "bytes_after",
/// "diff" }` — uniform across every section/replace/fm op.
pub(crate) fn commit(path: &Path, orig: Vec<u8>, new: Vec<u8>, dry_run: bool, assume_yes: bool, json: bool) -> Kb {
    let old_s = String::from_utf8_lossy(&orig).into_owned();
    let new_s = String::from_utf8_lossy(&new).into_owned();
    let d = diff::unified_diff(&old_s, &new_s, &path.display().to_string(), "proposed");
    let changed = new != orig;

    let emit_json = |applied: bool| {
        println!(
            "{{\n  \"file\": {},\n  \"changed\": {},\n  \"dry_run\": {},\n  \"applied\": {},\n  \"bytes_before\": {},\n  \"bytes_after\": {},\n  \"diff\": {}\n}}",
            jstr(&path.display().to_string()),
            jbool(changed),
            jbool(dry_run),
            jbool(applied),
            orig.len(),
            new.len(),
            jstr(&d),
        );
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
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kb.out".to_string());
    let tmp = dir.join(format!(".{name}.kb~tmp"));
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
