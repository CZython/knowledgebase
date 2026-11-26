//! `kb mcp` — MCP server surface for the knowledge base (MCP.md).
//!
//! The module lives in the *shared* CLI body (`kb/main.rs` `include!`s it
//! into both `kb-user` and `kb-hardened`) and is feature-gated behind
//! `feature = "mcp"`:
//!
//! - **enabled** in `kb-hardened` (`cargo build -p kb-hardened --features mcp`);
//! - **absent** from `kb-user` (there is no `mcp` feature in
//!   `kb-user/Cargo.toml`) — the subcommand is compile-time absent, not
//!   merely hidden (MCP.md locked requirement #3).
//!
//! When enabled, the loop is a `read()`/`write()` pair over
//! stdin/stdout — no socket, no `connect`, no DNS lookup (all `strace`ed
//! to zero for the `mcp` build too, by the no-network gate in `gate.sh`).
//! The server speaks JSON-RPC 2.0 (lines, one per line, per the MCP stdio
//! transport).

pub(crate) mod sanitizer;
pub(crate) mod tools;
#[cfg(feature = "mcp-writes")]
pub(crate) mod write;

use std::path::Path;

/// The MCP server entry point — the stdio JSON-RPC 2.0 loop.
///
/// Reads JSON-RPC 2.0 frames (newline-delimited) from **stdin** and writes
/// responses + notifications to **stdout**. Every log / diagnostic goes to
/// **stderr** (the stdout channel is sacred to the protocol).
///
/// # Return
/// A `Kb` (`Result<i32, (String, i32)>`, the shared-CLI verb convention):
/// `Ok(EXIT_OK)` when stdin closed cleanly (the normal EOF path);
/// `Err((msg, EXIT_ERR))` on any fatal failure (bad read, dispatch panic,
/// …) — `run()` prints `kb: {msg}` with `EXIT_ERR` and exits.
///
/// # Feature gating
/// The `serve` body is `#[cfg(feature = "mcp")]`-compiled out entirely on
/// the `kb-user` default build. `mcp-writes` (off by default) is the knob
/// that adds the write-only tools to the `tools/list` surface; when off,
/// the server is strictly read-only (MCP.md locked requirements).
#[cfg(feature = "mcp")]
pub(crate) fn serve(dbpath: Option<&Path>) -> crate::cli::error::Kb {
    match tools::serve(dbpath) {
        crate::EXIT_OK => Ok(crate::EXIT_OK),
        code => Err((
            "kb mcp serve: fatal failure (see stderr)".into(),
            code.max(crate::EXIT_ERR),
        )),
    }
}
