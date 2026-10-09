//! `kb-user` — the full CLI (default build): TUI + editor + every verb.
//!
//! Single source of truth is `kb/main.rs`; this file exists so cargo can
//! bind the file to a `[[bin]]` + a `clap` identity. The two lines below
//! are the *only* user-facing difference between this build and hardened:
//!   - `[features] default = ["editor", "tui"]` in `user/Cargo.toml`;
//!   - `name`/`version`/`about` selected via `cfg!(feature = "editor")`
//!     in the shared body (see `kb/main.rs::Cli`).
include!("../../kb/main.rs");
