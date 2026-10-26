// D4: the CLI's error model lives in `kb/cli/error` (not the crate root
// or `kb/main.rs`) so the name travels with the code: a new verb file can
// `use crate::cli::*` or `use crate as _;` and find `Kb` alongside everything
// else the CLI shares (json::JObject, the exit codes, etc.), instead of
// having to know that the alias is declared one specific file deeper in
// `kb/main.rs`.
//
// The shape `Result<i32, (String, i32)>` is deliberately *not* in `kb-core`:
// the left side (process exit code) is a CLI concern, not a pure-Rust one
// (`kb-core` is Verus-proven, no `std::process::ExitCode`). The right side
// (message + code) is what `?` over the whole body composes against.

/// A success code, or `(message, exit-code)` for a typed failure. `?` over any
/// `Result<_, (String, i32)>` therefore unwinds the whole command with the
/// right exit code, so fallible steps compose cleanly.
pub(crate) type Kb = Result<i32, (String, i32)>;
