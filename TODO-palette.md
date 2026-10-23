# TUI Design Pass — `--palette` flag + shared color/hierarchy

**Top priority.** User request: allow the user to supply a custom palette file (including a colorblind-safe one) via a `--palette` flag. Concretely: **A + `--palette` override** from the design audit, plus the palette becomes the single source of truth for every rendered style in `kb/tui`.

## Why top priority

- The current TUI only has color in three places (browse selection bar, edit-value pane, status bar). Everything else is monochrome (`identify` reports the captures as **Gray** PNGs; the edit-value capture as **sRGB**). The gap is user-visible.
- Two users, one terminal: the *default* user may have normal color vision, but a colorblind user (or a user with a custom terminal palette) will want to override the colors. A built-in file-based override with a **colorblind default** shipped next to it is strictly more capable than "no palette at all".
- Once `--palette` exists and `App` holds a `Palette` value, **every** other visual improvement in the audit (hierarchy in browse, markdown headings in read, empty markers in pickers, tint selection in managers) becomes trivial: read `app.palette.X` instead of hard-coding `Color::Gray`. This decouples *what* a role means (stable) from *how* it is painted (user-swapable).

## Goals

1. Add `kb tui --palette PATH` (and `kb tui --list-palettes` to list built-ins).
2. Define a `Palette` struct with named roles covering **every** `Style::` usage in `kb/tui/render.rs` and `kb/tui/mod.rs`.
3. **Default** palette (shipped, embedded) preserves the current colors so existing look does not regress.
4. **Colorblind** palette (shipped, embedded): no red/green pair, double-encoded with symbols (which we already do: `▸` / `◇` / `·` / `»`) so information survives in monochrome.
5. **File override**: `--palette PATH` reads a TOML file. Every role is **required** (fail loudly on missing role, not implicit default).
6. Refactor `render.rs` and `mod.rs` to read `app.palette.X` — no more inline `Style::default().fg(Color::Gray)`.
7. (Follow-on, same pass) Ship findings B–E from the audit against the new palette — each is a small render-only change now that the vocabulary exists.

## Non-goals (in this pass)

- Theming (dark/light). That's a *palette* job.
- Syntax highlighting beyond markdown headings.
- Colorblind-specific **symbol** changes (already double-encoded; don't over-engineer).
- Persistence (e.g. `~/.config/kb/palette.toml` auto-loaded). That's a later ergonomics step — the flag is the contract.

## Design

### Roles (vocabulary)

The set of semantic roles that *every* screen uses. Every color in `render.rs` maps to one of these. Anything else is a bug.

| Role | Meaning (what the user sees) | Symbol (double-encoding, palette-independent) |
|---|---|---|
| `label` | Primary structural text: mode header, panel titles, page `type:` prefix, alias `→`, root flags. | *(none)* |
| `default` | Body text, row primary value, candidate text. | *(none)* |
| `dim` | Read-only context: existing items, `·` dots, "(none)" empty previews, path locator. | `·` |
| `active` | The one thing your keys act on right now. | `▸` or `»` |
| `caret` | Text cursor character. | `▮` |
| `hint` | Keybinding hints, "type to filter" line, `(added: ...)` metadata. | `·` |
| `gated` | A page that has a `search_required_phrase` gate present. | `◆` |
| `open` | A page with no gate (search sees it unconditionally). | `◇` |
| `info` | Status bar, info-class line. | *(none)* |
| `success` | Status bar, success-class line. | *(none)* |
| `error` | Status bar, error-class line. | `!` |
| `selection` | Highlight style (fg+bg) for the currently-selected row across **all** lists (browse / alias / roots / picker). | *(none)* |
| `selection_ctx` | Highlight style for the sub-selection (e.g. an item within a list value in Selection sub-mode). | *(none)* |

Notes:
- `label` is **not** the same role as `default`. `label` is structural; `default` is content. This is what fixes the "gray-on-gray-border" problem: a user who sets their border to gray can also set `label` to a distinguishable tone.
- `selection` and `selection_ctx` are **style** roles (fg+bg), not just colors — so `Palette` exposes them as `Style` values, not `Color`.
- `gated` / `open` live in the vocabulary because the browse view (finding B) and the edit picker (finding D) both use them. Keeping them in the palette means a colorblind user can choose how those symbols are colored independently.

### `Palette` struct

```rust
// kb/tui/palette.rs
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub label:      Style,   // structural
    pub default:    Style,   // content
    pub dim:        Style,   // read-only
    pub active:     Style,   // "your keys act on this"
    pub caret:      Style,   // text cursor
    pub hint:       Style,   // keybinding hint
    pub gated:      Style,   // ◆ (page has search_required_phrase)
    pub open:       Style,   // ◇ (page has no gate)
    pub info:       Style,
    pub success:    Style,
    pub error:      Style,
    pub selection:      Style,   // fg+bg, full-row
    pub selection_ctx:  Style,   // fg+bg, in-list item
}

impl Palette {
    /// The default palette — preserves the current colors exactly,
    /// so an existing install's screen is byte-identical.
    pub fn default() -> Self { ... }

    /// The colorblind-safe default. No red/green pair; doubles up on
    /// the symbol layer (which is palette-independent) so the semantic
    /// is recoverable even in monochrome.
    pub fn colorblind() -> Self { ... }

    /// Load from a TOML file. Every role is **required**.
    pub fn from_toml(s: &str) -> KbResult<Self> { ... }

    /// List of (name, description) for `kb tui --list-palettes`.
    pub fn built_in_names() -> &'static [(&'static str, &'static str)] { ... }
}
```

### TOML file format

```toml
# Every color can be a name ("gray", "cyan", "light-red"), a hex
# ("#00FFAA"), or an RGB triple ([r, g, b]). Every modifier is any
# combo of "bold" | "dim" | "italic" | "underline" | "reverse".

[label]     fg = "cyan"      bold = true
[default]   fg = "white"
[dim]       fg = "gray"
[active]    fg = "green"     bold = true
[caret]     fg = "yellow"    bold = true
[hint]      fg = "gray"
[gated]     fg = "green"     bold = true   # ◆
[open]      fg = "gray"               # ◇
[info]      fg = "gray"
[success]   fg = "green"
[error]     fg = "red"       bold = true

# selection is a full style (fg+bg)
[selection]       fg = "black"   bg = "white"
[selection_ctx]   fg = "white"   bg = "gray"   # in-list sub-selection
```

### Built-in palettes (shipped)

- **`default`** — today's colors, verbatim. This is the `no-flag` behavior.
- **`colorblind`** — no red/green pair. `error` → Magenta. `success` → Cyan. `active` → Blue. Everything else keeps its *role*, only the color shifts off the red/green axis. Symbols already double-encode the meaning, so the palette is strictly a recolor.
- **`mono`** (bonus) — all roles use `fg` only, `dim` uses the terminal's dim modifier, `selection` is `reverse`. Useful for testing that the *symbols* (not the color) carry the meaning.

Shipped as embedded files: `kb/tui/palettes/default.toml`, `kb/tui/palettes/colorblind.toml`, `kb/tui/palettes/mono.toml`, each `include_str!`'d at compile time. `kb tui --palette colorblind` (name) loads from embedded; `kb tui --palette path/to/file.toml` (path) reads from disk. Disambiguation: if the string is a known built-in name, use it; else open it as a path. (Alternative: use a `--palette-name` + `--palette-file` pair — pick one, I'll decide when writing.)

### App wiring

```rust
// kb/tui/mod.rs
pub struct App {
    // ...existing fields...
    pub palette: palette::Palette,   // NEW
}

pub fn run_tui(dbpath: Option<&Path>, palette: Option<&str>) -> i32 {
    // 1. Resolve: if Some("name") → built-in; else → file path.
    // 2. Load into palette::Palette (from_toml).
    // 3. App::new(db, palette).
    // 4. Rest unchanged.
}

impl App {
    pub fn new(db: Db, palette: palette::Palette) -> KbResult<Self> { ... }
}
```

`kb/main.rs`:
```rust
Cmd::Tui { palette } => {
    let code = tui::run_tui(cli.db.as_deref(), palette.as_deref());
    ...
}
```

where `Cmd::Tui` becomes:

```rust
#[cfg(feature = "tui")]
Tui {
    /// A built-in name ("default" | "colorblind" | "mono") or a path to
    /// a TOML file. Default (no flag) = "default".
    #[arg(long)]
    palette: Option<String>,

    /// Print the available built-in palettes and exit.
    #[arg(long)]
    list_palettes: bool,
},
```

### Hardened build

`Cmd::Tui` is already `#[cfg(feature = "tui")]`. The new `palette` module and its deps must be the same — `palette.rs` should live under `kb/tui/` (which is `#[cfg(feature = "tui")]`) and add `serde`/`toml` to **kb-user only**, feature-gated. The hardened build never links them.

## Phases (each a commit, each gated by `gate.sh`)

- [ ] **P1. Introduce `palette.rs` with the three built-ins (embedded TOMLs + parse).** No render change yet. Only compiles if `tui` is on. `cargo test -p kb-user` green.
- [ ] **P2. Wire the `--palette` / `--list-palettes` flag** into `kb/main.rs` and thread it to `App::new`. `Palette::default()` (no flag) is the behavior; `run_tui` returns a clean error if the flag is set but the path/name is invalid.
- [ ] **P3. Refactor `render.rs` to read `app.palette.X`.** No change from `--palette default` (visual diff = 0). This is the big mechanical refactor — the point is to prove the palette covers **every** style in the file.
- [ ] **P4. Add the shipped `colorblind` + `mono` palettes.** Prove visually with `shotwin` that `--list-palettes` works, `--palette colorblind` changes the colors but keeps the same layout/symbols.
- [ ] **P5. `kb tui --list-palettes` UX** — print a terse table: name, one-line description, a preview of the key roles (label/active/error/selection) as ANSI-escaped so the user can actually *see* them in the terminal.
- [ ] **P6. (follow-on, audit finding B)** Browse row as `Vec<Span>` using `palette.label/default/dim/hint/gated/open`.
- [ ] **P7. (follow-on, audit finding C)** Read view: markdown H1–H6 styling using `palette.label/default`.
- [ ] **P8. (follow-on, audit finding D)** Edit picker: `◆`/`◇` empty markers using `palette.gated/open`.
- [ ] **P9. (follow-on, audit finding E)** Alias/Roots `highlight_style` = `palette.selection` (was "none"), plus `→` in `palette.label`.
- [ ] **P10. (follow-on, audit finding F)** `apply_filter` also matches tags + `search_required_phrase`.
- [ ] **P11. CHANGELOG entry under `[Unreleased]`: Added `--palette`; Added built-in `colorblind` + `mono`; Renamed: internal styles now read from `Palette`.**

## Verification gate (every phase)

- `./gate.sh` green: build, one-pkg build (kb-user + kb-hardened), clippy -D warnings, cargo test --workspace, Verus, behavior, dep-tree.
- **Behavioral:** `kb tui --list-palettes` prints the built-ins; `kb tui --palette badcolor` exits with a clear error; `kb tui --palette <path>` loads; `kb tui --palette colorblind` uses the shipped file.
- **Visual (via `shotwin`):** 3 screens × 3 palettes (default, colorblind, mono). The layout must be *identical* (same borders, same symbols, same text) — only the colors differ. Any shape change = a regression in that phase.

## Working recipe: drive + capture without fighting for focus

> *Context: another agent is concurrently running a windowed GUI. I must be able to drive **and** capture my TUI window by XID, without requiring it to be the active/focused window, and without killing unrelated window-manager state.*

- Terminal: **`terminator`** (user's default; xterm's font is unreadable).
- **Launch:**
  ```bash
  setsid terminator \
    -T "kbpalette"            # unique WM title (searchable)
    -r "kbpalette-uniq-X"     # unique WM_WINDOW_ROLE (unambiguous)
    -e "$KB tui --db $DB --palette <name>" \
    </dev/null >/dev/null 2>&1 & disown
  ```
  The role string is the stable handle.
- **Find by role (title works too):**
  ```bash
  for w in $(xdotool search --class terminator); do
    [ "$(xprop -id $w WM_WINDOW_ROLE 2>/dev/null | grep -v not)" = ... ] \
      && echo "XID=$w"
  done
  # or: xdotool search --name kbpalette
  ```
- **Capture** with the existing helper (already works by XID, no focus needed):
  ```bash
  shotwin -o "$OUT.png" "kbpalette"
  ```
  `shotwin` calls `import -window $xid`, which reads the window's **backing store** — so the capture is correct even when my window is occluded or not focused. (Verified: captured 654×426 sRGB while a different window was active.)
- **Send keys** to a specific window **by XID** (not to the active window):
  ```bash
  xdotool key  --window $XID Return
  xdotool type --window $XID --delay 80 "security"
  ```
  `--window $XID` targets a *specific* window regardless of focus order. This is the difference between "works when another GUI is stealing focus" and "only works when I'm active."
- **Clean exit** (do NOT `pkill terminator` — it is a *single process* managing all windows; killing it takes every terminator window, including the GUI agent's):
  ```bash
  xdotool key --window $XID q          # let the TUI's own `q` clean up
  # then, once the TUI exits, the shell child terminates and terminator
  # reclaims the pane naturally. Do NOT kill the terminator process.
  ```
- **If a window must be gone** and the TUI didn't respond to `q`:
  ```bash
  xdotool windowkill $XID    # sends WM_DELETE, closes just that window
  ```
  This is still safe — terminator is one process but `windowkill` only asks the WM to close *one* window. The terminator process stays up.

## Open questions (resolve during P2)

1. **Flag shape**: `--palette <name|path>` (disambiguate by whether the string matches a built-in name) vs. `--palette-name <name>` + `--palette-file <path>`. I lean toward the first (fewer flags, self-describing), but I'll confirm before P2 lands.
2. **TOML vs. JSON**: TOML is more human-editable and matches the "ship a file the user can edit" story. `kb-core` doesn't need TOML (it's the pure/verified crate). I'll add `toml = "0.8"` to `kb-user` only, feature-gated by `tui`. Confirm acceptable.
3. **Precedence** (future, not this pass): env var `KB_PALETTE`, then `--palette`, then default. This pass: flag + default only.
4. **`--list-palettes` output format**: I lean ANSI-colored (so the user can *see* the palette, not just read its name). Confirm.
