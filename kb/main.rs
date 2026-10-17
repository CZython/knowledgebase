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
use std::io::{Read, Write};
use std::path::{Path, PathBuf};


const EXIT_OK: i32 = 0;
const EXIT_ERR: i32 = 1;
const EXIT_SECTION: i32 = 2;
const EXIT_ETAG: i32 = 3;

/// A success code, or `(message, exit-code)` for a typed failure. `?` over any
/// `Result<_, (String, i32)>` therefore unwinds the whole command with the
/// right exit code, so fallible steps compose cleanly.
type Kb = Result<i32, (String, i32)>;

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
    name = if cfg!(feature = "editor") { "kb" } else { "kb-hardened" },
    version = if cfg!(feature = "editor") {
        env!("CARGO_PKG_VERSION")
    } else {
        concat!(env!("CARGO_PKG_VERSION"), " [hardened]")
    },
    about = "A single-binary Markdown knowledge base over embedded SQLite + FTS5 —\nindex a tree of notes, search them, and edit them by section.",
    long_about = "kb is a single-binary Markdown knowledge base backed by an embedded\nSQLite + FTS5 index. Register roots of note files, search them (with\n`type:`/`tag:` filters, OR/NOT, phrases, and tag-alias expansion), and\ncorrectly-shaped edits that take a whole new section's bytes in place of\na diff, or apply external `diff -u` / `git diff` patches. All of the\nedit / resolve / alias / index logic is in a Verus-verified pure-Rust\ncrate; the DB shell is an assumed boundary. The full build ships a TUI\nplus an in-editor flow; the hardened build is headless for agents.",
)]
struct Cli {
    /// Path to the SQLite DB (default: `~/.local/share/knowledgebase/kb.db`).
    #[arg(long, value_name = "PATH", global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

/// Flags shared by every mutating command.
#[derive(Args, Clone)]
struct WriteFlags {
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
struct CreateArgs {
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
struct TextSource {
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
enum Cmd {
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
    /// plus the `search_terms` gate rule (gated pages hidden unless the
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
    /// title, tags, search_terms, link count; label pages expand to their
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
    /// search_terms, links, mtime.
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
    /// read it, edits frontmatter (title / description / tags / search_terms)
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
    Tui,
}

/// The section-level write verbs (all bind to the pure `kb-core::section` ops).
#[derive(Subcommand)]
enum SectionOp {
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Bare-string dispatch (PLAN §11). When exactly one non-flag argument is
    // given, it is not a subcommand: an existing `.md` file → `info`,
    // everything else → `search` (multi-token strings with spaces would have
    // been captured earlier by the shell as one argument and would also
    // trigger this path — treat as search).
    let non_flag: Vec<&String> = args
        .iter()
        .filter(|a| !a.starts_with('-') && !a.contains('='))
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
    if non_flag.len() == 1 && !SUBCMDS.contains(&non_flag[0].as_str()) {
        let s = non_flag[0];
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
    let cli = Cli::parse();
    let code = run(&cli);
    std::process::exit(code);
}

fn run(cli: &Cli) -> i32 {
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
        Cmd::Tui => {
            let code = tui::run_tui(cli.db.as_deref());
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
                .unwrap_or_else(|| "~/.local/share/knowledgebase/kb.db".into());
            let q = shell_quote(&bin);
            let dq = shell_quote(&dbp);
            // The "command" is the bare refresh invocation; the "line" is the
            // paste-ready crontab fragment (hourly). Both are surfaced so a
            // script can pick whichever shape it wants.
            if *json {
                println!(
                    "{{\n  \"command\": {},\n  \"db\": {},\n  \"line\": {}\n}}",
                    jstr(&format!("{q} {dq} refresh")),
                    jstr(&dbp),
                    jstr(&format!("0 * * * * {q} {dq} refresh >> ~/.kb-cron.log 2>&1")),
                );
            } else {
                println!("{q} {dq} refresh");
                println!("# paste into `crontab -e`, e.g. hourly:");
                println!("0 * * * * {q} {dq} refresh >> ~/.kb-cron.log 2>&1");
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

fn open_db(path: Option<&Path>) -> Result<Db, (String, i32)> {
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

fn refresh_all(db: &Db, full: bool, json: bool) -> Kb {
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

fn cmd_add(dbpath: Option<&Path>, path: &Path, ext: &[String], include_hidden: bool, json: bool) -> Kb {
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| (format!("{path:?}: {e} (must exist and be a directory)"), EXIT_ERR))?;
    if !canonical.is_dir() {
        return Err((format!("{}: not a directory", canonical.display()), EXIT_ERR));
    }
    let extra_exts = if ext.is_empty() { None } else { Some(ext.join("\n")) };
    let db = open_db(dbpath)?;
    db.add_root(&canonical.to_string_lossy(), extra_exts.as_deref(), include_hidden)
        .map_err(|e| (format!("add_root: {e}"), EXIT_ERR))?;
    if json {
        let summary = kb_refresh::refresh(&db)
            .map_err(|e| (format!("refresh: {e}"), EXIT_ERR))?;
        println!(
            "{{\n  \"registered\": {},\n  \"refresh\": {}\n}}",
            jstr(&canonical.to_string_lossy()),
            refresh_summary_json(false, &summary),
        );
        return Ok(EXIT_OK);
    }
    println!("root registered: {}", canonical.display());
    refresh_all(&db, false, false)
}

/// `kb create <NAME>` — scaffold a new `.md` page and index it (PLAN §11).
///
/// Disk stays authoritative: the scaffold (plus whatever `-e` leaves behind)
/// is the source of truth, and the DB row is written with the *same*
/// `build_page`/`upsert_page` pair `kb refresh` uses, so a page created here
/// reconciles byte-identically on the next refresh. Indexing is **skipped
/// with a note, never an error** when: no roots are registered, the new page
/// sits outside every registered root, or the final bytes are not indexable
/// (§3/§4 `type:` warning contract).
fn cmd_create(dbpath: Option<&Path>, filename: &str, a: &CreateArgs) -> Kb {
    let dir = a.path.as_deref();
    let type_arg = &a.type_;
    let dry_run = a.dry_run;
    let yes = a.yes;
    let json = a.json;
    // The editor inherits our stdio, so it could pollute stdout — same
    // contract as `section edit --json` (§13p): an editor flow is not JSON.
    // SECURITY_REPORT 3.1: only reachable with `--edit`, which is itself an
    // editor-feature surface (a hardened build would reject it in clap);
    // gate the `edit` bindings so a hardened compile has no dangling reads.
    #[cfg(feature = "editor")]
    {
        let edit = a.edit;
        if edit && json {
            return Err((
                "`create --edit --json` is unsupported — `--edit` is an editor flow, not JSON (drop one of the two flags)".into(),
                EXIT_ERR,
            ));
        }
    }
    let kind = match NoteType::parse(type_arg) {
        Some(NoteType::Wiki) => "wiki",
        Some(NoteType::Note) => "note",
        Some(NoteType::Label) => "label",
        None => {
            return Err((
                format!("--type must be wiki|note|label (got {type_arg:?})"),
                EXIT_ERR,
            ))
        }
    };

    // ---- target path: [--path DIR] + NAME, `.md` appended when absent ----
    let name = filename.trim();
    if name.is_empty() {
        return Err(("create: NAME must not be empty".into(), EXIT_ERR));
    }
    if name.chars().any(|c| c == '\0' || c == '\n' || c == '\r') {
        return Err(("create: NAME must not contain newlines or NUL bytes".into(), EXIT_ERR));
    }
    let mut target = match dir {
        Some(d) => d.join(name),
        None => PathBuf::from(name),
    };
    match target.extension().and_then(|e| e.to_str()) {
        Some("md") => {}
        Some("") | None => {
            let mut os = target.clone().into_os_string();
            os.push(".md");
            target = PathBuf::from(os);
        }
        Some(other) => {
            return Err((
                format!(
                    "create: {name:?} is not a .md page (extension {other:?}) — PLAN §3 indexes `.md` only"
                ),
                EXIT_ERR,
            ));
        }
    }
    let title = target
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(("create: NAME must yield a non-empty page title".into(), EXIT_ERR));
    }
    // Work in absolute terms from here on: the diff label, the JSON `file`
    // and the `pages.abs_path` key all want the same string, and an agent
    // reading a relative `file` would have to re-anchor it on the cwd.
    if !target.is_absolute() {
        let cwd = std::env::current_dir()
            .map_err(|e| (format!("current dir: {e}"), EXIT_ERR))?;
        target = cwd.join(&target);
    }
    if target.exists() {
        return Err((format!("create: {} already exists", target.display()), EXIT_ERR));
    }

    // ---- the scaffold (PLAN §4: `type:` + `title:`, no H1 required) -------
    let scaffold = format!(
        "---\ntype: {kind}\ntitle: {title}\ndescription:\ntags: []\nsearch_terms: []\n---\n\n# {title}\n\n"
    );
    let d = diff::unified_diff("", &scaffold, "/dev/null", &target.display().to_string());

    // Human render of the new-file diff: stdout normally, stderr under
    // `--json` (stdout must carry exactly one JSON document — §13p).
    if !d.is_empty() {
        if json {
            eprint!("{d}");
        } else {
            print!("{d}");
        }
    }

    if dry_run {
        if json {
            let r = CreateReport {
                target: target.as_path(),
                kind,
                title: title.as_str(),
                id: None,
                created: false,
                edited: false,
                indexed: false,
                dry_run: true,
                bytes: scaffold.len(),
                diff: d.as_str(),
            };
            println!("{}", r.to_json());
        } else {
            eprintln!("kb: dry-run — {} not created", target.display());
        }
        return Ok(EXIT_OK);
    }

    if !confirm("create?", yes || json) {
        return Err(("aborted (not confirmed)".into(), EXIT_ERR));
    }

    // ---- write (atomic: same-dir temp + rename) ---------------------------
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| (format!("create {}: {e}", parent.display()), EXIT_ERR))?;
        }
    }
    atomic_write(&target, scaffold.as_bytes())
        .map_err(|e| (format!("write {}: {e}", target.display()), EXIT_ERR))?;
    if !json {
        println!("kb: created {} ({} bytes)", target.display(), scaffold.len());
    }

    // ---- optional in-place edit: the saved bytes are what we index --------
    // SECURITY_REPORT 3.1: `-e` / `a.edit` are editor-feature surface —
    // the `edit` field on `CreateArgs` only exists when the feature is on,
    // so in a hardened build clap rejects `-e` as an unexpected argument
    // before `cmd_create` sees it. The `edited` binding is a constant
    // `false` in that mode; the editor fork never happens.
    #[cfg(feature = "editor")]
    let edited = if a.edit {
        edit_in_place(&target)?;
        true
    } else {
        false
    };
    #[cfg(not(feature = "editor"))]
    let edited = false;

    // ---- index (best-effort; disk is authoritative, `kb refresh` repairs) --
    let db = open_db(dbpath)?;
    let roots = roots_with_seq(&db)?;
    let mut indexed = false;
    let mut id: Option<String> = None;
    let mut note: Option<String> = None;
    if roots.is_empty() {
        note = Some(
            "no roots registered — skipped indexing (run `kb add <dir>` first)".to_string(),
        );
    } else {
        let abs = std::fs::canonicalize(&target)
            .map_err(|e| (format!("resolve {}: {e}", target.display()), EXIT_ERR))?;
        let abs_s = abs.to_string_lossy().to_string();
        match kb_db::walk_snapshot::winning_root(&roots, &abs_s) {
            None => {
                note = Some(format!(
                    "{} is not under any registered root — skipped indexing (run `kb add <dir>`)",
                    abs.display()
                ));
            }
            Some((root, rel)) => match kb_db::index::build_page(&abs, &root, &rel, true) {
                Ok(rec) => match kb_db::index::upsert_page(&db, &rec) {
                    Ok(_) => {
                        id = Some(rec.id.clone());
                        indexed = true;
                    }
                    Err(e) => note = Some(format!("indexed skipped (DB upsert: {e})")),
                },
                Err(e) => note = Some(format!(
                    "not indexed ({e}) — fix the frontmatter, then `kb refresh`"
                )),
            },
        }
    }

    if json {
        let r = CreateReport {
            target: target.as_path(),
            kind,
            title: title.as_str(),
            id: id.as_deref(),
            created: true,
            edited,
            indexed,
            dry_run: false,
            bytes: scaffold.len(),
            diff: d.as_str(),
        };
        println!("{}", r.to_json());
    } else if indexed {
        if let Some(i) = &id {
            println!("kb: indexed as {i}");
        }
    }
    if let Some(n) = note {
        eprintln!("kb: {n}");
    }
    Ok(EXIT_OK)
}

/// The `create` result envelope — one JSON object on stdout (§13p), keys
/// stable across the dry-run / write paths so an agent can branch on
/// `indexed` / `dry_run` without parsing prose.
struct CreateReport<'a> {
    target: &'a Path,
    kind: &'a str,
    title: &'a str,
    id: Option<&'a str>,
    created: bool,
    edited: bool,
    indexed: bool,
    dry_run: bool,
    bytes: usize,
    diff: &'a str,
}

impl CreateReport<'_> {
    fn to_json(&self) -> String {
        format!(
            "{{\n  \"file\": {},\n  \"type\": {},\n  \"title\": {},\n  \"id\": {},\n  \"created\": {},\n  \"edited\": {},\n  \"indexed\": {},\n  \"dry_run\": {},\n  \"bytes\": {},\n  \"diff\": {}\n}}",
            jstr(&self.target.display().to_string()),
            jstr(self.kind),
            jstr(self.title),
            self.id.map(jstr).unwrap_or_else(|| "null".to_string()),
            jbool(self.created),
            jbool(self.edited),
            jbool(self.indexed),
            jbool(self.dry_run),
            self.bytes,
            jstr(self.diff),
        )
    }
}

/// `(path, seq)` for every registered root — the `winning_root` pick set
/// (PLAN §4: innermost, then most-recent, wins).
fn roots_with_seq(db: &Db) -> Result<Vec<(String, i64)>, (String, i32)> {
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

fn cmd_roots(dbpath: Option<&Path>, json: bool) -> Kb {
    let db = open_db(dbpath)?;
    let mut stmt = db.conn
        .prepare("SELECT path, added_at, extra_exts, include_hidden\nFROM roots\nORDER BY seq DESC, path")
        .map_err(|e| (format!("prepare: {e}"), EXIT_ERR))?;
    let roots = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i32>(3)?,
            ))
        })
        .map_err(|e| (format!("query: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("rows: {e}"), EXIT_ERR))?;

    if json {
        if roots.is_empty() {
            println!(r#"[ ]"#);
            return Ok(EXIT_OK);
        }
        let items: Vec<String> = roots.iter().map(|(path, added_at, extra_exts, include_hidden)| {
            let mut o = String::new();
            o.push_str(&format!("\n  {{\n    \"path\": {}", jstr(path)));
            if let Some(a) = added_at {
                o.push_str(&format!(",\n    \"added_at\": {}", jstr(a)));
            }
            if let Some(exts) = extra_exts {
                let v: Vec<String> = exts.lines().map(|l| l.to_string()).filter(|l| !l.is_empty()).collect();
                if !v.is_empty() {
                    let arr: Vec<String> = v.iter().map(|e| jstr(e)).collect();
                    o.push_str(&format!(",\n    \"ext\": [{}]", arr.join(",")));
                }
            }
            o.push_str(&format!(",\n    \"include_hidden\": {}", jbool(*include_hidden != 0)));
            o.push_str("\n  }\n");
            o
        }).collect();
        println!("[{}]", items.join(","));
        return Ok(EXIT_OK);
    }

    if roots.is_empty() {
        println!("(no roots registered — use `kb add <path>` to register one)");
        return Ok(EXIT_OK);
    }
    for (path, added_at, extra_exts, include_hidden) in roots {
        let mut opts: Vec<String> = Vec::new();
        let exts: Vec<String> = extra_exts
            .map(|s| s.lines().map(|l| l.to_string()).filter(|l| !l.is_empty()).collect())
            .unwrap_or_default();
        if !exts.is_empty() {
            opts.push(format!("ext:{}", exts.join(",")));
        }
        if include_hidden != 0 {
            opts.push("include-hidden".into());
        }
        let opt_str = if opts.is_empty() { String::new() } else { format!("  [{}]", opts.join(" ")) };
        println!("{path}{opt_str}  (added: {})", added_at.as_deref().unwrap_or("?"));
    }
    Ok(EXIT_OK)
}

fn cmd_drop_root(dbpath: Option<&Path>, path: &Path, json: bool) -> Kb {
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| (format!("{path:?}: {e}"), EXIT_ERR))?;
    let db = open_db(dbpath)?;
    let n = db.drop_root(&canonical.to_string_lossy())
        .map_err(|e| (format!("drop_root: {e}"), EXIT_ERR))?;
    if json {
        let summary = kb_refresh::refresh(&db)
            .map_err(|e| (format!("refresh: {e}"), EXIT_ERR))?;
        println!(
            "{{\n  \"dropped\": {},\n  \"dropped_rows\": {},\n  \"refresh\": {}\n}}",
            jstr(&canonical.to_string_lossy()),
            n,
            refresh_summary_json(false, &summary),
        );
        return Ok(EXIT_OK);
    }
    if n == 0 {
        println!("no root matched {} — nothing dropped", canonical.display());
    } else {
        println!("dropped root: {}", canonical.display());
    }
    refresh_all(&db, false, false)
}

fn cmd_refresh(dbpath: Option<&Path>, full: bool, json: bool) -> Kb {
    let db = open_db(dbpath)?;
    refresh_all(&db, full, json)
}

/// The `RefreshSummary` as a JSON object (or `None` if the refresh failed — the
/// caller maps the error before this is invoked).
fn refresh_summary_json(full: bool, s: &kb_db::refresh::RefreshSummary) -> String {
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

fn search_term_text(t: &search_grammar::Term) -> String {
    match t {
        search_grammar::Term::Token { text, .. } => text.clone(),
    }
}

#[allow(dead_code)]
struct SearchRow {
    abs_path: String,
    id: String,
    pt: String,
    title: Option<String>,
    description: Option<String>,
    root: String,
    rel_path: String,
    search_terms: Option<String>,
    score: Option<f64>,
}

fn cmd_search(dbpath: Option<&Path>, query: &str, json: bool, limit: usize) -> Kb {
    let parsed = search_grammar::parse(query);
    let has_and = !parsed.and_terms.is_empty();
    let has_or = !parsed.or_group.is_empty();
    let has_not = !parsed.not_terms.is_empty();
    let has_match = has_and || has_or;

    let gate_text: String = {
        let mut parts: Vec<String> = Vec::new();
        for t in &parsed.and_terms { parts.push(search_term_text(t)); }
        for t in &parsed.or_group { parts.push(search_term_text(t)); }
        for t in &parsed.not_terms { parts.push(search_term_text(t)); }
        parts.join(" ")
    };

    let db = open_db(dbpath)?;

    // tag filters need corpus + alias data (loaded outside the main query)
    // SECURITY_REPORT 3.17: hoist the alias + distinct-tag loads outside the
    // per-filter loop — N tag filters used to trigger N full table scans of
    // the same data.
    let has_tag_filter = parsed.filters.iter().any(|f| f.is_tag());
    let (aliases, all_tags) = if has_tag_filter {
        let aliases: Vec<(String, String)> = {
            let mut stmt = db.conn.prepare("SELECT alias, canonical FROM tag_aliases")
                .map_err(|e| (format!("prepare aliases: {e}"), EXIT_ERR))?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| (format!("aliases: {e}"), EXIT_ERR))?
                .collect::<Result<_, _>>()
                .map_err(|e| (format!("aliases rows: {e}"), EXIT_ERR))?;
            rows
        };
        let all_tags: Vec<String> = {
            let mut stmt = db.conn.prepare("SELECT DISTINCT tag FROM page_tags")
                .map_err(|e| (format!("prepare tags: {e}"), EXIT_ERR))?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| (format!("tags: {e}"), EXIT_ERR))?
                .collect::<Result<_, _>>()
                .map_err(|e| (format!("tag rows: {e}"), EXIT_ERR))?;
            rows
        };
        (aliases, all_tags)
    } else {
        (Vec::new(), Vec::new())
    };
    let mut tag_match_sets: Vec<Vec<String>> = Vec::new();
    for f in &parsed.filters {
        if f.is_tag() {
            let tag_val = f.value.clone();
            let match_set: Vec<String> = kbtags::canonicalize(&tag_val, &all_tags, &aliases)
                .into_iter().map(|s: &str| s.to_string()).collect();
            if match_set.is_empty() {
                if !json {
                    println!("0 results (no pages tagged {tag_val:?} in the corpus)");
                } else {
                    println!("[{{}}]");
                }
                return Ok(EXIT_OK);
            }
            tag_match_sets.push(match_set);
        }
    }

    let mut params_vec: Vec<String> = Vec::new(); // all params are TEXT for v1
    let mut wheres: Vec<String> = Vec::new();

    let fts_render = parsed.render_fts();
    // A NOT-only query (no AND, no OR) can't be expressed as a single FTS5
    // MATCH string (leading NOT is rejected). SECURITY_REPORT 3.5: this was
    // previously a *silent no-op* — the query returned the whole corpus up to
    // `LIMIT` with a reassuring note. We keep the FTS join active and add one
    // `NOT (pages_fts MATCH <term>)` clause per NOT term instead, the same
    // exclusion FTS5's own NOT gives when a positive term is present.
    let not_only = !has_match && has_not;
    let fts_active = has_match && !fts_render.is_empty() || not_only;
    if has_match && !fts_render.is_empty() {
        params_vec.push(fts_render);
        wheres.push(format!("pages_fts MATCH ?{}", params_vec.len()));
    }
    if not_only {
        for t in &parsed.not_terms {
            // FTS5 `MATCH` must appear as a top-level WHERE predicate of a
            // SELECT that directly queries the FTS5 table — `NOT (pages_fts
            // MATCH ?)` in an outer WHERE on a non-FTS5 main table is rejected
            // ("unable to use function MATCH in the requested context"). The
            // subquery satisfies the requirement: its FROM is the FTS5 table.
            params_vec.push(search_term_text(t));
            wheres.push(format!(
                "NOT EXISTS (SELECT 1 FROM pages_fts WHERE rowid = pages.rowid \
                 AND pages_fts MATCH ?{})",
                params_vec.len()
            ));
        }
    }

    for f in &parsed.filters {
        if !f.is_tag() {
            params_vec.push(f.value.clone());
            wheres.push(format!(
                "lower(pages.{}) = lower(?{})",
                f.key, params_vec.len()
            ));
        }
    }

    for match_set in tag_match_sets.iter() {
        // SECURITY_REPORT 3.4: the left side of the comparison is
        // `lower(pt.tag)`, so the IN-list must be lowercased too. `canonicalize`
        // returns the *stored* strings in their original casing (that is how
        // case/alias folding is decided), but a stored mixed-case tag like
        // `Czech` would then fail `lower('Czech') IN ('Czech',…)` → `'czech'
        // IN ('Czech',…)` → no match. Folding the IN-list to lowercase makes
        // both sides of the comparison the same case-fold.
        let in_list: Vec<String> = match_set
            .iter()
            .map(|t| format!("'{}'", t.to_lowercase().replace('\'', "''")))
            .collect();
        wheres.push(format!(
            "EXISTS (SELECT 1 FROM page_tags pt WHERE pt.abs_path = pages.abs_path\nAND lower(pt.tag) IN ({}))",
            in_list.join(",")
        ));
    }

    if gate_text.is_empty() {
        wheres.push(
            "NOT EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = pages.abs_path)".into(),
        );
    } else {
        params_vec.push(gate_text.clone());
        wheres.push(format!(
            "(NOT EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = pages.abs_path)\n\
             OR EXISTS (SELECT 1 FROM page_gates g WHERE g.abs_path = pages.abs_path\n\
             AND lower(?{}) LIKE '%' || lower(g.term) || '%'))",
            params_vec.len()
        ));
    }

    let mut sql = String::new();
    if fts_active {
        sql.push_str(
            "SELECT pages.abs_path, pages.id, pages.type,\n\
              pages.title, pages.description, pages.root, pages.rel_path,\n\
              pages.search_terms, bm25(pages_fts) AS score\n"
        );
        sql.push_str(
            "FROM pages\n\
              JOIN pages_fts ON pages.rowid = pages_fts.rowid\n"
        );
    } else {
        sql.push_str(
            "SELECT pages.abs_path, pages.id, pages.type,\n\
             pages.title, pages.description, pages.root, pages.rel_path,\n\
             pages.search_terms, NULL AS score\n"
        );
        sql.push_str("FROM pages\n");
    }
    sql.push_str("WHERE ");
    sql.push_str(&wheres.join("\nAND "));
    if fts_active {
        sql.push_str("\nORDER BY score ASC");
    } else {
        sql.push_str("\nORDER BY pages.title COLLATE NOCASE");
    }
    sql.push_str(&format!("\nLIMIT {limit}"));

    let db_ref = &db;
    let mut stmt = db_ref.conn.prepare(&sql)
        .map_err(|e| (format!("prepare search: {e}\nSQL: {sql}"), EXIT_ERR))?;
    let rows: Vec<SearchRow> = stmt
        .query_map(rusqlite::params_from_iter(params_vec.iter()), |r| {
            Ok(SearchRow {
                abs_path: r.get::<_, String>(0)?,
                id: r.get::<_, String>(1)?,
                pt: r.get::<_, String>(2)?,
                title: r.get::<_, Option<String>>(3)?,
                description: r.get::<_, Option<String>>(4)?,
                root: r.get::<_, String>(5)?,
                rel_path: r.get::<_, String>(6)?,
                search_terms: r.get::<_, Option<String>>(7)?,
                score: r.get::<_, Option<f64>>(8)?,
            })
        })
        .map_err(|e| (format!("search query: {e}"), EXIT_ERR))?
        .collect::<Result<_, _>>()
        .map_err(|e| (format!("search rows: {e}"), EXIT_ERR))?;

    if json {
        let items: Vec<String> = rows.iter().map(|r| {
            let score = match r.score {
                Some(s) => format!("{s:.6}"),
                None => "null".to_string(),
            };
            format!(
                "{{\"id\":{},\"type\":{},\"title\":{},\"description\":{},\"abs_path\":{},\"rel_path\":{},\"score\":{score}}}",
                jstr(&r.id),
                jstr(&r.pt),
                jstr_opt(&r.title),
                jstr_opt(&r.description),
                jstr(&r.abs_path),
                jstr(&r.rel_path),
            )
        }).collect();
        println!("[{}]", items.join(","));
    } else {
        if rows.is_empty() {
            println!("0 results for {query:?} (the query was too strict, or the page is gated)");
        } else {
            for (i, r) in rows.iter().enumerate() {
                let title = r.title.as_deref().unwrap_or("");
                let desc = r.description.as_deref().unwrap_or("");
                let tail = if desc.is_empty() {
                    r.rel_path.clone()
                } else {
                    format!("{desc}  ({})", r.rel_path)
                };
                println!("{:>3}  {:<24} {}  {}", i + 1, r.id, title, tail);
            }
            println!("── {} result(s)", rows.len());
        }
    }
    Ok(EXIT_OK)
}

fn jstr_opt(s: &Option<String>) -> String {
    match s {
        Some(v) => jstr(v),
        None => "null".into(),
    }
}

/// A JSON boolean literal (no serde dep — `jstr`/`jstr_opt` above are the
/// only other helpers; literals like `true`/`false`/numbers are inlined).
fn jbool(b: bool) -> String {
    if b { "true".into() } else { "false".into() }
}

/// A single page record as a JSON object. Emits the *union* of the fields an
/// agent might want; the optional ones are omitted when absent so the output
/// stays honest (no `null` for a field that simply does not apply). Shared by
/// `browse`, `tag`, `info`, and `search` so the page shape is one thing.
struct PageJson {
    id: String,
    pt: String,
    title: Option<String>,
    abs: String,
    rel: String,
    tags: Vec<String>,
    gates: String,
    /// `None` = omit the field (e.g. `search` has no link count).
    links: Option<i64>,
    /// `None` = omit (non-label pages have no expansion).
    refs: Option<Vec<String>>,
    /// `None` = omit (`browse`/`tag` do not show the description; `info` does).
    description: Option<String>,
}

impl PageJson {
    fn render(&self) -> String {
        let mut o = String::with_capacity(256);
        o.push_str(&format!("\n  {{\n    \"id\": {}", jstr(&self.id)));
        o.push_str(&format!(",\n    \"type\": {}", jstr(&self.pt)));
        match &self.title {
            Some(t) if !t.is_empty() => o.push_str(&format!(",\n    \"title\": {}", jstr(t))),
            _ => {}
        }
        o.push_str(&format!(",\n    \"abs_path\": {}", jstr(&self.abs)));
        if !self.rel.is_empty() {
            o.push_str(&format!(",\n    \"rel_path\": {}", jstr(&self.rel)));
        }
        if let Some(d) = &self.description {
            if !d.is_empty() {
                o.push_str(&format!(",\n    \"description\": {}", jstr(d)));
            }
        }
        if !self.tags.is_empty() {
            let items: Vec<String> = self.tags.iter().map(|t| jstr(t)).collect();
            o.push_str(&format!(",\n    \"tags\": [{}]", items.join(",")));
        }
        if !self.gates.is_empty() {
            o.push_str(&format!(",\n    \"search_terms\": {}", jstr(&self.gates)));
        }
        if let Some(n) = self.links {
            o.push_str(&format!(",\n    \"links\": {n}"));
        }
        if let Some(refs) = &self.refs {
            let items: Vec<String> = refs.iter().map(|r| jstr(r)).collect();
            o.push_str(&format!(",\n    \"refs\": [{}]", items.join(",")));
        }
        o.push_str("\n  }\n");
        o
    }
}

// ---- browse ----

fn page_tags_str(db: &Db, abs_path: &str) -> String {
    let mut stmt = match db.conn.prepare("SELECT tag FROM page_tags WHERE abs_path = ?1 ORDER BY tag") {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let mut out = Vec::new();
    if let Ok(it) = stmt.query_map(params![abs_path], |r| r.get::<_, String>(0)) {
        for t in it.flatten() {
            out.push(t);
        }
    }
    out.join(",")
}

fn page_link_count(db: &Db, abs_path: &str) -> i64 {
    db.conn
        .query_row(
            "SELECT COUNT(*) FROM links WHERE from_path = ?1",
            params![abs_path],
            |r| r.get(0),
        )
        .unwrap_or(0)
}

fn label_expand(db: &Db, abs_path: &str) -> Vec<String> {
    let body: Option<String> = db
        .conn
        .query_row("SELECT body FROM pages WHERE abs_path = ?1", params![abs_path], |r| r.get(0))
        .ok()
        .flatten();
    let Some(body) = body else { return vec![abs_path.to_string()] };
    match labels::classify(&body) {
        labels::LabelRef::Files(refs) => {
            let dir = Path::new(abs_path)
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_string_lossy()
                .to_string();
            refs.iter().map(|r| {
                let p = Path::new(&dir).join(r);
                p.to_string_lossy().to_string()
            }).collect()
        }
        labels::LabelRef::Folder => {
            let dir = Path::new(abs_path)
                .parent()
                .unwrap_or_else(|| Path::new("."));
            enumerate_non_recursive(dir)
        }
    }
}

fn enumerate_non_recursive(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return out,
    };
    for entry in rd.flatten() {
        out.push(entry.path().to_string_lossy().to_string());
    }
    out.sort();
    out
}

/// A `kb browse` row: the seven display columns of a `pages` row. (Named so
/// the local binding below stays readable without inlining the tuple type.)
type BrowseRow = (String, String, String, Option<String>, String, String, Option<String>);

fn cmd_browse(dbpath: Option<&Path>, path: Option<&Path>, json: bool) -> Kb {
    let db = open_db(dbpath)?;

    let filter_prefix: Option<String> = path
        .map(|p| {
            let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            c.to_string_lossy().to_string()
        });

    let map_row = |r: &rusqlite::Row| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    };
    let pages: Vec<BrowseRow> =
        match &filter_prefix {
            Some(prefix) => {
                let like = format!("{prefix}/%");
                let mut stmt = db.conn
                    .prepare(
                        "SELECT abs_path, id, type, title, root, rel_path, search_terms\n\
                         FROM pages\n\
                         WHERE abs_path LIKE ?1 OR abs_path = ?2\n\
                         ORDER BY abs_path",
                    )
                    .map_err(|e| (format!("prepare browse: {e}"), EXIT_ERR))?;
                let rows = stmt
                    .query_map(params![like, prefix], &map_row)
                    .map_err(|e| (format!("browse query: {e}"), EXIT_ERR))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| (format!("browse rows: {e}"), EXIT_ERR))?;
                rows
            }
            None => {
                let mut stmt = db.conn
                    .prepare(
                        "SELECT abs_path, id, type, title, root, rel_path, search_terms\n\
                         FROM pages\n\
                         ORDER BY root, rel_path",
                    )
                    .map_err(|e| (format!("prepare browse: {e}"), EXIT_ERR))?;
                let rows = stmt
                    .query_map([], &map_row)
                    .map_err(|e| (format!("browse query: {e}"), EXIT_ERR))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| (format!("browse rows: {e}"), EXIT_ERR))?;
                rows
            }
        };

    // JSON: a clean array of page objects.
    if json {
        if pages.is_empty() {
            println!("[ ]");
        } else {
            let mut items = Vec::new();
            for (abs_path, id, pt, title, _root, rel_path, search_terms) in pages.iter() {
                let tags: Vec<String> = page_tags_str(&db, abs_path).split(',')
                    .filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string()).collect();
                let refs = if pt == "label" { Some(label_expand(&db, abs_path)) } else { None };
                let gj = PageJson {
                    id: id.clone(), pt: pt.clone(),
                    title: title.clone(), abs: abs_path.clone(), rel: rel_path.clone(),
                    tags, gates: search_terms.clone().unwrap_or_default(),
                    links: Some(page_link_count(&db, abs_path)),
                    refs, description: None,
                };
                items.push(gj.render());
            }
            println!("[{}]", items.join(","));
        }
        return Ok(EXIT_OK);
    }

    if pages.is_empty() {
        println!("no pages found under the given path");
        return Ok(EXIT_OK);
    }

    for (i, (abs_path, id, pt, title, _root, rel_path, search_terms)) in pages.iter().enumerate() {
        let tags = page_tags_str(&db, abs_path);
        let link_count = page_link_count(&db, abs_path);
        let gate_str = search_terms.as_deref().unwrap_or("");

        let mut line = format!("{:>3}  {:<20} {}", i + 1, id, title.as_deref().unwrap_or(""));
        if !rel_path.is_empty() {
            line.push_str(&format!("  {rel_path}"));
        }
        if !tags.is_empty() {
            line.push_str(&format!("  tags: {tags}"));
        }
        if !gate_str.is_empty() {
            line.push_str(&format!("  gates: {gate_str}"));
        }
        if link_count > 0 {
            line.push_str(&format!("  links: {link_count}"));
        }
        println!("{line}");

        if pt == "label" {
            let refs = label_expand(&db, abs_path);
            for r in &refs {
                println!("     → {r}");
            }
        }
    }
    println!("── {} page(s)", pages.len());
    Ok(EXIT_OK)
}

// ---- tag ----

fn cmd_tag(dbpath: Option<&Path>, tag: &str, json: bool) -> Kb {
    let db = open_db(dbpath)?;

    let aliases: Vec<(String, String)> = {
        let mut stmt = db.conn.prepare("SELECT alias, canonical FROM tag_aliases")
            .map_err(|e| (format!("prepare aliases: {e}"), EXIT_ERR))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| (format!("aliases: {e}"), EXIT_ERR))?
            .collect::<Result<_, _>>()
            .map_err(|e| (format!("aliases rows: {e}"), EXIT_ERR))?;
        rows
    };
    let all_tags: Vec<String> = {
        let mut stmt = db.conn.prepare("SELECT DISTINCT tag FROM page_tags")
            .map_err(|e| (format!("prepare tags: {e}"), EXIT_ERR))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| (format!("tags: {e}"), EXIT_ERR))?
            .collect::<Result<_, _>>()
            .map_err(|e| (format!("tag rows: {e}"), EXIT_ERR))?;
        rows
    };
    let match_set: Vec<String> = kbtags::canonicalize(tag, &all_tags, &aliases)
        .into_iter().map(|s: &str| s.to_string()).collect();
    let _ = (all_tags, aliases);
    if match_set.is_empty() {
        if json {
            println!("{}", "[]" as &str);
        } else {
            println!("no pages tagged with {tag:?} (or any alias)");
        }
        return Ok(EXIT_OK);
    }

    // SECURITY_REPORT 3.4: the left side of the comparison is
    // `lower(pt.tag)`, so the IN-list must be lowercased too. `canonicalize`
    // returns the *stored* strings in their original casing, so a stored
    // mixed-case tag like `Czech` would otherwise fail `lower('Czech')
    // IN ('Czech',…)` → no match. Folding to lowercase matches both sides.
    let in_list: Vec<String> = match_set
        .iter()
        .map(|t| format!("'{}'", t.to_lowercase().replace('\'', "''")))
        .collect();

    let sql = format!(
        "SELECT pages.abs_path, pages.id, pages.type, pages.title, pages.root, pages.rel_path\n\
         FROM pages\n\
         JOIN page_tags pt ON pt.abs_path = pages.abs_path\n\
         WHERE lower(pt.tag) IN ({})\n\
         ORDER BY pages.root, pages.rel_path",
        in_list.join(",")
    );

    let mut stmt = db.conn.prepare(&sql)
        .map_err(|e| (format!("prepare tag query: {e}"), EXIT_ERR))?;
    let pages: Vec<(String, String, String, Option<String>, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| (format!("tag query: {e}"), EXIT_ERR))?
        .collect::<Result<_, _>>()
        .map_err(|e| (format!("tag rows: {e}"), EXIT_ERR))?;

    if json {
        // An empty match-set is still a clean empty array (the agent does not
        // need the human "match-set was: ..." hint).
        if pages.is_empty() {
            println!("[ ]");
        } else {
            let mut items = Vec::new();
            for (abs_path, id, pt, title, _root, rel_path) in pages.iter() {
                let tags: Vec<String> = page_tags_str(&db, abs_path).split(',')
                    .filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string()).collect();
                let refs = if pt == "label" { Some(label_expand(&db, abs_path)) } else { None };
                let gj = PageJson {
                    id: id.clone(), pt: pt.clone(),
                    title: title.clone(), abs: abs_path.clone(), rel: rel_path.clone(),
                    tags, gates: String::new(),
                    links: None, refs, description: None,
                };
                items.push(gj.render());
            }
            println!("[{}]", items.join(","));
        }
        return Ok(EXIT_OK);
    }

    if pages.is_empty() {
        println!("no pages tagged with {tag:?} (match-set was: {match_set:?})");
        return Ok(EXIT_OK);
    }

    for (i, (abs_path, id, pt, title, _root, rel_path)) in pages.iter().enumerate() {
        let tags = page_tags_str(&db, abs_path);
        let mut line = format!("{:>3}  {:<24} {}", i + 1, id, title.as_deref().unwrap_or(""));
        if !rel_path.is_empty() {
            line.push_str(&format!("  {rel_path}"));
        }
        if !tags.is_empty() {
            line.push_str(&format!("  tags: {tags}"));
        }
        println!("{line}");

        if pt == "label" {
            let refs = label_expand(&db, abs_path);
            for r in &refs {
                println!("     → {r}");
            }
        }
    }
    println!("── {} tagged page(s) for {tag:?}", pages.len());
    Ok(EXIT_OK)
}

// ---- tag-alias ----

/// Read the full `tag_aliases` table, sorted by alias (deterministic).
fn read_all_aliases(db: &Db) -> Result<Vec<(String, String)>, (String, i32)> {
    let mut stmt = db.conn
        .prepare("SELECT alias, canonical FROM tag_aliases ORDER BY alias")
        .map_err(|e| (format!("prepare: {e}"), EXIT_ERR))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| (format!("query: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("rows: {e}"), EXIT_ERR))?;
    Ok(rows)
}

struct TagAliasOpts {
    add: Option<String>,
    canonical: Option<String>,
    rm: Option<String>,
    list: bool,
    export: bool,
    import: bool,
    file: Option<PathBuf>,
}

fn cmd_tag_alias(dbpath: Option<&Path>, o: &TagAliasOpts, json: bool) -> Kb {
    let add = o.add.as_deref();
    let canonical = o.canonical.as_deref();
    let rm = o.rm.as_deref();
    let list = o.list;
    let export = o.export;
    let import = o.import;
    let file = o.file.as_deref();
    let db = open_db(dbpath)?;

    // ---- export / import (the M8 recovery pathway) -----------------------
    if export {
        let aliases = read_all_aliases(&db)?;
        let text = kbalias::to_text(&aliases);
        let n = aliases.len();
        match file {
            Some(p) => {
                std::fs::write(p, text)
                    .map_err(|e| (format!("write {}: {e}", p.display()), EXIT_ERR))?;
                println!("exported {n} alias row(s) to {}", p.display());
            }
            None => {
                // stdout (the caller can redirect to the recovery file:
                // `kb tag-alias --export > backup.tsv`).
                print!("{text}");
            }
        }
        return Ok(EXIT_OK);
    }

    if import {
        let text = match file {
            Some(p) => std::fs::read_to_string(p)
                .map_err(|e| (format!("read {}: {e}", p.display()), EXIT_ERR))?,
            None => {
                // stdin (so `kb tag-alias --import < backup.tsv` works).
                let mut buf = String::new();
                use std::io::Read;
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|e| (format!("read stdin: {e}"), EXIT_ERR))?;
                buf
            }
        };
        // Report the *skips* (malformed / non-data lines) before loading, so
        // a partially-typed backup is never silently truncated. This walks
        // the same `parse_line` the pure core uses, so the codec is the single
        // source of truth for what counts as a data line.
        let mut loaded = 0usize;
        let mut skipped = 0usize;
        let mut rows: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            match kbalias::parse_line(line) {
                Some((a, c)) => {
                    if let Some(slot) = rows.iter_mut().find(|(x, _)| x == a) {
                        slot.1 = c.to_string();
                    } else {
                        rows.push((a.to_string(), c.to_string()));
                        loaded += 1;
                    }
                }
                None => {
                    if !kbalias::line_is_ignorable(line) {
                        skipped += 1;
                    }
                }
            }
        }
        // SECURITY_REPORT 3.8: raw `BEGIN` / `COMMIT` via `execute_batch`
        // leaves the transaction *open* if any step in between returns `Err`
        // (e.g. `prepare` or mid-batch `upsert.execute`). `rusqlite` then
        // keeps that open transaction on the reused `Connection`, so any
        // subsequent command (or a process death in the gap) sees / loses the
        // still-uncommitted batch. `unchecked_transaction()` is a checked
        // guard whose `Drop` issues `ROLLBACK` on error paths.
        let tx = db
            .conn
            .unchecked_transaction()
            .map_err(|e| (format!("begin import: {e}"), EXIT_ERR))?;
        {
            let mut upsert = tx
                .prepare(
                    "INSERT INTO tag_aliases(alias, canonical) VALUES (?1, ?2)\n\
                      ON CONFLICT(alias) DO UPDATE SET canonical = excluded.canonical",
                )
                .map_err(|e| (format!("prepare import: {e}"), EXIT_ERR))?;
            for (a, c) in &rows {
                upsert
                    .execute(rusqlite::params![a, c])
                    .map_err(|e| (format!("upsert import: {e}"), EXIT_ERR))?;
            }
        }
        tx.commit()
            .map_err(|e| (format!("commit import: {e}"), EXIT_ERR))?;
        if json {
            println!(
                "{{\n  \"loaded\": {loaded},\n  \"skipped\": {skipped}\n}}"
            );
        } else {
            println!(
                "imported {loaded} alias row(s) (last-wins on duplicate aliases); {skipped} line(s) skipped (blank/comment/malformed)",
            );
        }
        return Ok(EXIT_OK);
    }

    if list {
        let mut stmt = db.conn
            .prepare("SELECT alias, canonical FROM tag_aliases ORDER BY alias")
            .map_err(|e| (format!("prepare: {e}"), EXIT_ERR))?;
        let aliases = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| (format!("query: {e}"), EXIT_ERR))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| (format!("rows: {e}"), EXIT_ERR))?;
        if json {
            if aliases.is_empty() {
                println!("[ ]");
            } else {
                let items: Vec<String> = aliases.iter().map(|(alias, canonical)| {
                    format!(
                        "\n  {{\n    \"alias\": {},\n    \"canonical\": {}\n  }}",
                        jstr(alias),
                        jstr(canonical),
                    )
                }).collect();
                println!("[{}]", items.join(","));
            }
            return Ok(EXIT_OK);
        }
        if aliases.is_empty() {
            println!("(no tag aliases defined)");
        }
        for (alias, canonical) in aliases {
            println!("{alias}  →  {canonical}");
        }
        return Ok(EXIT_OK);
    }

    if let Some(alias) = rm {
        let n = db.conn
            .execute(
                "DELETE FROM tag_aliases WHERE alias = ?1",
                rusqlite::params![alias],
            )
            .map_err(|e| (format!("delete alias: {e}"), EXIT_ERR))?;
        if json {
            println!(
                "{{\n  \"alias\": {},\n  \"removed\": {}\n}}",
                jstr(alias),
                n,
            );
        } else if n == 0 {
            println!("no alias {alias:?} found — nothing removed");
        } else {
            println!("removed alias: {alias:?}");
        }
        return Ok(EXIT_OK);
    }

    let Some(alias) = add else {
        return Err(("--add <alias> --canonical <tag> | --rm <alias> | --list | --export | --import required".into(), EXIT_ERR));
    };
    let Some(canonical) = canonical else {
        return Err((format!("--add {alias:?} requires --canonical <tag>"), EXIT_ERR));
    };
    db.conn
        .execute(
            "INSERT INTO tag_aliases(alias, canonical) VALUES (?1, ?2)\n\
             ON CONFLICT(alias) DO UPDATE SET canonical = excluded.canonical",
            rusqlite::params![alias, canonical],
        )
        .map_err(|e| (format!("insert alias: {e}"), EXIT_ERR))?;
    if json {
        println!(
            "{{\n  \"alias\": {},\n  \"canonical\": {}\n}}",
            jstr(alias),
            jstr(canonical),
        );
    } else {
        println!("alias: {alias:?} → {canonical:?}");
    }
    Ok(EXIT_OK)
}

// ---- links ----

fn cmd_links(dbpath: Option<&Path>, target: &str, json: bool) -> Kb {
    let db = open_db(dbpath)?;

    // Resolve the target to an abs_path. It could be:
    // - an absolute path (/home/u/notes/foo.md)
    // - a relative-ish path (resolved against the page's root)
    // - a display id (wiki:slug, label:slug, note:slug)
    let abs_path: Option<String> = if target.starts_with('/') {
        Some(target.to_string())
    } else if target.contains(':') {
        // display id
        let mut stmt = db.conn
            .prepare("SELECT abs_path FROM pages WHERE id = ?1")
            .map_err(|e| (format!("prepare id lookup: {e}"), EXIT_ERR))?;
        stmt.query_row(rusqlite::params![target], |r| r.get::<_, String>(0))
            .optional()
            .map_err(|e| (format!("id lookup: {e}"), EXIT_ERR))?
    } else {
        // treat as path — try matching against abs_path (exact or relative)
        let mut stmt = db.conn
            .prepare("SELECT abs_path FROM pages WHERE abs_path = ?1 OR abs_path LIKE '%' || ?1")
            .map_err(|e| (format!("prepare path lookup: {e}"), EXIT_ERR))?;
        stmt.query_row(rusqlite::params![target], |r| r.get::<_, String>(0))
            .optional()
            .map_err(|e| (format!("path lookup: {e}"), EXIT_ERR))?
    };

    let Some(abs_path) = abs_path else {
        return Err((format!("no page found for {target:?}"), EXIT_ERR));
    };

    // Load the corpus once — used both to resolve the *current* page's forward
    // wikilinks (so a `[[B]]` in this page's body is shown as the page it
    // points to, not the raw "B") and to build the true backlink set (pages
    // whose extracted forward links resolve to `abs_path`).
    let corpus_pages = load_corpus(&db)?;
    let abs_index: std::collections::HashMap<String, &resolver::Page> =
        corpus_pages.iter().map(|p| (p.abs_path.clone(), p)).collect();

    // forward links — resolve raw wikilink/path targets to their page id.
    // Collect into (to_path, style, Some(resolved_abs) | None) so the human +
    // JSON renderers share the exact same resolution.
    let mut fwd_stmt = db.conn
        .prepare("SELECT to_path, style FROM links WHERE from_path = ?1 ORDER BY to_path, style")
        .map_err(|e| (format!("prepare fwd: {e}"), EXIT_ERR))?;
    let forward_raw: Vec<(String, String)> = fwd_stmt
        .query_map(rusqlite::params![abs_path], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| (format!("fwd query: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("fwd rows: {e}"), EXIT_ERR))?;
    let forward: Vec<(String, String, Option<String>)> = forward_raw
        .iter()
        .map(|(to_path, style)| {
            let resolved = resolve_target(&corpus_pages, to_path);
            (to_path.clone(), style.clone(), resolved)
        })
        .collect();

    // backlinks — pages whose forward edge *resolves to* this page. We must
    // iterate the corpus (not `WHERE to_path = ?` as the old SQL did) because
    // a wikilink's raw written target text is stored, not the page's
    // abs_path. A raw target text that fails to resolve is a *dead* link and
    // does NOT contribute a backlink (§4 collision is legal; §8 says the
    // indexer leaves to_path as a pending ref).
    // SECURITY_REPORT 3.17: load ALL forward edges once (one `prepare` + one
    // full-scan) into a `BTreeMap<from_path, Vec<to_path>>` instead of
    // re-preparing the statement per corpus page (N prepares for N pages).
    let edges_by_from: std::collections::BTreeMap<String, Vec<String>> = {
        let mut e = db.conn
            .prepare("SELECT from_path, to_path FROM links")
            .map_err(|e| (format!("prepare back-edge: {e}"), EXIT_ERR))?;
        let rows = e.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| (format!("back-edge query: {e}"), EXIT_ERR))?;
        let mut map: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for row in rows {
            let (from, to) = row.map_err(|e| (format!("back-edge rows: {e}"), EXIT_ERR))?;
            map.entry(from).or_default().push(to);
        }
        map
    };
    let mut back: Vec<String> = Vec::new();
    for (from_abs, _p) in corpus_pages
        .iter()
        .map(|p| (p.abs_path.clone(), p))
        .filter(|(src,_)| *src != abs_path)
    {
        // look up this page's forward edges (already loaded) and check if any
        // resolve to abs_path.
        let edges = edges_by_from.get(&from_abs);
        let mut hit = false;
        if let Some(targets) = edges {
            for target_text in targets {
                if resolve_target(&corpus_pages, target_text).as_deref() == Some(abs_path.as_str()) {
                    hit = true;
                    break;
                }
            }
        }
        if hit { back.push(from_abs.clone()); }
    }
    back.sort();
    back.dedup();

    // ---- JSON ---------------------------------------------------------
    if json {
        let fwd_items: Vec<String> = forward.iter().map(|(to_path, style, resolved)| {
            format!(
                "{{\n    \"style\": {},\n    \"to_path\": {},\n    \"resolves_to\": {}\n  }}",
                jstr(style),
                jstr(to_path),
                match resolved {
                    Some(r) => jstr(r),
                    None => "null".to_string(),
                },
            )
        }).collect();
        let back_items: Vec<String> = back.iter().map(|from_path| {
            format!(
                "{{\n    \"abs_path\": {},\n    \"id\": {}\n  }}",
                jstr(from_path),
                match abs_index.get(from_path) {
                    Some(p) => jstr(&format!("{}:{}", p.typ, p.slug)),
                    None => "null".to_string(),
                },
            )
        }).collect();
        println!(
            "{{\n  \"page\": {},\n  \"forward\": [{}],\n  \"backlinks\": [{}]\n}}",
            jstr(&abs_path),
            fwd_items.join(","),
            back_items.join(","),
        );
        return Ok(EXIT_OK);
    }

    // ---- human ---------------------------------------------------------
    println!("page: {abs_path}");

    if forward.is_empty() {
        println!("\nforward links: none");
    } else {
        println!("\nforward links ({}):", forward.len());
        for (to_path, style, resolved) in &forward {
            let annotated = match resolved {
                Some(r) => {
                    let display = abs_index
                        .get(r)
                        .map(|p| format!("{}:{}", p.typ, p.slug))
                        .unwrap_or(r.clone());
                    format!("  → {display}")
                }
                None => "  (unresolved)".to_string(),
            };
            println!("  [ {style} ]  {to_path}{annotated}");
        }
    }

    if back.is_empty() {
        println!("\nbacklinks: none");
    } else {
        println!("\nbacklinks ({}):", back.len());
        let _ = &abs_index; // (kept for symmetry — the corpus is what we used)
        for from_path in &back {
            if let Some(p) = abs_index.get(from_path) {
                println!("  {from_path}  ({}:{})", p.typ, p.slug);
            } else {
                println!("  {from_path}");
            }
        }
    }

    Ok(EXIT_OK)
}

// ---- resolve ----

/// Load the identity projection of `pages` into `resolver::Page` rows:
/// the absolute path, the resolved slug (stored in `title`), the `type`, and
/// the root-relative path. Exactly the columns the §4 resolver reads — nothing
/// more — so a future schema addition that the resolver should ignore is
/// already filtered out here (and a column the resolver *needs* being absent
/// is an obvious prepare error, not a silent wrong-resolution).
fn load_corpus(db: &Db) -> Result<Vec<resolver::Page>, (String, i32)> {
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
fn resolve_target(pages: &[resolver::Page], target: &str) -> Option<String> {
    match resolver::resolve(target, pages) {
        resolver::Resolution::Unique(abs) => Some(abs),
        resolver::Resolution::Candidates(v) => v.into_iter().next(),
        resolver::Resolution::Empty => None,
    }
}

fn cmd_resolve(dbpath: Option<&Path>, query: &str, json: bool) -> Kb {
    let db = open_db(dbpath)?;
    let pages = load_corpus(&db)?;
    let outcome = resolver::resolve(query, &pages);
    // Map each resolved `abs_path` back to its page so we can render the
    // display id (`type:slug`), not just the path — the shell's contract is
    // "show me the candidate list", and the pure core's outcome (an
    // abs_path set) is exactly the identity key we index on.
    let idx: std::collections::HashMap<&str, &resolver::Page> =
        pages.iter().map(|p| (p.abs_path.as_str(), p)).collect();
    let resolved: Vec<String> = match &outcome {
        resolver::Resolution::Unique(p) => vec![p.clone()],
        resolver::Resolution::Candidates(v) => v.clone(),
        resolver::Resolution::Empty => Vec::new(),
    };
    let kind = match &outcome {
        resolver::Resolution::Unique(_) => "unique",
        resolver::Resolution::Candidates(v) if v.len() > 1 => "candidates",
        resolver::Resolution::Candidates(_) => "unique",
        resolver::Resolution::Empty => "empty",
    };
    if json {
        let items: Vec<String> = resolved.iter().map(|abs| {
            let p = idx.get(abs.as_str()).unwrap();
            format!(
                "\n  {{\n    \"id\": {},\n    \"type\": {},\n    \"slug\": {},\n    \"abs_path\": {}\n  }}",
                jstr(&format!("{}:{}", p.typ, p.slug)),
                jstr(&p.typ),
                jstr(&p.slug),
                jstr(&p.abs_path),
            )
        }).collect();
        println!(
            "{{\n  \"query\": {},\n  \"outcome\": {},\n  \"candidates\": [{}]\n}}",
            jstr(query),
            jstr(kind),
            items.join(","),
        );
        return Ok(EXIT_OK);
    }
    if resolved.is_empty() {
        println!("no page matches {query:?}");
        return Ok(EXIT_OK);
    }
    if resolved.len() == 1 {
        // a clean unique hit — render the page + a "resolved to" marker so the
        // user can see the resolution terminated.
        let p = idx.get(resolved[0].as_str()).unwrap();
        println!(
            "1  {:<24} {:<6} {}  {}   [resolved to abs_path]",
            format!("{}:{}", p.typ, p.slug), p.typ, p.slug, p.abs_path
        );
    } else {
        // a collision — the §4 contract: a candidate list, never a hard error.
        for (i, abs) in resolved.iter().enumerate() {
            let p = idx.get(abs.as_str()).unwrap();
            println!(
                "{:>3}  {:<24} {:<6} {}  {}   [candidate]",
                i + 1, format!("{}:{}", p.typ, p.slug), p.typ, p.slug, p.abs_path
            );
        }
        println!(
            "\n{} candidates — disambiguate with a `type:slug` id or a more specific slug/substring",
            resolved.len()
        );
    }
    Ok(EXIT_OK)
}

// ---- info ----

/// The ten display columns of a `kb info` row, in SELECT order. (Named so the
/// optional row binding stays readable without inlining the tuple type.)
type InfoRow = (
    String, String, String, Option<String>, String, String,
    Option<String>, i64, Option<String>, Option<String>,
);

fn cmd_info(dbpath: Option<&Path>, path: &Path, json: bool) -> Kb {
    let abs: String = path
        .to_str()
        .ok_or_else(|| (format!("{:?}: not a UTF-8 path", path), EXIT_ERR))?
        .to_string();
    let db = open_db(dbpath)?;
    let row: Option<InfoRow> = db.conn
        .query_row(
            "SELECT abs_path, id, type, title, root, rel_path,\n\
             description, mtime_ns, created_at, search_terms\n\
             FROM pages WHERE abs_path = ?1",
            // mtime_ns is INTEGER NOT NULL; created_at is TEXT (ISO-8601).
            params![abs],
            |r| Ok((
                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?,
                r.get(4)?, r.get(5)?, r.get(6)?,
                r.get(7)?, r.get(8)?, r.get(9)?,
            )),
        )
        .optional()
        .map_err(|e| (format!("info query: {e}"), EXIT_ERR))?;
    let Some(rec) = row else {
        return Err((format!("no indexed page for {abs}"), EXIT_ERR));
    };
    let (abs_path, id, pt, title, _root, _rel_path, description, mtime_ns, created_at, search_terms) = rec;
    let tags = page_tags_str(&db, &abs_path);
    let fwd_count = db.conn
        .query_row(
            "SELECT COUNT(*) FROM links WHERE from_path = ?1",
            params![abs_path],
            |r| r.get(0),
        ).unwrap_or(0);
    let back_count = db.conn
        .query_row(
            "SELECT COUNT(*) FROM links WHERE to_path = ?1",
            params![abs_path],
            |r| r.get(0),
        ).unwrap_or(0);
    let secs_f = (mtime_ns as f64) / 1e9;
    let mtime_str = format!("{secs_f:.1}s since unix epoch");
    let search_terms = search_terms.unwrap_or_default();
    if json {
        let tag_arr: Vec<String> = tags.split(',')
            .filter(|s| !s.trim().is_empty()).map(|s| jstr(s.trim())).collect();
        println!(
            "{{\n  \"id\": {},\n  \"type\": {},\n  \"title\": {},\n  \"abs_path\": {},\n  \"rel_path\": {},\n  \"description\": {},\n  \"search_terms\": {},\n  \"tags\": [{}],\n  \"links\": {{\n    \"out\": {},\n    \"in\": {}\n  }},\n  \"mtime\": {},\n  \"created_at\": {}\n}}",
            jstr(&id),
            jstr(&pt),
            jstr_opt(&title),
            jstr(&abs_path),
            jstr(&_rel_path),
            jstr_opt(&description),
            jstr(&search_terms),
            tag_arr.join(","),
            fwd_count,
            back_count,
            secs_f,
            jstr_opt(&created_at),
        );
        return Ok(EXIT_OK);
    }
    println!("path:    {abs_path}");
    println!("id:      {id}");
    println!("type:    {pt}");
    if !title.as_deref().unwrap_or("").is_empty() {
        println!("title:   {}", title.as_deref().unwrap_or(""));
    }
    if !description.as_deref().unwrap_or("").is_empty() {
        println!("desc:    {}", description.as_deref().unwrap_or(""));
    }
    if !search_terms.is_empty() {
        println!("gates:   {search_terms}");
    }
    if !tags.is_empty() { println!("tags:    {tags}"); }
    println!("links:   {fwd_count} out / {back_count} in");
    println!("mtime:   {mtime_str}");
    if let Some(c) = created_at {
        println!("created: {c}");
    }
    Ok(EXIT_OK)
}

// ---- stats ----

fn cmd_stats(dbpath: Option<&Path>, json: bool) -> Kb {
    let db = open_db(dbpath)?;
    let roots: Vec<(String, Option<String>, i32)> = {
        let mut stmt = db.conn
            .prepare("SELECT path, extra_exts, include_hidden FROM roots ORDER BY seq DESC")
            .map_err(|e| (format!("prepare roots: {e}"), EXIT_ERR))?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        }).map_err(|e| (format!("roots: {e}"), EXIT_ERR))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| (format!("roots rows: {e}"), EXIT_ERR))?;
        rows
    };
    let mut root_stats: Vec<(String, Option<Vec<String>>, i32, i64)> = Vec::new();
    if json {
        for (root, extra_exts, include_hidden) in &roots {
            let count: i64 = db.conn
                .query_row(
                    "SELECT COUNT(*) FROM pages WHERE root = ?1",
                    params![root],
                    |r| r.get(0),
                ).unwrap_or(0);
            let exts = extra_exts.as_ref().and_then(|e| {
                if e.is_empty() { None }
                else { Some(e.split('\n').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect()) }
            });
            root_stats.push((root.clone(), exts, *include_hidden, count));
        }
    } else {
        println!("== per-root page counts ==");
        for (root, extra_exts, include_hidden) in &roots {
            let count: i64 = db.conn
                .query_row(
                    "SELECT COUNT(*) FROM pages WHERE root = ?1",
                    params![root],
                    |r| r.get(0),
                ).unwrap_or(0);
            let mut opts = Vec::new();
            if let Some(exts) = extra_exts {
                if !exts.is_empty() {
                    opts.push(format!("ext:{}", exts.split('\n').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(",")));
                }
            }
            if *include_hidden != 0 { opts.push("include-hidden".into()); }
            let opt_str = if opts.is_empty() { String::new() } else { format!(" [{}]", opts.join(" ")) };
            println!("{root}{opt_str}  →  {count} page(s)");
        }
    }
    let total: i64 = db.conn
        .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
        .unwrap_or(0);
    let tag_count: i64 = db.conn
        .query_row("SELECT COUNT(DISTINCT tag) FROM page_tags", [], |r| r.get(0))
        .unwrap_or(0);
    let link_count: i64 = db.conn
        .query_row("SELECT COUNT(*) FROM links", [], |r| r.get(0))
        .unwrap_or(0);
    let alias_count: i64 = db.conn
        .query_row("SELECT COUNT(*) FROM tag_aliases", [], |r| r.get(0))
        .unwrap_or(0);
    // db file size
    let db_path = db_path_str(dbpath);
    let size: u64 = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let last = db.conn
        .query_row(
            "SELECT COALESCE(MAX(updated_at), MAX(created_at)) FROM pages",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten();

    if json {
        let root_items: Vec<String> = root_stats.iter().map(|(path, exts, inc, count)| {
            let mut o = format!("\n    {{\n      \"path\": {}", jstr(path));
            if let Some(v) = exts {
                let arr: Vec<String> = v.iter().map(|e| jstr(e)).collect();
                o.push_str(&format!(",\n      \"ext\": [{}]", arr.join(",")));
            }
            o.push_str(&format!(",\n      \"include_hidden\": {}", jbool(*inc != 0)));
            o.push_str(&format!(",\n      \"pages\": {count}\n    }}\n"));
            o
        }).collect();
        let last_val = match last {
            Some(ns) => format!("{:?}", ns as f64 / 1e9),
            None => "null".into(),
        };
        println!(
            "{{\n  \"roots\": [{}{}],\n  \"totals\": {{\n    \"pages\": {total},\n    \"tags\": {tag_count},\n    \"links\": {link_count},\n    \"tag_aliases\": {alias_count}\n  }},\n  \"db\": {{\n    \"path\": {},\n    \"size_bytes\": {size},\n    \"last_touch\": {last_val}\n  }}\n}}",
            if root_items.is_empty() { String::new() } else { root_items.join(",") },
            if root_items.is_empty() { "" } else { " " },
            jstr(&db_path),
        );
        return Ok(EXIT_OK);
    }

    println!("\n== totals ==");
    println!("pages:        {total}");
    println!("tags:         {tag_count}");
    println!("links:        {link_count}");
    println!("tag-aliases:  {alias_count}");
    println!("db file size: {size} bytes ({db_path})");
    if let Some(ns) = last {
        println!("last touch:   {:.1}s since unix epoch", ns as f64 / 1e9);
    } else {
        println!("last touch:   (never)");
    }
    Ok(EXIT_OK)
}

fn db_path_str(dbpath: Option<&Path>) -> String {
    dbpath
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| format!("{}/.local/share/knowledgebase/kb.db", dirs::home_dir().map(|h| h.display().to_string()).unwrap_or_else(|| "~".into())))
}

// ---- shell-quote helper (for kb cron) ----

fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    let safe: bool = s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.' || c == ':');
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

// ========================== M6 single-file namespace ==========================

fn cmd_toc(file: &Path, json: bool) -> Kb {
    let data = read_file(file)?;
    let doc = Doc::new(data);
    if json {
        let secs: Vec<String> = doc
            .sections
            .iter()
            .map(|s| {
                format!(
                    "{{\"ref\":{},\"level\":{},\"title\":{},\"line\":{}}}",
                    jstr(&s.ref_),
                    s.level,
                    jstr(&s.title),
                    s.start_line + 1
                )
            })
            .collect();
        println!(
            "{{\"file\":{},\"sections\":[{}]}}",
            jstr(&file.display().to_string()),
            secs.join(",")
        );
        return Ok(EXIT_OK);
    }
    if doc.sections.is_empty() {
        println!("{}: (no sections)", file.display());
        return Ok(EXIT_OK);
    }
    for s in &doc.sections {
        let indent = "  ".repeat((s.level.saturating_sub(1)) as usize);
        println!(
            "{indent}{}   ref {}  (level {})",
            s.title,
            s.ref_,
            s.level
        );
    }
    Ok(EXIT_OK)
}

fn cmd_show(file: &Path, ref_: Option<&str>, body: bool, full: bool, json: bool, show_etag: bool) -> Kb {
    let data = read_file(file)?;
    let doc = Doc::new(data);

    let Some(r) = ref_ else {
        // No ref → whole file.
        if json {
            println!(
                "{{\"file\":{},\"lines\":{},\"bytes\":{}}}",
                jstr(&file.display().to_string()),
                doc.total_lines,
                doc.data.len()
            );
        } else {
            print!("{}", String::from_utf8_lossy(&doc.data));
        }
        return Ok(EXIT_OK);
    };

    if is_fm_ref(r) {
        return cmd_show_fm(&doc.data, r, json, show_etag);
    }

    if body && full {
        return Err(("--body and --full are mutually exclusive".into(), EXIT_ERR));
    }
    let use_full = full;
    let bytes: Vec<u8> = (if use_full {
        doc.full_bytes(r).map_err(|e| (e.to_string(), EXIT_SECTION))
    } else {
        doc.own_bytes(r).map_err(|e| (e.to_string(), EXIT_SECTION))
    })?
    .to_vec();

    if show_etag {
        println!("{}", etag_of(&bytes));
        return Ok(EXIT_OK);
    }

    let idx = doc.resolve(r).map_err(|e| (e.to_string(), EXIT_SECTION))?;
    let s = &doc.sections[idx];
    if json {
        let span = if use_full { "full" } else { "body" };
        let end_line = if use_full { s.end_line } else { s.own_end_line };
        println!(
            "{{\"ref\":{},\"title\":{},\"level\":{},\"span\":\"{}\",\"line\":{},\"lines\":{},\"etag\":{},\"text\":{}}}",
            jstr(&s.ref_),
            jstr(&s.title),
            s.level,
            span,
            s.start_line + 1,
            end_line.saturating_sub(s.start_line),
            jstr(&etag_of(&bytes)),
            jstr(&String::from_utf8_lossy(&bytes))
        );
    } else {
        print!("{}", String::from_utf8_lossy(&bytes));
    }
    Ok(EXIT_OK)
}
/// Is `r` a frontmatter ref (`fm` or `fm.<key>`)?
fn is_fm_ref(r: &str) -> bool {
    r == "fm" || r.starts_with("fm.")
}

/// Byte-level line splitter — inverse of `commit`'s `split_inclusive('\n')`
/// on valid UTF-8. Local helper (the kernel's is private to `kb-core`).
fn lines_of_str(data: &[u8]) -> Vec<String> {
    let s = std::str::from_utf8(data).unwrap_or("");
    s.split_inclusive('\n').map(|l| l.to_string()).collect()
}

/// Show a frontmatter frame or a single field in `data`.
/// `fm` prints the whole `---\n...\n---` block including its delimiters.
/// `fm.<key>` prints *just* that field's line(s).
fn cmd_show_fm(
    data: &[u8],
    r: &str,
    json: bool,
    show_etag: bool,
) -> Kb {
    let fm = match fm::find_fm(data) {
        Some(f) => f,
        None => return Err(("no frontmatter frame found".into(), EXIT_SECTION)),
    };
    let lines = lines_of_str(data);
    // Include both delimiters (open..=close) in the "frame" view.
    let frame: Vec<&str> = lines
        .iter()
        .skip(fm.open_line)
        .take(fm.close_line - fm.open_line + 1)
        .map(|s| s.as_str())
        .collect();
    let interior: Vec<&str> = if frame.len() >= 2 {
        frame[1..frame.len() - 1].to_vec()
    } else {
        Vec::new()
    };
    let key: Option<&str> = r.strip_prefix("fm.").map(|s| s.trim());
    let text: String = match key {
        None => frame.concat(),
        Some(k) => {
            // Find the `key:` line, then emit it plus any trailing contiguous
            // `- item` / blank / comment lines (list fields). Interior lines
            // already carry their trailing newline (split_inclusive), so we
            // push them as-is.
            let mut out = String::new();
            let mut i = 0usize;
            while i < interior.len() {
                let t = interior[i].trim_start_matches('\u{feff}').trim();
                let Some(colon) = t.find(':') else { i += 1; continue; };
                if t[..colon].trim() == k {
                    out.push_str(interior[i]);
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    let mut j = i + 1;
                    while j < interior.len() {
                        let nt = interior[j].trim_start_matches('\u{feff}').trim();
                        if nt.starts_with('-') || nt.is_empty() || nt.starts_with('#') {
                            out.push_str(interior[j]);
                            if !out.ends_with('\n') {
                                out.push('\n');
                            }
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    break;
                }
                i += 1;
            }
            out
        }
    };
    if show_etag {
        println!("{}", etag_of(text.as_bytes()));
        return Ok(EXIT_OK);
    }
    if json {
        println!(
            "{{\"ref\":{},\"etag\":{},\"text\":{}}}",
            jstr(r),
            jstr(&etag_of(text.as_bytes())),
            jstr(&text)
        );
    } else {
        print!("{text}");
    }
    Ok(EXIT_OK)
}

/// Extract the `<key>` part of a `fm.<key>` ref.
fn fm_key(r: &str) -> Option<&str> {
    r.strip_prefix("fm.")
}

/// Resolve the fm value from the optional `--value` flag, falling back to the
/// text source.
fn fm_resolve_value(value: &Option<String>, text: &TextSource, file: &Path) -> Result<String, (String, i32)> {
    if let Some(v) = value {
        return Ok(v.clone());
    }
    read_text(text, file)
}

/// Under `--json` an etag mismatch must still leave exactly one JSON object
/// on stdout: the agent contract promises stdout is always a clean JSON
/// document (success *or* failure) and `--json` suppresses the prose prompt.
/// Emit the failure envelope here; the caller then returns `Err` as before so
/// `run` prints the human message to stderr and the process exits `EXIT_ETAG`.
/// `scope` disambiguates a section ref (`"1.2"`) from the whole file (`"*"`).
fn emit_etag_error_json(scope: &str, expected: &str, actual: &str) {
    println!(
        "{{\n  \"ok\": false,\n  \"applied\": false,\n  \"op\": \"etag_check\",\n  \"scope\": {},\n  \"code\": {},\n  \"error\": {},\n  \"etag_expected\": {},\n  \"etag_actual\": {}\n}}",
        jstr(scope),
        EXIT_ETAG,
        jstr(&format!("etag mismatch: expected {expected}, got {actual} (section/file changed since read)")),
        jstr(expected),
        jstr(actual),
    );
}

/// Enforce `--if-unchanged` over the *whole file* (the fm-op etag contract).
fn check_etag_whole(data: &[u8], flags: &WriteFlags) -> Kb {
    let Some(expected) = flags.if_unchanged.as_deref() else {
        return Ok(EXIT_OK);
    };
    let cur = etag_of(data);
    if cur != expected {
        if flags.json { emit_etag_error_json("*", expected, &cur); }
        return Err((
            format!("etag mismatch: expected {expected}, got {cur} (file changed since read)"),
            EXIT_ETAG,
        ));
    }
    Ok(EXIT_OK)
}

fn cmd_section(file: &Path, op: &SectionOp) -> Kb {
    match op {
        SectionOp::Set {
            ref_: r,
            text,
            keep_heading,
            value,
            flags,
        } => {
            if is_fm_ref(r) {
                let key = fm_key(r).ok_or_else(|| (format!("`set` on whole-frontmatter ref {r:?} needs a key (fm.<key>)"), EXIT_SECTION))?;
                let val = fm_resolve_value(value, text, file)?;
                let orig = read_file(file)?;
                check_etag_whole(&orig, flags)?;
                let new = fm::set_scalar(&orig, key, &val).map_err(|e| (e.to_string(), EXIT_SECTION))?;
                commit(file, orig, new, flags.dry_run, flags.yes, flags.json)
            } else {
                let orig = read_file(file)?;
                let doc = Doc::new(orig.clone());
                check_etag(&doc, Some(r.as_str()), flags)?;
                let text = read_text(text, file)?;
                let new = doc
                    .set_section(r, &text, *keep_heading)
                    .map_err(|e| (e.to_string(), EXIT_SECTION))?;
                commit(file, orig, new, flags.dry_run, flags.yes, flags.json)
            }
        }
        SectionOp::Append { ref_: r, text, value, flags } => {
            if is_fm_ref(r) {
                let key: &str = fm_key(r).ok_or_else(|| (format!("`append` on whole-frontmatter ref {r:?} needs a key (fm.<key>)"), EXIT_SECTION))?;
                let val = fm_resolve_value(value, text, file)?;
                let orig = read_file(file)?;
                check_etag_whole(&orig, flags)?;
                let new = fm::append_list_item(&orig, key, &val).map_err(|e| (e.to_string(), EXIT_SECTION))?;
                commit(file, orig, new, flags.dry_run, flags.yes, flags.json)
            } else {
                let orig = read_file(file)?;
                let doc = Doc::new(orig.clone());
                check_etag(&doc, Some(r.as_str()), flags)?;
                let text = read_text(text, file)?;
                let new = doc
                    .append_section(r, &text)
                    .map_err(|e| (e.to_string(), EXIT_SECTION))?;
                commit(file, orig, new, flags.dry_run, flags.yes, flags.json)
            }
        }
        SectionOp::Insert {
            ref_: r,
            title,
            placement,
            level,
            text,
            flags,
        } => {
            let orig = read_file(file)?;
            let doc = Doc::new(orig.clone());
            check_etag(&doc, r.as_deref(), flags)?;
            let text = read_text(text, file)?;
            let place = parse_placement(placement)?;
            let (ins, chunk) = doc
                .insert_section(place, r.as_deref(), &text, title, *level)
                .map_err(|e| (e.to_string(), EXIT_SECTION))?;
            let new = splice(&doc.data, ins, ins, &chunk);
            commit(file, orig, new, flags.dry_run, flags.yes, flags.json)
        }
        SectionOp::Replace { ref_: r, text, flags } => {
            let orig = read_file(file)?;
            let doc = Doc::new(orig.clone());
            check_etag(&doc, Some(r.as_str()), flags)?;
            let text = read_text(text, file)?;
            // `replace` = replace the section's body, keep its heading.
            let new = doc
                .set_section(r, &text, true)
                .map_err(|e| (e.to_string(), EXIT_SECTION))?;
            commit(file, orig, new, flags.dry_run, flags.yes, flags.json)
        }
        #[cfg(feature = "editor")]
        SectionOp::Edit { ref_: r, flags } => {
            if flags.json {
                return Err((
                    "`section edit --json` is unsupported — `--edit` is an editor flow, not JSON (use `set --json` over a text source instead)".into(),
                    EXIT_SECTION,
                ));
            }
            let orig = read_file(file)?;
            let doc = Doc::new(orig.clone());
            check_etag(&doc, Some(r.as_str()), flags)?;
            let old_full = doc.full_bytes(r).map_err(|e| (e.to_string(), EXIT_SECTION))?;
            let edited = run_editor(old_full)?;
            if edited.as_slice() == old_full {
                println!("{}: no changes (section unchanged in editor)", file.display());
                return Ok(EXIT_OK);
            }
            let new_text = String::from_utf8_lossy(&edited).into_owned();
            // The editor had the complete section (heading + body + subsections);
            // hand the whole thing back to `set_section` (keep_heading = false)
            // so the full span is replaced exactly as edited.
            let new = doc
                .set_section(r, &new_text, false)
                .map_err(|e| (e.to_string(), EXIT_SECTION))?;
            commit(file, orig, new, flags.dry_run, flags.yes, flags.json)
        }
        SectionOp::Rm { ref_: r, value, flags } => {
            if is_fm_ref(r) {
                let key: &str = fm_key(r).ok_or_else(|| (format!("`rm` on whole-frontmatter ref {r:?} needs a key (fm.<key>)"), EXIT_SECTION))?;
                let orig = read_file(file)?;
                check_etag_whole(&orig, flags)?;
                let new = match value {
                    Some(v) => fm::remove_list_item(&orig, key, v).map_err(|e| (e.to_string(), EXIT_SECTION))?,
                    None => fm::remove_field(&orig, key).map_err(|e| (e.to_string(), EXIT_SECTION))?,
                };
                commit(file, orig, new, flags.dry_run, flags.yes, flags.json)
            } else {
                Err((format!("`rm` on a section ref is not yet supported (try `rm fm.<key>` instead, got ref {r:?})",), EXIT_SECTION))
            }
        }
    }
}

fn cmd_replace(file: &Path, find: &str, new: &str, scope: Option<&str>, count: Option<usize>, flags: &WriteFlags) -> Kb {
    let orig = read_file(file)?;
    let doc = Doc::new(orig.clone());
    check_etag(&doc, scope, flags)?;
    let (nb, n) = doc
        .replace_in(scope, find, new, count.unwrap_or(0))
        .map_err(|e| (e.to_string(), EXIT_SECTION))?;
    if !flags.dry_run {
        eprintln!("kb: {n} replacement(s)");
    }
    commit(file, orig, nb, flags.dry_run, flags.yes, flags.json)
}

fn cmd_patch(
    file: &Path,
    patch_file: Option<&Path>,
    patch_stdin: bool,
    patch_text: Option<&str>,
    flags: &WriteFlags,
) -> Kb {
    // Read the patch from exactly one source.
    //   `patch_file = Some(p)`       → read from that file (`-` = stdin)
    //   `patch_stdin = true`         → read from stdin
    //   `patch_text = Some(t)`       → use literal text
    // Multiple sources is an error (the caller must be deliberate); zero
    // is an error (the CLI's flags are not required — this is a runtime
    // invariant; the message names the fix).
    let mut sources = 0usize;
    if patch_file.is_some() { sources += 1; }
    if patch_stdin         { sources += 1; }
    if patch_text.is_some() { sources += 1; }
    if sources != 1 {
        return Err((
            format!(
                "one of --patch-file / --patch-stdin / --patch-text is required (got {sources})"
            ),
            EXIT_ERR,
        ));
    }
    let patch: String = if let Some(t) = patch_text {
        t.to_string()
    } else if let Some(p) = patch_file {
        if p == Path::new("-") {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| (format!("read stdin: {e}"), EXIT_ERR))?;
            s
        } else {
            std::fs::read_to_string(p).map_err(|e| (format!("read {}: {e}", p.display()), EXIT_ERR))?
        }
    } else {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| (format!("read stdin: {e}"), EXIT_ERR))?;
        s
    };

    // Apply. `apply_patch` is total over its input (either `Ok` with a
    // new-text `String`, or `Err(err_msg)`) — no `?`-able panic path.
    let orig = read_file(file)?;
    let doc = Doc::new(orig.clone());
    check_etag(&doc, None, flags)?;
    let orig_s = String::from_utf8_lossy(&orig).into_owned();
    let new_s = diff::apply_patch(&orig_s, &patch)
        .map_err(|e| (format!("apply patch: {e}"), EXIT_SECTION))?;
    let new = new_s.into_bytes();
    commit(file, orig, new, flags.dry_run, flags.yes, flags.json)
}

/// Enforce `--if-unchanged`: the target section's etag must still equal the
/// value the caller captured earlier. The etag is FNV-1a 64-bit over the
/// target section's own-body bytes (or the whole document when no ref is
/// given, e.g. `insert --placement top|end`).
fn check_etag(doc: &Doc, ref_: Option<&str>, flags: &WriteFlags) -> Kb {
    let Some(expected) = flags.if_unchanged.as_deref() else {
        return Ok(EXIT_OK);
    };
    let bytes = match ref_ {
        Some(r) => doc.own_bytes(r).map_err(|e| (e.to_string(), EXIT_SECTION))?.to_vec(),
        None => doc.data.clone(),
    };
    let cur = etag_of(&bytes);
    if cur != expected {
        let scope = ref_.unwrap_or("*");
        if flags.json { emit_etag_error_json(scope, expected, &cur); }
        return Err((
            format!("etag mismatch: expected {expected}, got {cur} (section changed since read)"),
            EXIT_ETAG,
        ));
    }
    Ok(EXIT_OK)
}

/// Show the diff, confirm, re-verify the file, then write atomically. When
/// `json` is true: every human-readable thing (diff, "applied" / "no changes"
/// notes) goes to **stderr** so stdout carries exactly one JSON result object,
/// and the "apply?" prompt is suppressed (a JSON result is non-interactive —
/// the write is applied, or `--dry-run` holds it). The JSON result is
/// `{ "file", "applied", "changed", "dry_run", "bytes_before", "bytes_after",
/// "diff" }` — uniform across every section/replace/fm op.
fn commit(path: &Path, orig: Vec<u8>, new: Vec<u8>, dry_run: bool, assume_yes: bool, json: bool) -> Kb {
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
    if !confirm("apply?", assume_yes || json) {
        return Err(("aborted (not confirmed)".into(), EXIT_ERR));
    }
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

fn read_file(p: &Path) -> Result<Vec<u8>, (String, i32)> {
    std::fs::read(p).map_err(|e| (format!("read {}: {e}", p.display()), EXIT_ERR))
}

/// Resolve the 3-way text source to a `String`.
fn read_text(src: &TextSource, file: &Path) -> Result<String, (String, i32)> {
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

fn parse_placement(s: &str) -> Result<SectionPlacement, (String, i32)> {
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

#[cfg(feature = "editor")]
fn run_editor(initial: &[u8]) -> Result<Vec<u8>, (String, i32)> {
    // Open `initial` in $VISUAL, then $EDITOR, then `vi`, in a temp file;
    // return the post-edit bytes.
    let tmp = std::env::temp_dir().join(format!("kb-edit-{}.md", std::process::id()));
    std::fs::write(&tmp, initial).map_err(|e| (format!("write temp edit buffer: {e}"), EXIT_ERR))?;
    let editor = pick_editor();
    let st = std::process::Command::new(&editor)
        .arg(&tmp)
        .status()
        .map_err(|e| (format!("launch {editor}: {e}"), EXIT_ERR))?;
    if !st.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err((format!("editor {editor} exited with {st}"), EXIT_ERR));
    }
    let bytes = std::fs::read(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);
    Ok(bytes)
}

#[cfg(feature = "editor")]
fn pick_editor() -> String {
    for k in ["VISUAL", "EDITOR"] {
        if let Some(v) = std::env::var(k).ok().filter(|v| !v.trim().is_empty()) {
            return v.split_whitespace().next().unwrap_or("vi").to_string();
        }
    }
    "vi".to_string()
}

/// Open an already-written file in the user's editor **in place** ($VISUAL,
/// then $EDITOR, then `vi`). Used by `kb create -e`, where the scaffold on
/// disk *is* the buffer (unlike `section edit`, which splices a temp copy
/// back through the section ops). A non-zero editor exit is an error — the
/// scaffold is left on disk, so nothing is lost, but nothing is indexed
/// either (the caller unwinds before the DB write).
/// SECURITY_REPORT 3.1: gates with `run_editor`/`pick_editor` — in a hardened
/// build the whole editor surface (incl. `-e`, which calls here via
/// `cmd_create`) is compiled out; clap rejects `-e` as an unexpected argument.
#[cfg(feature = "editor")]
fn edit_in_place(path: &Path) -> Result<(), (String, i32)> {
    let editor = pick_editor();
    let st = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .map_err(|e| (format!("launch {editor}: {e}"), EXIT_ERR))?;
    if !st.success() {
        return Err((
            format!(
                "editor {editor} exited with {st} — {} was left with the scaffold (fix it by hand, or delete it and re-run `kb create`)",
                path.display()
            ),
            EXIT_ERR,
        ));
    }
    Ok(())
}

/// Interactive `y/N` prompt. Non-interactive stdin (piped / no TTY) never
/// auto-approves — the caller must pass `--yes`.
fn confirm(prompt: &str, assume_yes: bool) -> bool {
    if assume_yes {
        return true;
    }
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{prompt} [y/N]? ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    )
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
pub(crate) fn etag_of(b: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(PRIME);
    }
    format!("{h:016x}")
}

/// Minimal JSON string encoder (no external dep — `kb` stays a single
/// static binary).
fn jstr(s: &str) -> String {
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
assert d['search_terms']== 'two\\back\ttabs', d['search_terms'].encode()
assert d['links']       == 3, d['links']
assert d['refs']        == ['x\ny'], d['refs']
print('round-trip-ok')
"#;
        let mut cmd = std::process::Command::new("python3");
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
