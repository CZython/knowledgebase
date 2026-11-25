//! kb-gui — native egui desktop client for `kb` (PLAN-GUI.md).
//!
//! G0: eframe window, built-in themes, the three-pane shell painting in the
//! first frame with skeleton content and NO database work, a DB worker thread
//! behind `Cmd`/`Evt` channels spawned after frame 1, argv launch routing, and
//! a `--startup-report` / `KB_GUI_TRACE_STARTUP` first-frame timer.

use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;

mod app;
mod data;
mod preferences;
mod state;
mod theme;
mod ui;

/// The time of process entry — the anchor for the first-frame budget
/// (PLAN-GUI §3.4: the budget is measured from process spawn, not from when
/// the event loop starts handing us frames). Evaluated on the first touch.
fn process_start() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// kb-gui — a native keyboard-first desktop client for the kb knowledge base.
#[derive(Debug, Parser)]
#[command(
    name = "kb-gui",
    version,
    about = "kb knowledge base, desktop edition (egui, native, no webview)."
)]
struct Args {
    /// Database file to open. Default: `~/.local/share/knowledgebase/kb.db`
    /// (the same DB the `kb` CLI uses).
    #[arg(long)]
    db: Option<PathBuf>,

    /// Theme: `dark` (default), `light`, or `system`.
    #[arg(long, value_name = "dark|light|system", default_value = "dark")]
    theme: String,

    /// Print the spawn→first-frame timing at exit (also `KB_GUI_TRACE_STARTUP=1`).
    #[arg(long)]
    startup_report: bool,

    /// Quit after this many frames (0 = run until the window is closed). Used
    /// by the headless smoke test to get a deterministic first-frame run.
    #[arg(long, default_value_t = 0)]
    max_frames: u32,

    /// A Markdown page to open after the shell appears (`file` or
    /// `file#Section`). Optional — without it, the app lands on Home.
    page: Option<String>,
}

/// Route an argv page spec into a [`state::LaunchRoute`] (PLAN-GUI §3.4:
/// "a path argument loads asynchronously; the shell appears first, then the
/// file"). `file.md#Section` splits on the first `#`.
fn route_from_arg(page: Option<String>) -> state::LaunchRoute {
    match page {
        None => state::LaunchRoute::Home,
        Some(spec) => match spec.split_once('#') {
            Some((file, section)) => state::LaunchRoute::OpenSection {
                file: file.to_string(),
                section: section.to_string(),
            },
            None => state::LaunchRoute::OpenFile(spec),
        },
    }
}

/// Whether the first-frame timing print is requested (flag or env).
fn trace_startup() -> bool {
    std::env::var_os("KB_GUI_TRACE_STARTUP")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// The recorded spawn→first-paint time in milliseconds (the single source of
/// truth the `--startup-report` / smoke test reads). A module-level shared
/// static so `record_first_frame` and `first_frame_ms` see the same
/// `OnceLock` (two same-named statics in two function bodies are NOT the
/// same variable).
fn ms_static() -> &'static std::sync::OnceLock<u64> {
    static MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    &MS
}

/// The recorded spawn→first-paint time in milliseconds, or `None` if frame 1
/// has not painted yet. Public so the startup test (and `--startup-report`)
/// can read a single source of truth instead of re-anchoring the clock.
pub(crate) fn first_frame_ms() -> Option<u64> {
    ms_static().get().copied()
}

/// Called exactly once, on the first frame that actually paints the shell.
/// Prints the budget verdict when `--startup-report` / `KB_GUI_TRACE_STARTUP`
/// is on; records the value into the process-global the tests read.
pub(crate) fn record_first_frame() {
    let ms = process_start().elapsed().as_millis() as u64;
    if ms_static().get().is_some() {
        return; // already recorded
    }
    let _ = ms_static().set(ms);
    let report = state::StartupReport::new(ms);
    if trace_startup() {
        eprintln!("[kb-gui] {}", report.line());
    }
}

pub fn main() {
    let args = Args::parse();
    let want_report = args.startup_report || trace_startup();

    let theme = match args.theme.as_str() {
        "light" => state::ThemeChoice::Light,
        "system" => state::ThemeChoice::System,
        _ => state::ThemeChoice::Dark,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    let launch = route_from_arg(args.page);
    let max_frames = args.max_frames;
    // Resolve the theme *once* so frame 1 and the steady-state app agree
    // (PLAN-GUI §11.3: no theme flicker between the `CreationContext` and
    // the first `update`).
    let resolved_theme = app::App::theme_for(theme);
    let result = eframe::run_native(
        "kb",
        options,
        Box::new(move |cc| {
            // Zero-cost when `EGUI_INSPECTION` is unset; `EGUI_INSPECTION=1`
            // enables the TCP wire server on 127.0.0.1:5719 for dev
            // (tree read / event injection / screenshots — see
            // ~/egui-inspection-instructions.md).
            let _ =
                egui_inspection::attach_from_env(&cc.egui_ctx, Some(env!("CARGO_PKG_NAME").into()));
            crate::theme::apply(&cc.egui_ctx, &resolved_theme);
            let mut app = app::App::new(args.db.clone(), launch.clone(), theme);
            app.set_max_frames(max_frames);
            Ok(Box::new(app))
        }),
    );

    if let Err(e) = result {
        eprintln!("kb-gui: {e}");
        std::process::exit(1);
    }

    if want_report {
        match first_frame_ms() {
            Some(ms) => eprintln!("[kb-gui] {}", state::StartupReport::new(ms).line()),
            None => eprintln!("[kb-gui] first frame never painted (window closed early?)"),
        }
    }
}
