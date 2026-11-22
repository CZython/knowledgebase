# `kb mcp` — MCP server surface for the knowledge base

> **Status: TABLED.** This is a design, not a build. It captures the
> agreed constraints and the sharp edges we must solve *before* writing
> code. Reopen by working the "open decisions" section; this doc is the
> source of truth for that discussion. Nothing here compiles; none of
> `kb-hardened` is touched yet.

## Goal

Expose the knowledge base to MCP-speaking agents through **a stdio
JSON-RPC server compiled into `kb-hardened`**, under a hidden `kb mcp`
subcommand. The agent sees and edits pages by **uuid** (the stable,
path-free identity token already stored in `pages.uuid`), never by
filesystem path. A per-page frontmatter opt-out lets the owner hide any
page from the agent entirely.

The hard invariants, in priority order:

1. **No network.** The server is a `read()`/`write()` loop over
   stdin/stdout. No socket, no connect, no lookup, no listen.
2. **`kb-hardened` only.** Not in `kb-user`, not behind a flag there.
3. **No metadata escape.** Filesystem paths (root / rel_path / abs_path)
   and the owner's username do not reach the agent, *unless they are
   literally the page content the agent asked for.*
4. **Hidden surface.** Absent from `--help` and completions; documented
   only here (and one pointer line in the README).

## Why MCP, why stdio, why now

A *reference* implementation already lives at
`~/programming/Rust/kb-mcp` (Python, separate process). We want the same
capability in-process, in the proven/assumed boundary this repo is built
around, and with the no-escape property that the ad-hoc Python version
doesn't have. stdio is already how MCP is designed to run for a
local, single-process server — the agent spawns the binary and talks to
it over a pipe. That pipe is a file descriptor, **not**
`SOCK_STREAM`; there is no `connect()` and no address family. The
"no network" invariant therefore comes for free *if* we never link a
socket library.

## Locked requirements (the four invariants, made concrete)

1. `kb-hardened mcp serve` reads JSON-RPC 2.0 lines from **stdin** and
   writes JSON-RPC 2.0 lines to **stdout**. Every log/diagnostic goes to
   **stderr** (the stdout channel is sacred to the protocol).
2. No crate in the `kb-hardened` dependency tree pulls in a network
   stack — verified by a `gate.sh` stage that greps
   `cargo tree -p kb-hardened --features mcp` for
   `{reqwest, ureq, hyper, tokio, mio}` and fails if any appears.
   JSON parsing is std-only via a dependency already in the tree
   (`serde_json`, currently pulled only under `tui` — we re-home it as a
   standalone optional dep for `mcp`).
3. `kb mcp serve` on the `kb-user` binary does **not** exist — the
   feature is absent from `kb-user/Cargo.toml`, not merely off.
   (Same structural guarantee as "no TUI in hardened": compile-time
   absent, not runtime-hidden.)
4. `kb-hardened --help` and `kb-hardened complete bash` do not list
   `mcp`. The subcommand is `#[command(hide = true)]`. A human or agent
   that types `kb-hardened mcp serve` gets the server; one that never
   heard of it never sees it.

## Identity: uuid in, path never out

The `pages` table already stores the uuid (see
`kb-db/src/migrate.rs` — `uuid TEXT` column, populated by
`kb-core::uuid::uuid_of(id, abs_path)`):

```
pages:
  abs_path  TEXT PK      -- leaks filesystem (root + file)
  id        TEXT         -- type:slug  (slug may repeat across roots)
  type      TEXT
  root      TEXT         -- the directory kb was added from (LEAKS USER)
  rel_path  TEXT         -- path within the root                  (LEAKS USER)
  title     TEXT
  ...
  uuid      TEXT         -- 16 hex, FNV-1a 64 of (id \0 abs_path)
```

The uuid is the unique, content-derived handle we want the agent to use.
Two caveats we have already verified and must not mis-sell:

- **It is a FNV-1a 64-bit hash, not cryptographic.** Collision-safe to
  ~2^32 pages (birthday bound), so collision-far-beyond-what-we-ship.
  But it is *deterministic*: given `id` + `abs_path` any party can
  recompute it. So the uuid is **an opaque label**, not a *secret*. The
  win is that the string we hand the agent is the same whether it came
  from `/home/jakub/…/wal.md` or `/home/jakub/…/other/wal.md`, and it
  does not *show* the path.
- **`type:slug` can collide** (`wiki:notes` in two roots). The uuid
  cannot, because the hash folds in `abs_path`. This is the real reason
  we standardise on uuid — not secrecy, but uniqueness.

The uuid *alone* does not fix the leak: today even a `search --json`
row carries `abs_path`, `rel_path`, and `root` (see
`kb/cli/json.rs:185-188` and `kb/cli/search.rs:12-18`). The fix is a
**public view** that drops those columns and returns the uuid in their
place — and a *filter* that drops blocked rows outright.

## The sanitizer: single chokepoint, one file

Everything that leaves the process passes through one function — this
is the auditable surface, so it lives in one file:

```
kb/mcp/sanitizer.rs   --  fn sanitize(payload)  (see "shape" below)
```

It does, in order:

1. **Drop rows** where the page's frontmatter sets `mcp_hidden: true`
   (see "per-page opt-out" below). If the row is gone, the agent cannot
   learn the page exists. (Not "hide the field"; *delete the row* — a
   row that says `mcp_hidden: true` has already told the agent it
   exists.)
2. **Strip path columns** from every remaining row: `root`, `rel_path`,
   `abs_path`. The uuid (already in the row) covers the handle.
3. **Sanitise snippets**: FTS returns a `frontmatter_text` slice. Strip
   the `mcp_hidden:` token from it (a search for a *blocked* page must
   not leak the fact that the *token* was on the row).
4. **Sanitise errors**: `kb_db::Error` variants that carry a path (e.g.
   "no such page at `…`") are mapped to a **stable error code** + the
   uuid (if known), **not** the path. An error is also a payload.

Everything downstream of the sanitizer may use whatever internal fields
it wants (the renderer, the resolver) — the sanitizer is the last line.

## Per-page opt-out: `mcp_hidden: true`

A new **known** frontmatter field, parsed by
`kb-core/src/frontmatter.rs` next to the existing
`search_required_phrase` (which is exactly the "opt-out of a surface"
pattern we already ship — see the gate-phrase machinery in
`kb-db/src/migrate.rs` and `kb/cli/search.rs`).

Rules:

- **Default: `false`.** A page is visible to MCP unless the owner says
  otherwise. (This is a *relaxation* gate — the MCP surface is open by
  default and the owner hides; the inverse design is "everything hidden
  until opt-in" and is harder to live with for a single-user KB.)
- **Parsed, not FTS-indexed.** The field is a gate, not a search term.
  A search for `mcp_hidden` must not match pages that *have* it as a
  value on a different page (FTS token bleed). Store it in a new,
  non-indexed column (or in the existing `page_gates` table) alongside
  `search_required_phrase`, and apply the filter in the search/browse /
  resolve SQL, not in `MATCH`.
- **Applies to read verbs** (`search`, `browse`, `tag`, `resolve`,
  `show`, `toc`, `info`, `links`). **Applies to write verbs**
  (`section_*`, `replace`, `patch`, `create`) — an agent cannot touch a
  page it cannot see; write-side lookup goes through the same
  "does it exist publicly" gate as read-side.
- **Not in the body.** If the *body* text says "hidden page, see
  /home/jakub/…", the sanitizer does **not** touch it — content is
  content. "Metadata only" is the boundary.
- **One-word rule, no escape hatch.** There is no `mcp_hidden: [x, y]`
  or `mcp_hidden: {fields: [description]}` — the page is in or out, not
  partially. (Partially-visible pages defeat the "indistinguishable
  from never-seen" property; if we ever want it, it's a follow-up.)
- **Ownership: user-authored.** It lives in markdown, so a human editing
  their own note sets it. An agent that can *write* pages (via
  `create` / `section_set`) can also *unset* it on a page (write an
  empty frontmatter block to drop the field). That is acceptable for the
  single-user threat model this KB targets and is documented as such.

## Tool surface (curated, not mirrored)

MCP clients expect a small stable set of tools. We expose a curated
subset, all id-scoped, all in/out in terms the sanitizer can control.
The full `Cmd` enum (20+ verbs) does not become MCP tools 1:1 — that
would be an agent calling `replace --find …` with a path it was never
told, and the no-escape goal dies.

**Read (always available):**

| tool              | current verb | note                                              |
|-------------------|--------------|---------------------------------------------------|
| `kb_search`       | `search`     | public view (sanitised)                            |
| `kb_browse`       | `browse`     | public view                                        |
| `kb_show`         | `show`       | page or one section, by uuid or section-ref         |
| `kb_toc`          | `toc`        | by uuid (section tree, no path)                     |
| `kb_info`         | `info`       | frontmatter + metadata, path-stripped               |
| `kb_links`        | `links`      | as-written links normalised to uuid where possible  |
| `kb_tag`          | `tag`        | page list for a tag                                 |
| `kb_tag_alias_*`  | `tag_alias`  | list / add / rm aliases                             |
| `kb_resolve`      | `resolve`    | candidate list (uuids only)                         |
| `kb_roots`        | `roots`      | root *ids* only, not root *paths*                   |
| `kb_stats`        | `stats`      | counts + sizes (numbers only)                       |

**Write (feature-flagged, off by default):**

| tool              | current verb | note                                                |
|-------------------|--------------|-----------------------------------------------------|
| `kb_create`       | `create`     | new page (uuid assigned at index)                    |
| `kb_section_set`  | `section set`  | full new body + heading (id-scoped)                 |
| `kb_section_replace` | `section replace` | full new body (id-scoped)              |
| `kb_section_append`  | `section append`  | prepend to tail of section                 |
| `kb_section_insert`  | `section insert`  | new section at a placement             |
| `kb_section_rm`      | `section rm`      | drop a section / field / list element               |
| `kb_replace`        | `replace`    | substring (id-scoped, `--scope` for one section)     |
| `kb_patch`          | `patch`      | apply an external `diff -u` to a page                 |

**Not exposed to the agent**: `add_root` / `drop_root` / `refresh` /
`cron` — those are *KB management* verbs (register a tree, GC it,
re-index); they are the human's surface and would, if exposed, let an
agent add new filesystem roots (which is a leak, because the root
*path* would be the argument).

Write verbs are **feature-gated** behind a separate knob, `mcp-writes`,
so the default hardened build the agent gets is **read-only** until the
owner opts in. This also keeps the write surface smaller, and keeps the
"hidden page is hidden" property stronger in the default posture.

## Layout (when we build it)

```
kb/mcp/                       # new module in the shared body
├── mod.rs                    # JSON-RPC 2.0 line reader/writer (stdin/stdout)
├── tools/
│   ├── mod.rs                # dispatch table: name -> handler
│   ├── read.rs               # kb_search / kb_browse / kb_show / kb_toc / kb_info
│   │                         #   kb_links / kb_tag / kb_resolve / kb_roots / kb_stats
│   └── write.rs              # kb_create / kb_section_* / kb_replace / kb_patch
│                             #   (behind the `mcp-writes` feature)
└── sanitizer.rs              # the one file that strips paths + drops hidden rows

kb/main.rs
  + #[cfg(feature = "mcp")]
    #[command(hide = true)] Mcp { serve: bool, writes: bool }

kb-hardened/Cargo.toml
  + [features]
  + mcp        = ["dep:serde_json"]      # OFF by default
  + mcp-writes = ["mcp"]                 # OFF by default; implies read

kb-user/Cargo.toml
  + (unchanged) — `mcp` is NOT a feature here.

gate.sh  (new stage)
  + cargo tree -p kb-hardened --features mcp  | ! grep -E 'reqwest|ureq|hyper|tokio|mio'
  + cargo tree -p kb-user --features tui,editor | grep mcp  -> must be empty
```

## Verification checklist (when we build it)

```
# The surface is hidden
./kb-hardened --help                       # no "mcp" line
./kb-hardened complete bash | grep mcp     # no "mcp" in completions
./kb --help                                # no "mcp" line in the full build either
./kb-user/Cargo.toml | grep ' mcp '        # (nothing)

# It works
./kb-hardened mcp serve < mcp_fixture.json # serves; stderr-only logs

# No network
cargo tree -p kb-hardened --features mcp \
  | grep -Ei 'reqwest|ureq|hyper|tokio|mio'  # empty

# The escape property (sanitiser tests, in kb/mcp/sanitizer.rs)
  - A `mcp_hidden: true` page returns "no such page" on `show` (not "hidden")
  - A `search` on any blocked page's *content* returns no rows for it
  - `search --json` on an unblocked page contains no `root` / `rel_path` / `abs_path`
  - A page whose body says "/home/jakub/…" is returned unchanged
    (content is not metadata)
  - A FTS snippet whose row *has* `mcp_hidden: true` does not emit the token
  - An error from a write that hit an unresolvable target has a stable code,
    not a filesystem path

# The build gate
./gate.sh                                  # all green
./verify.sh                                # kb-core still verified
```

## Open decisions (blocked on the user)

1. **Tool scope** — is the curated read set (11 tools) the right size,
   or do we want even fewer (5-7)? What we expose here we commit to
   supporting. The current list is the maximum defensible for the
   single-user + single-machine threat model.
2. **Write side: on by default in hardened?**  We recommend OFF (the
   `mcp-writes` feature is not in the hardcoded build) and document it.
   Flipping the default is a one-line change in `kb-hardened/Cargo.toml`
   when we do.
3. **`mcp_hidden` — one word or a small escape hatch?**  We recommend
   the one-word rule (above). If we want partial visibility (some fields
   hidden, some returned), we need a different mechanism — and we lose
   the "indistinguishable from never-seen" property.
4. **`roots` tool** — do we expose the *list* of root paths, or just
   the *count*? The list is more useful to the agent but it is also a
   leak (the root is the leak). We default to "count only" and
   `list` as a write-verb-equivalent.
5. **`kb_resolve` shape** — returns `Uuid | [Uuid] | [Uuid+disambiguator]`
   (agent has to pick) or `Uuid` (we resolve uniquely or fail)? We
   recommend the former (agent picks from a disambiguation list);
   it's the existing behaviour for the CLI's `resolve` verb.
6. **Where does `mcp_hidden` go in the schema** — a new column on
   `pages`, or a row in `page_gates` (the existing gate-phrase table)?
   The latter needs zero schema migration; the former is cleaner for
   querying. We recommend the latter (reuse `page_gates`, add a second
   `term` column semantics of `mcp_hidden = true`).

## What we deliberately do NOT do

- **Not a library.** `kb mcp` is a subcommand, not a Rust crate the
  agent links against. The MCP server runs as a child process in
  stdio. Anything else (an HTTP MCP server, a WebSocket bridge, a
  library) needs networking or a non-stdio channel, which is exactly
  what this design excludes.
- **Not a *richer* API than the CLI.** The MCP tools map to existing
  verbs, with the same SQL underneath. We do not add agent-only verbs
  for the sake of "MCP-ness." If an agent needs a new capability, the
  verb first goes in the CLI, then is exposed to MCP.
- **Not a secret channel.** The uuid is a *stable opaque label*. We do
  not claim it is a *secret*. The no-network + no-escape invariant is
  about the *information flow* (what string crosses the wire), not
  about cryptography.
