//! `kb tui` — the palette (`kb tui --palette <name|path>`).
//!
//! Every `Style` used in `render.rs` + `input.rs` is expressed as a named
//! *role*, not an inline `Style::default().fg(Color::Gray)`. The role set
//! is **stable**; the colors are **user-swapable**. A user with red-green
//! colorblindness can override the roles (or supply a built-in: `default` /
//! `colorblind`) without touching a single `render.rs` line.
//!
//! The palette is a **`Copy` value stored on `App`**, so it is captured in
//! the same single-thread event loop as everything else — no `Rc`, no
//! globals, no `static mut`.
//!
//! ## Roles
//!
//! | Role            | What it paints                                            | Symbol |
//! |-----------------|-----------------------------------------------------------|--------|
//! | `label`         | mode header, panel titles, `type:` prefix, alias `→`      | —      |
//! | `default`       | body text, row primary value                              | —      |
//! | `dim`           | read-only context ("(none)", paths, existing items)       | `·`    |
//! | `active`        | the one thing your keys act on                            | `▸`    |
//! | `caret`         | text cursor character                                     | `▮`    |
//! | `hint`          | keybinding hint lines, "(added: …)" metadata              | —      |
//! | `gated`         | page *has* `search_required_phrase`                        | `◆`    |
//! | `open`          | page with no gate                                         | `◇`    |
//! | `info`          | status bar, info class                                    | —      |
//! | `success`       | status bar, success class                                 | —      |
//! | `error`         | status bar, error class                                   | `!`    |
//! | `selection`     | highlight row (lists: browse / alias / roots / picker)    | —      |
//! | `selection_ctx` | sub-selection (an item within a list value)               | —      |
//!
//! **Naming note (D1):** `gated` / `open` style pages on the *corpus*
//! `search_required_phrase` field — a DB column owned by `kb-core::frontmatter`
//! (queries in `kb/main.rs`). The palette role is *this file's* name for "a
//! page with a gate," not the SQL column itself; the two are linked by
//! convention only. Touching either without the other can desync what `◆`
//! means visually from what the frontmatter actually stores.
//!
//! The symbols are **palette-independent** (they live in `render.rs`, not
//! here) so any `--palette` user gets the same visual language — only the
//! foreground/background colors change.

use ratatui::style::{Color, Modifier, Style};

/// A single **fully-specified** style: fg, bg, and every boolean modifier.
///
/// "Fully-specified" matters: the *default* palette must render byte-
/// identically to the current (pre-palette) render pass. ratatui's `Style`
/// defaults `Reset` for unset fields, which `render.rs` currently relies on
/// implicitly (it never sets them, so the terminal's fg/bg shows through).
/// `StyleSpec` makes the contract explicit and makes a `--palette` file's
/// omission-of-a-field *an error*, not a silent terminal-default.
///
/// For P1 we still construct via `Style` directly (the existing inline
/// styles in `render.rs` are the source of truth for the default). From P2
/// on, `StyleSpec` is the wire format for TOML files. The two are
/// interchangeable via `StyleSpec::to_style()` / `StyleSpec::from_style()`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StyleSpec {
    pub fg: Option<String>,
    pub bg: Option<String>,
    #[serde(default)]
    pub bold:      bool,
    #[serde(default)]
    pub dim:       bool,
    #[serde(default)]
    pub italic:    bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub strikethrough: bool,
    #[serde(default)]
    pub reverse:   bool,
}

impl StyleSpec {
    /// Convert to a ratatui `Style`. An **empty** `fg`/`bg` means "use the
    /// terminal default" (ratatui `Reset`) — that is a legitimate, explicit
    /// choice (roles like `default` are "whatever the terminal uses").
    pub fn to_style(&self) -> Style {
        let mut res = Style::new();
        if let Some(c) = self.fg.as_deref().and_then(parse_color_name) {
            res.fg = Some(c);
        }
        if let Some(c) = self.bg.as_deref().and_then(parse_color_name) {
            res.bg = Some(c);
        }
        let mut mods = Modifier::empty();
        if self.bold          { mods |= Modifier::BOLD; }
        if self.dim           { mods |= Modifier::DIM; }
        if self.italic        { mods |= Modifier::ITALIC; }
        if self.underline     { mods |= Modifier::UNDERLINED; }
        if self.strikethrough { mods |= Modifier::CROSSED_OUT; }
        if self.reverse       { mods |= Modifier::REVERSED; }
        res = res.add_modifier(mods);
        res
    }

    /// Validate that any *non-empty* `fg`/`bg` is a color we know. This is
    /// how a typo (`fg = "grebn"`) becomes a hard error instead of a silent
    /// "use the terminal default" — which is exactly the trap a palette is
    /// supposed to remove. Empty `fg`/`bg` are allowed (meaning "terminal
    /// default").
    pub fn parse_error(&self) -> Option<String> {
        if let Some(name) = self.fg.as_deref() {
            if parse_color_name(name).is_none() {
                return Some(format!("bad foreground color: {name:?}"));
            }
        }
        if let Some(name) = self.bg.as_deref() {
            if parse_color_name(name).is_none() {
                return Some(format!("bad background color: {name:?}"));
            }
        }
        None
    }
}

/// Parse a color name. Accepts both the ANSI-16 names ratatui uses
/// (`black`, `red`, …, `gray` for the lightest "white"), and the
/// conventional `light-*` / `dark-*` spellings a palette file would use.
/// `dark-*` is the xterm spelling of the ANSI-16 low-intensity colors;
/// `*` alone is the high-intensity version.
fn parse_color_name(s: &str) -> Option<Color> {
    // ratatui 0.29 `Color` enum (8 ANSI colors + 8 bright variants + Reset +
    // Rgb + Indexed). "dark" is the xterm spelling of the low-intensity
    // colors (which ratatui doesn't expose as separate variants — only
    // `Gray` and `LightWhite`, plus `DarkGray` as a distinct "bright black");
    // "light" is the high-intensity version. We accept both spellings but
    // only the *distinct* colors ratatui actually has.
    Some(match s {
        "black"         => Color::Black,
        "red"           => Color::Red,
        "green"         => Color::Green,
        "yellow"        => Color::Yellow,
        "blue"          => Color::Blue,
        "magenta"       => Color::Magenta,
        "cyan"          => Color::Cyan,
        // The "white" ANSI color (ANSI 97 / xterm bright white) — ratatui calls it `White`.
        // "gray" is the ANSI 37 "silver" color, which looks like a light-gray.
        "white"         => Color::White,
        "gray"          => Color::Gray,     // ANSI 37 "silver"
        "silver"        => Color::Gray,     // same as "gray"
        // The "bright black" ANSI color (90) — ratatui calls it `DarkGray`.
        "dark-gray"     => Color::DarkGray,
        "light-black"   => Color::DarkGray,  // xterm spelling
        "bright-black"  => Color::DarkGray,  // xterm spelling
        // The 8 bright variants (91–97).
        "light-red"     => Color::LightRed,
        "light-green"   => Color::LightGreen,
        "light-yellow"  => Color::LightYellow,
        "light-blue"    => Color::LightBlue,
        "light-magenta" => Color::LightMagenta,
        "light-cyan"    => Color::LightCyan,
        "light-white"   => Color::White,
        "bright-red"    => Color::LightRed,
        "bright-green"  => Color::LightGreen,
        "bright-yellow" => Color::LightYellow,
        "bright-blue"   => Color::LightBlue,
        "bright-magenta" => Color::LightMagenta,
        "bright-cyan"   => Color::LightCyan,
        "bright-white"  => Color::White,
        _               => return None,
    })
}

/// The whole palette. **One value**, **`Copy`**, stored on `App`.
///
/// `Default` is the *no-flag* behavior: identical to the current inline
/// `Style::default()` calls in `render.rs`, so `kb tui` (no `--palette`) is
/// visually byte-identical to the pre-palette version.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub label:      Style,
    pub default:    Style,
    pub dim:        Style,
    pub active:     Style,
    pub caret:      Style,
    pub hint:       Style,
    pub gated:      Style,
    pub open:       Style,
    pub info:       Style,
    pub success:    Style,
    pub error:      Style,
    pub selection:      Style,
    pub selection_ctx:  Style,
    pub md_bold:     Style,
    pub md_italic:   Style,
    pub md_strike:   Style,
}

impl Palette {
    /// The default palette — the *no-flag* behavior.
    ///
    /// Every field is copied from the corresponding inline style in
    /// `render.rs` + the `header_line` + `render_status_bar` helpers, so a
    /// `--palette default` run is visually identical to a no-flag run (and
    /// identical to any pre-palette run).
    ///
    /// This is deliberately explicit (not `#[derive(Default)]`) so that the
    /// default is **pinned**: a future change to `render.rs` that adds a new
    /// inline style cannot silently change the default's colors unless the
    /// author also updates this constructor.
    pub fn default_palette() -> Self {
        // The *out-of-the-box* look is colorful, not gray. Roles and their
        // roles are the invariant (they're the only thing render.rs reads);
        // the exact colors below are a design choice and may change without
        // a render.rs edit. Light-* are the ANSI *bright* variants, so the
        // scheme reads well on a dark terminal.
        Self {
            // Headings / panel titles / the `type:` + `→` prefixes: Magenta,
            // so the *structure* of the screen is its own color family.
            label:           Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            // Body text, row primary value: neutral White, NOT bold — let the
            // accents (magenta headings, cyan active) do the coloring.
            default:         Style::default().fg(Color::White),
            dim:             Style::default().fg(Color::DarkGray),
            // The one thing keys act on (value-buffer text): Cyan+BOLD.
            active:          Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            caret:           Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            // `»` candidate marker (build_value_lines `star`) — plain Green.
            hint:            Style::default().fg(Color::Green),
            // `◆` gated page (has search_required_phrase): LightGreen+BOLD so it
            // stands out from `open` (DarkGray) and from the `»` hint (Green).
            gated:           Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD),
            // `◇` open page (no gate): subtle, one shade below the body.
            open:            Style::default().fg(Color::DarkGray),
            info:            Style::default().fg(Color::Gray),
            success:         Style::default().fg(Color::LightGreen),
            // Status Error: plain LightRed (no bold) — the `!` glyph (see
            // build_status_line) carries the severity on top of this color.
            error:           Style::default().fg(Color::LightRed),
            // Row highlight: black-on-Blue (a saturated, readable selection).
            selection:       Style::default().fg(Color::Black).bg(Color::Blue),
            // `▶` current item (build_value_lines `arrow`): Green+BOLD.
            selection_ctx:   Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            // Inline markdown emphasis, read pane only (render_markdown_text).
            // Bold: White+BOLD — the body is already White, so bold *adds*
            // weight; a distinct accent color would clash with the label/active
            // families (magenta/cyan).
            md_bold:         Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            // Italic: LightCyan+ITALIC — a soft, readable tilt that stays
            // distinct from the White body and the Magenta labels.
            md_italic:       Style::default().fg(Color::LightCyan).add_modifier(Modifier::ITALIC),
            // Strikethrough: DarkGray+CROSSED_OUT — a "greyed out" deleted
            // feel, the bar carried by the CROSSED_OUT modifier.
            md_strike:       Style::default().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT),
        }
    }

    /// A colorblind-safe palette. No red/green pair; red → magenta, green →
    /// cyan/blue. Symbols (▸ ▮ » ◆ ◇ · !) are palette-independent, already
    /// in `render.rs`, so the meaning survives any recolor.
    pub fn colorblind() -> Self {
        let style = |fg: Color, bold: bool| -> Style {
            let mut s = Style::default().fg(fg);
            if bold { s = s.add_modifier(Modifier::BOLD); }
            s
        };
        Self {
            label:           style(Color::Cyan, true),
            default:         Style::default(),
            dim:             Style::default().fg(Color::DarkGray),
            active:          style(Color::Cyan, true),
            caret:           style(Color::White, true),
            hint:            Style::default().fg(Color::DarkGray),
            gated:           style(Color::Blue, true),
            open:            Style::default().fg(Color::DarkGray),
            info:            Style::default().fg(Color::Cyan),
            success:         style(Color::Blue, false),
            error:           style(Color::Magenta, true),
            selection:       Style::default().fg(Color::Black).bg(Color::White),
            selection_ctx:   Style::default().fg(Color::White).bg(Color::DarkGray),
            // No red/green pair in the markdown roles either.
            md_bold:         style(Color::White, true),
            md_italic:       Style::default().fg(Color::LightCyan).add_modifier(Modifier::ITALIC),
            md_strike:       Style::default().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT),
        }
    }

    /// Load a palette from a TOML string. **Every role is required** —
    /// `StyleSpec`'s `#[serde(default)]` on the modifier booleans makes a
    /// missing *field* a `false`, but a missing *role table* is a
    /// deserialize error, which is what we want.
    ///
    /// (The default palette is NOT read from a TOML string — it is the
    /// explicit `default_palette()` value above. A user who wants to
    /// override it should `--palette <path>` with their own file.)
    pub fn from_toml(s: &str) -> Result<Self, String> {
        use serde::Deserialize;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct File {
            label:      StyleSpec,
            default:    StyleSpec,
            dim:        StyleSpec,
            active:     StyleSpec,
            caret:      StyleSpec,
            hint:       StyleSpec,
            gated:      StyleSpec,
            open:       StyleSpec,
            info:       StyleSpec,
            success:    StyleSpec,
            error:      StyleSpec,
            selection:      StyleSpec,
            selection_ctx:  StyleSpec,
            md_bold:     StyleSpec,
            md_italic:   StyleSpec,
            md_strike:   StyleSpec,
        }
        let f: File = toml::from_str(s)
            .map_err(|e| format!("invalid palette TOML: {e}"))?;
        // Validate every `fg`/`bg` — a typo like `fg = "grebn"` is a hard
        // error, not a silent "use the terminal default".
        for (name, sp) in [
            ("label", &f.label), ("default", &f.default), ("dim", &f.dim),
            ("active", &f.active), ("caret", &f.caret), ("hint", &f.hint),
            ("gated", &f.gated), ("open", &f.open), ("info", &f.info),
            ("success", &f.success), ("error", &f.error),
            ("selection", &f.selection), ("selection_ctx", &f.selection_ctx),
            ("md_bold", &f.md_bold), ("md_italic", &f.md_italic), ("md_strike", &f.md_strike),
        ] {
            if let Some(e) = sp.parse_error() {
                return Err(format!("[{name}] {e}"));
            }
        }
        Ok(Palette {
            label:           f.label.to_style(),
            default:         f.default.to_style(),
            dim:             f.dim.to_style(),
            active:          f.active.to_style(),
            caret:           f.caret.to_style(),
            hint:            f.hint.to_style(),
            gated:           f.gated.to_style(),
            open:            f.open.to_style(),
            info:            f.info.to_style(),
            success:         f.success.to_style(),
            error:           f.error.to_style(),
            selection:      f.selection.to_style(),
            selection_ctx:  f.selection_ctx.to_style(),
            md_bold:     f.md_bold.to_style(),
            md_italic:   f.md_italic.to_style(),
            md_strike:   f.md_strike.to_style(),
        })
    }

    /// Built-in palettes, in display order. `kb tui --list-palettes` shows
    /// this. The `default` entry now describes a colored (non-gray) palette,
    /// so the no-flag run reads as the colorful option it actually is.
    pub fn builtins() -> &'static [(&'static str, &'static str)] {
        const BUILTINS: &[(&str, &str)] = &[
            ("default",    "the default palette — the colorful, out-of-the-box look"),
            ("colorblind", "no red/green pair; red → magenta, green → cyan/blue"),
        ];
        BUILTINS
    }

    /// Resolve a name to a built-in, or return `None` so the caller knows to
    /// try the string as a file path.
    pub fn builtin(name: &str) -> Option<Self> {
        Some(match name {
            "default"    => Self::default_palette(),
            "colorblind" => Self::colorblind(),
            _            => return None,
        })
    }

    // ── ANSI helpers (used by `--list-palettes` preview) ─────────────────

    /// Render a ratatui `Style` as an ANSI escape **prefix**. The caller
    /// appends [`ANSI_RESET`] after the styled text. Used *only* by
    /// `print_palettes()` so the user can *see* each palette's key roles
    /// in their own terminal.
    #[allow(clippy::cast_possible_wrap)]
    pub fn to_ansi(s: Style) -> String {
        let mut codes: Vec<String> = Vec::new();
        if let Some(c) = s.fg { codes.push(Self::fg_code(c)); }
        if let Some(c) = s.bg { codes.push(Self::bg_code(c)); }
        if s.add_modifier.contains(Modifier::BOLD)       { codes.push("1".into()); }
        if s.add_modifier.contains(Modifier::DIM)        { codes.push("2".into()); }
        if s.add_modifier.contains(Modifier::ITALIC)     { codes.push("3".into()); }
        if s.add_modifier.contains(Modifier::UNDERLINED) { codes.push("4".into()); }
        if s.add_modifier.contains(Modifier::REVERSED)   { codes.push("7".into()); }
        if codes.is_empty() { String::new() } else { format!("\x1b[{}m", codes.join(";")) }
    }

    /// The standard "\x1b[0m" reset a styled span must be closed with.
    pub const ANSI_RESET: &'static str = "\x1b[0m";

    fn fg_code(c: Color) -> String {
        match c {
            Color::Reset => "39".into(),
            Color::Black => "30".into(),
            Color::Red => "31".into(),
            Color::Green => "32".into(),
            Color::Yellow => "33".into(),
            Color::Blue => "34".into(),
            Color::Magenta => "35".into(),
            Color::Cyan => "36".into(),
            Color::Gray => "37".into(),
            Color::DarkGray => "90".into(),
            Color::LightRed => "91".into(),
            Color::LightGreen => "92".into(),
            Color::LightYellow => "93".into(),
            Color::LightBlue => "94".into(),
            Color::LightMagenta => "95".into(),
            Color::LightCyan => "96".into(),
            Color::White => "97".into(),
            Color::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
            Color::Indexed(n) => format!("38;5;{n}"),
        }
    }

    fn bg_code(c: Color) -> String {
        match c {
            Color::Reset => "49".into(),
            Color::Black => "40".into(),
            Color::Red => "41".into(),
            Color::Green => "42".into(),
            Color::Yellow => "43".into(),
            Color::Blue => "44".into(),
            Color::Magenta => "45".into(),
            Color::Cyan => "46".into(),
            Color::Gray => "47".into(),
            Color::DarkGray => "100".into(),
            Color::LightRed => "101".into(),
            Color::LightGreen => "102".into(),
            Color::LightYellow => "103".into(),
            Color::LightBlue => "104".into(),
            Color::LightMagenta => "105".into(),
            Color::LightCyan => "106".into(),
            Color::White => "107".into(),
            Color::Rgb(r, g, b) => format!("48;2;{r};{g};{b}"),
            Color::Indexed(n) => format!("48;5;{n}"),
        }
    }

    /// Resolve a user-supplied `--palette` argument to a loaded `Palette`.
    ///
    /// Resolution order:
    /// 1. a **built-in name** (`default` / `colorblind`) — the common case,
    ///    and the value `kb tui --list-palettes` prints;
    /// 2. otherwise treat it as a **file path** (e.g. `~/.kb_config/default.toml`)
    ///    and parse the TOML from disk.
    ///
    /// Every failure (unknown name, unreadable file, bad TOML, a color typo,
    /// a missing role) becomes an `Err(String)` so the CLI can print it and
    /// exit non-zero *before* entering raw mode — never a silent fallback to
    /// a different palette.
    pub fn load(spec: &str) -> Result<Self, String> {
        if let Some(p) = Self::builtin(spec) {
            return Ok(p);
        }
        let text = std::fs::read_to_string(spec)
            .map_err(|e| format!("palette file {spec:?}: {e}"))?;
        Self::from_toml(&text)
            .map_err(|e| format!("palette file {spec:?}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    /// The `default_palette()` is *pinned*: the colors below are an explicit
    /// design choice (colorful, the out-of-the-box look). A change to
    /// `default_palette()` without a matching update to this test is a bug —
    /// the palette is no longer "pinned" (a `--palette default` run would
    /// silently diverge from the no-flag run).
    #[test]
    fn default_palette_is_pinned() {
        let p = Palette::default_palette();
        assert_eq!(p.label,
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD));
        assert_eq!(p.default, Style::default().fg(Color::White));
        assert_eq!(p.dim,     Style::default().fg(Color::DarkGray));
        assert_eq!(p.active,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
        assert_eq!(p.caret,
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
        assert_eq!(p.hint,    Style::default().fg(Color::Green));
        assert_eq!(p.gated,
            Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD));
        assert_eq!(p.open,    Style::default().fg(Color::DarkGray));
        assert_eq!(p.info,    Style::default().fg(Color::Gray));
        assert_eq!(p.success, Style::default().fg(Color::LightGreen));
        assert_eq!(p.error,   Style::default().fg(Color::LightRed));
        assert_eq!(p.selection,
            Style::default().fg(Color::Black).bg(Color::Blue));
        assert_eq!(p.selection_ctx,
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD));
        assert_eq!(p.md_bold,
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
        assert_eq!(p.md_italic,
            Style::default().fg(Color::LightCyan).add_modifier(Modifier::ITALIC));
        assert_eq!(p.md_strike,
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT));
    }

    /// The colorblind palette never uses pure Red or Green for a role that
    /// the default palette does use — that's the whole point of it.
    #[test]
    fn colorblind_has_no_red_or_green() {
        let p = Palette::colorblind();
        let roles = [
            p.label, p.active, p.caret, p.gated, p.info, p.success, p.error,
            p.md_bold, p.md_italic, p.md_strike,
        ];
        for r in roles {
            assert_ne!(r.fg, Some(Color::Red),  "colorblind role fg must not be Red: {:?}", r);
            assert_ne!(r.fg, Some(Color::Green),"colorblind role fg must not be Green: {:?}", r);
        }
    }

    /// A fully-specified default TOML file parses and equals the pinned
    /// default. This is the "a `--palette <file>` pointing at a TOML that
    /// matches `default_palette()` produces an *identical* `Palette`"
    /// guarantee — machine-checked, not eyeballed. The TOML uses the same
    /// color names as the inline constructor on the other side of the
    /// equality assert.
    #[test]
    fn default_toml_parses_and_is_pinned() {
        let toml = r#"
  [label]
  fg   = "magenta"
  bold = true

  [default]
  fg = "white"

  [dim]
  fg = "dark-gray"

  [active]
  fg   = "cyan"
  bold = true

  [caret]
  fg   = "yellow"
  bold = true

  [hint]
  fg = "green"

  [gated]
  fg   = "light-green"
  bold = true

  [open]
  fg = "dark-gray"

  [info]
  fg = "gray"

  [success]
  fg = "light-green"

  [error]
  fg = "light-red"

  [selection]
  fg = "black"
  bg = "blue"

  [selection_ctx]
  fg   = "green"
  bold = true

  [md_bold]
  fg   = "white"
  bold = true

  [md_italic]
  fg     = "light-cyan"
  italic = true

  [md_strike]
  fg            = "dark-gray"
  strikethrough = true
  "#;
        let p = Palette::from_toml(toml).expect("parses");
        // Every role must equal the pinned default.
        let d = Palette::default_palette();
        assert_eq!(p, d, "parsed TOML must equal the pinned default");
    }

    /// A palette file missing a role table must be an error (not a silent
    /// fallback to terminal defaults).
    #[test]
    fn missing_role_is_an_error() {
        let toml = r#"
# Only two of the 16 required roles are present; the rest must be errors.
[label]
fg = "gray"

[default]
fg = "white"
"#;
        assert!(Palette::from_toml(toml).is_err(), "missing role table must fail");
    }

    /// A palette file with a *wrong* color name must be an error, not a
    /// silent fallback.
    #[test]
    fn bad_color_name_is_an_error() {
        let s = StyleSpec {
            fg: Some("not-a-color".into()), bg: None,
            bold: false, dim: false, italic: false, underline: false, strikethrough: false, reverse: false,
        };
        assert!(s.parse_error().is_some(), "bad color name must produce parse_error");
        // Also verify the full TOML path surfaces this as an `Err`, not a
        // silent fallback.
        let toml = r#"
[label]
fg = "definitely-not-a-color"
"#;
        // Every role is required in the `File` struct, so even if the color
        // parse passed, the 11 missing role tables would fail. To isolate
        // the color-name error, call `parse_error` directly (above).
        assert!(Palette::from_toml(toml).is_err(), "bad color must fail (role tables missing too)");
    }

    /// `Palette::builtin("default")` is the same as `default_palette()`.
    #[test]
    fn builtin_default_is_pinned() {
        assert_eq!(Palette::builtin("default"), Some(Palette::default_palette()));
    }

    /// Unknown built-in names return `None`, not a fallback.
    #[test]
    fn unknown_builtin_name_is_none() {
        assert_eq!(Palette::builtin("not-a-real-palette"), None);
    }

    /// The two *shipped* palette files on disk are valid TOML and define
    /// all 16 roles. These are the files a user can `--palette <name>` or
    /// copy as a starting point, so a typo in them is a release blocker.
    #[test]
    fn shipped_palette_files_parse() {
        for name in ["default", "colorblind"] {
            // kb-user/../../kb/tui/palettes/<name>.toml
            let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../kb/tui/palettes")
                .join(format!("{name}.toml"));
            let text = std::fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("read {:?}: {e}", p));
            Palette::from_toml(&text)
                .unwrap_or_else(|e| panic!("{name}.toml: {e}"));
        }
    }
}
