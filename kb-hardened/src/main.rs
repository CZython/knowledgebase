//! `kb-hardened` — the headless CLI (agent build): no TUI, no editor.
//!
//! Single source of truth is `kb/main.rs`; this file exists so cargo can
//! bind the file to a `[[bin]]` + a `clap` identity. The user-visible
//! differences from the `user` build are exactly:
//!   - `[features] default = []` in `hardened/Cargo.toml` (editor + tui off);
//!   - `name`/`version`/`about` selected via `cfg!(feature = "editor")`
//!     in the shared body (the `editor` proxy is OFF here → "kb-hardened"
//!     + `0.0.0 [hardened]`).
include!("../../kb/main.rs");
