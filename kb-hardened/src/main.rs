//! `kb-hardened` — the headless CLI (agent build): no TUI, no editor.
//!
//! Single source of truth is `kb/main.rs`; this file exists so cargo can
//! bind the file to a `[[bin]]` + a `clap` identity. The user-visible
//! differences from the `kb` build are exactly:
//!   - `[features] default = []` in `kb-hardened/Cargo.toml` (editor + tui off);
//!   - `name`/`version`/`about` selected via `cfg!(feature = "editor")`
//!     in the shared body (the `editor` proxy is OFF here → "kb-hardened"
//!     + `0.1.0 [hardened]`).
//!
//! The workspace lints (`Cargo.toml` `[workspace.lints.rust]`) pin the
//! whole project as `unsafe`-free (`unsafe_code = "forbid"`); this bin
//! inherits that, so a future `unsafe` block in the shared body or any
//! crate is a compile error unless deliberately allowed (C2).
include!("../../kb/main.rs");
