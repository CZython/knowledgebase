# TODO — 5-task roadmap

Status legend: `[ ]` not started · `[~]` in progress · `[x]` done

---

## T1 — GUI (egui desktop client)

Spec: `PLAN-GUI.md`. Crate: new `kb-gui` in the workspace.

- [x] G0 — Foundation
  - [x] Add `kb-gui` cargo package (binary) to the workspace; pin `egui`/`eframe` versions
  - [x] Startup sequence within budget: open DB, load roots, first frame (PLAN-GUI §3.4)
  - [x] Window layout skeleton: top bar / collections rail / note list / editor host / status bar (§6.1–6.7)
  - [x] DB access off the render thread (§3.2); source-of-truth contract (§3.3)
- [x] G1 — All pages + collections + search parity
  - [x] Pages list grouped by root/collection (§2.1, 6.3–6.4)
  - [x] Search overlay with parity to `kb search` and `kb qs` (§6.8)
  - [x] Launch states: no path/ref → home (§6.13); no roots → welcome + add-root flow (§6.14)
- [x] G2 — Raw editor + save + frontmatter
  - [x] Raw Markdown editor, save to disk, dirty tracking (§9.2)
  - [x] Frontmatter inspector (§6.9)
- [x] G3 — Formatted read view + generic Markdown (IR+parser `ui/md.rs`, renderer `ui/render.rs`, `DocumentKind`+classify, §6.15 banner + Convert dialog `ui/convert.rs`, `Cmd::ConvertPage` worker, inspector gating)
  - [x] Read-only rendered view, no embedded browser (§7)
  - [x] Generic Markdown + conversion rules (§6.15) — e2e: pure-body file → indexed note
  - [x] G4 — WYSIWYG block editing MVP
    - [x] Block-span model + splice + round-trip guarantees (§8) — `ui/splice.rs`, 28 tests
    - [x] Read-wysiwyg / edit-plain toggle — `ui/block_edit.rs` (click block → TextEdit, Esc exits)
    - [x] WYSIWYG editing of *simple tight* lists (§8) — whole list = one splicable unit, multiline TextEdit; Enter inserts an item, Up/Down navigate lines; complex lists (task/loose/nested) fall back read-only — `splice.rs` (`BlockKind::List` + `tight_list_span_for`), `md.rs` (`list_item_text_from_line`), 11 new tests
   - [x] Shell rebuilt on egui `Panel` (left/right/top/bottom) + `CentralPanel` (`ui/mod.rs`)
   - [x] Banner as a vertical multi-line layout (heading / description / buttons) — `preview::paint_banner`
   - [x] Single tabbed left panel (`Collections | Notes`, only active tab visible) — `LeftPaneKind` in
     `state.rs`, consolidated `Panel::left("kb.left")` + `left_pane` tab bar in `ui/mod.rs`
 - [x] G5 — Settings and corpus management parity
   - [x] Roots add/drop, refresh, tag aliases visible in UI (§6.12) — `kb-gui-g5`,
     Settings modal (`ui/settings.rs`) + `data/admin.rs`, `☰` app menu, `Cmd/Ctrl+,`,
     Roots / Tag-aliases / Data tabs + 6 unit tests
- [~] G6 — Polish: theming/custom themes (§11), keyboard shortcuts (§6.11), dialogs (§6.10)
  - [x] G6-1 — Theming core (§11) + preferences persistence — `kb-gui-g6`
    - [x] Semantic token model `theme.rs`: `Color` (hex serde), `ColorTokens`/`MarkdownTokens`/`Metrics`/`Fonts` (all `#[serde(default)]`, `deny_unknown_fields`), built-ins `marknote-dark` / `marknote-light` / `high-contrast` (pass WCAG AA `contrast_audit`), `to_visuals()` + `apply()`, custom-TOML loader `from_toml_path()` (≤64 KB) — 16 unit tests
    - [x] `preferences.rs`: §11.3 schema persisted to `<config>/kb-gui/prefs.toml` (load/save non-fatal) — 5 unit tests
    - [x] Settings **Appearance** tab (built-in + custom-TOML picker, live switch, WCAG contrast audit view) and **General** tab (restore-last-page / privacy / editor view / autosave) — `ui/settings.rs`
    - [x] Frame-1 and steady-state theme agree (single `App::theme_for` resolver; `--theme` CLI > prefs > default)
- [ ] G7 — Packaging and release (shares infrastructure with T5)
- [ ] Testing: unit, integration, manual matrix, startup measurement, CI gates (§13)

---

## T2 — MCP server

Spec: `MCP.md`. Home: `kb-hardened`, hidden `kb mcp` subcommand, stdio JSON-RPC.

- [ ] JSON-RPC over stdio skeleton inside `kb-hardened` (hand-rolled or minimal dep, no network)
- [ ] `initialize` / `tools/list` / `tools/call` for the curated tool surface (MCP §"Tool surface")
- [ ] Sanitizer chokepoint: `kb/mcp/sanitizer.rs`, no-path-escape + no-network properties with tests (§"The sanitizer")
- [ ] Identity: uuid in, path never out (§"Identity")
- [ ] Per-page opt-out honored: `mcp_hidden: true` frontmatter (§"Per-page opt-out")
- [ ] Hidden subcommand: not in `--help`, documented as hidden (verification checklist §"The surface is hidden")
- [ ] Verification checklist green: works, no network, escape property tests, build gate (§"Verification checklist")
- [ ] Resolve open decisions (MCP §"Open decisions (blocked on the user)")

---

## T3 — E2E test: install → create → search → toc → refresh

- [ ] Write cross-platform e2e script: `cargo install` from tag → create root → create pages → `kb search` → `kb toc` → `kb refresh`; assert expected JSON/exit codes
- [ ] Run green on Linux (native + musl binary)
- [ ] Run on Windows: **Win10 VM in QEMU/KVM is available for local instrumentation** (SSH/VNC in; pass PAT + `--db` as env, not args)
  - [ ] Capture the known-Windows problems and file/fix them here before CI (see T4)
- [ ] Parameterize: `--db` path, page count, assertions as a single shared harness (shell or Rust test)

T3/T4 note: Windows problems spotted up front — the known suspect areas: path quoting in shell scripts, `~`/`USERPROFILE` data-dir resolution, `kb.db-wal`/`kb.db-shm` file locking under Windows, `cargo install` exit-code surfacing, and UTF-8 in the Windows console. Confirm each or clear it on the VM.

---

## T4 — E2E in CI (Linux + Windows)

- [ ] `ci.yml`: add `e2e-linux` job (runs T3 harness on the linux-musl binary)
- [ ] `ci.yml`: add `e2e-windows` job (MSVC runner; `cargo install` → T3 harness via PowerShell, not bash)
- [ ] Ensure the 5 gates still run and auto-merge-dev fires on all green
- [ ] Gate T3 harness as its own artifact or checked-in script so both jobs share one source of truth

---

## T5 — Native packaging

Spec: `PACKAGING.md`. Users need a clickable icon per platform.

- [ ] `dist/nfpm.yaml` + `dist/package-linux.sh` → `.deb` and `.pkg.tar.zst`
- [ ] `dist/package-appimage.sh` + `AppDir`/`AppRun` → `.AppImage`
- [ ] Windows `.msi` via cargo-wix (author + commit WiX files)
- [ ] macOS `.pkg` via `pkgbuild`
- [ ] **Uninstall-keeps-pages contract** per platform: delete DB + sidecars + (empty) data dir, never Markdown pages
  - [ ] Linux: generated `postrm` / `kb_uninstall_data()` function
  - [ ] AppImage: `--uninstall`
  - [ ] Windows: MSI cleanup (D9 sibling-package guard)
  - [ ] macOS: bundled `kb-uninstall` helper (receipts, not `command -v`)
- [ ] `release.yml`: platform matrix (linux, windows, macos), upload globs, checksums job
- [ ] Icons: ship platform icons (`.png`/`.ico`/`.icns`) in the packages so installers show a clickable icon
- [ ] Verification matrix per platform (PACKAGING §"Verification matrix"), including Windows smoke test on the VM
- [ ] Phase 4 (later): signing + notarization, needs GITHUB secrets

---

## Sequence (fixed, user-ordered)

T1 → T2 → T3 → T4 → T5.

Win10 QEMU/KVM VM is available for T3/T4 (Windows e2e) and T5 (Windows packaging smoke tests).
