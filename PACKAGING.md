# `kb` — native package formats in GitHub Releases (design)

## Goal

The release pipeline already publishes **raw binaries** per platform
(`.github/workflows/release.yml`, produced by `dist/build-*.sh`). This design
adds **installable native packages** for the same two binaries (`kb` from
package `kb-user`, `kb-hardened` from package `kb-hardened`) and attaches them
to the same GitHub Release:

| Requested format | Platform | Produced by | Installer |
|------------------|----------|-------------|-----------|
| `.deb`           | Debian/Ubuntu | `nfpm` | `dpkg -i` / `apt install ./…deb` |
| `.pkg.tar.zst`   | Arch Linux | `nfpm` | `pacman -U` |
| `.msi`           | Windows | `cargo-wix` (WiX v3) | double-click / `msiexec` |
| `.pkg`           | macOS | `pkgbuild` | `installer -pkg` |
| `.AppImage`      | any Linux ("the weird one") | `appimagetool` | `chmod +x` and run; no install |

> Naming caveat: `.pkg.tar.zst` is an **Arch Linux** package (a tar stream with
> a `.PKGINFO` and `.MTREE`). `.pkg` is the **macOS** flat installer package.
> They are unrelated formats that happen to share the `.pkg` stem, so the
> upload globs must list them separately (`*.pkg.tar.zst` vs `*.pkg`).
>
> `.AppImage` is the conventional spelling (capital `A`, capital `I`, no
> leading dot in the filename: `kb-x86_64-linux.AppImage`). Linux filesystems
> are case-sensitive, so the upload glob must match that exact case or the
> asset silently does not upload.

A second, non-negotiable requirement drives the package scripts: **the
OS-native uninstaller must remove the application-created data directory,
the database, and all database sidecars, but must never delete the user's
Markdown pages.** See [Uninstall: delete the DB, keep the pages](#uninstall-delete-the-db-keep-the-pages).
The five formats differ sharply in what an "uninstaller" can do; the sections
below make the per-format behaviour explicit rather than assuming all five
can honour it identically. AppImage has no install lifecycle at all, so its
"uninstall" is partly an explicit command (see the AppImage section).

Non-goals: publishing to distro repositories (APT/YUM/AUR/Homebrew taps),
auto-update channels, and a GUI installer. The packages are single-file
downloads from the Releases tab, like the current binaries.

## Current state

- `.github/workflows/release.yml` has a `gate` job and a `build` matrix of
  `ubuntu-latest` (static musl) and `windows-latest` (native MSVC).
- Each matrix job runs `dist/build-<os>.sh`, smoke-tests the staged binary,
  then uploads `dist/kb-*` via `softprops/action-gh-release@v2` (stable tag
  `v*` or a dated `unstable-<date>-<sha>` prerelease).
- `dist/build-linux.sh` stages self-contained musl binaries +
  `.tar.zst`/`.tar.gz` + per-file `.sha256` into `dist/`.
- `dist/build-windows.sh` stages `kb-*.exe` + `.sha256` into `dist/`.
- `dist/build-macos.sh` **only builds**; it does not stage, archive, or
  checksum, and macOS is **not in the CI matrix** today.
- README's Releases table (README.md:59) advertises macOS artifacts that CI
  does not yet produce. This design also closes that gap.

## Locked decisions

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | One package per **binary**, not one package containing both | `kb` and `kb-hardened` target different audiences (interactive vs agent/CI) and can coexist (different binary names, shared DB). A user can install just one. |
| D2 | Package the **already-staged release binaries** | Keeps a single build path; the exact bytes smoke-tested by CI are the bytes inside every package. Do not rebuild inside the packagers where avoidable. |
| D3 | Build each format on its **native OS** | MSI needs WiX + Windows Installer; `.pkg` needs Apple's `pkgbuild`; deb/arch want a Linux `dpkg`/`bsdtar` for verification. Matches the existing "build natively" design. |
| D4 | Linux packages contain the **static musl** binary, no `depends:` | It runs on any libc; declaring no runtime deps is truthful and avoids glibc-version pinning. |
| D5 | Attach packages to the **same Release as the binaries** | One download page per version; no extra release/repo plumbing. |
| D6 | Unstable (per-push) builds are **prereleases with the same assets** | Parity with today's nightly behaviour; package managers see a uniquely-versioned prerelease string. |
| D7 | Pin packager tool versions in CI | `nfpm`, `cargo-wix`, WiX are fetched at release time; a silent major bump must not change output. |
| D8 | Uninstall removes the **data dir, DB, and DB sidecars**, never Markdown pages | User requirement. The default data dir (`…/knowledgebase/`) contains only the DB and its sidecars; each Markdown root is a user-chosen directory elsewhere, registered *inside* the DB. Deleting the data dir therefore cannot touch pages — and we still refuse `rm -rf` (D10) as a second guard. |
| D9 | The **last-removed** package deletes the shared DB | `kb` and `kb-hardened` share one DB. Uninstalling one while the sibling remains must keep the DB (otherwise the sibling silently loses its index). |
| D10 | Delete **known files + `rmdir` only**, never `rm -rf` | If the data dir contains anything else (a page tree the user pointed there, a stray export), the directory is left in place. The uninstaller must be safe even when the layout is not what we assumed. |
| D11 | Uninstall is **not** a data-reset on upgrade | Package upgrades must pass through without touching data (`postrm` arg `upgrade`, MSI `UPGRADINGPRODUCTCODE`, pacman `pre_upgrade`/`post_upgrade`). Only a real remove/ purge deletes. |
| D12 | AppImage wraps the **same static musl binary**, no bundled libs | Keeps the AppImage tiny and identical in behaviour to the raw binary; the musl binary has no shared-lib dependencies to bundle. The AppImage is convenience, not a separate build. |
| D13 | AppImage is distributed as a **single file with no installer**, and gets an explicit `--uninstall` AppRun action | There is no package database to hook, so the only "uninstall" is deleting the file; the bundled action supplies the DB cleanup the requirement asks for. |
| D14 | AppImage desktop/icon metadata is **generated**, not hand-maintained twice | One `.desktop` template + one PNG per variant, rendered by `dist/package-appimage.sh`; avoids metadata drift between `kb` and `kb-hardened`. |

## Version model

The same commit must yield package versions that (a) sort correctly and
(b) satisfy each format's grammar. Compute three values once per job from the
ref and the workspace version (`Cargo.toml` `[workspace.package] version`):

```sh
# Stable: GITHUB_REF_TYPE=tag, GITHUB_REF_NAME=v1.2.3
# Unstable: GITHUB_REF_TYPE=branch, differs per commit
BASE_VERSION="1.2.3"                       # from `cargo metadata` / tag
if [ "${GITHUB_REF_TYPE}" = tag ]; then
  PRERELEASE=""                             # stable
else
  PRERELEASE="unstable.$(date -u +%Y%m%d).${GITHUB_SHA::8}"
fi
```

| Format | Version string | Release/revision | Example (unstable) |
|--------|----------------|------------------|--------------------|
| deb    | `${BASE}~${PRERELEASE}` (tilde sorts *before* the release) | `1` | `1.2.3~unstable.20261118.abc12345-1` |
| Arch   | `${BASE}.${PRERELEASE}` (dots only; Arch forbids `-` in `pkgver`) | `1` (pkgrel) | `1.2.3.unstable.20261118.abc12345-1` |
| MSI    | `${BASE}` (WiX requires 3 numeric fields, `major.minor.build`) | n/a | `1.2.3` |
| pkg    | `${BASE}` (or `${BASE}.${PRERELEASE}` after sanitising) | n/a | `1.2.3` |

Stable versions are already valid everywhere (`1.2.3`). For unstable MSI/pkg
builds the packaged version is the release base version; uniqueness lives in
the release tag/asset name, and CI should not expect MSI "upgrade" semantics
between two same-version nightly builds (see Risks R4).

Add `dist/release-version.sh` (sourced by all packaging steps) that prints
`BASE_VERSION`, `PRERELEASE`, `DEB_VERSION`, `ARCH_VERSION`, and a unique
`UNSTABLE_TAG`, so the four jobs and the final checksums job agree.

## Linux: `.deb` + `.pkg.tar.zst` via `nfpm`

`nfpm` (GoReleaser's packager) emits deb and archlinux packages from one YAML,
with `${VAR}` environment expansion. It needs no root, no `dpkg-deb`, no
`makepkg`, so it runs directly on `ubuntu-latest`.

### `dist/nfpm.yaml`

```yaml
name: ${PKG_NAME}
arch: ${GOARCH}
platform: linux
version: ${PKG_VERSION}
release: ${PKG_RELEASE}
section: utils
priority: optional
maintainer: Jakub <maintainer@example.invalid>
description: |
  ${PKG_DESCRIPTION}
homepage: https://github.com/CZython/knowledgebase
license: MIT
contents:
  - src: ${PKG_SRC}
    dst: /usr/bin/${PKG_NAME}
    file_info:
      mode: 0755
scripts:
  # Generated by package-linux.sh: the canonical cleanup body inlined into a
  # self-contained maintainer script (see "Shared script" below).
  postremove: dist/scripts/postremove.generated.sh
```

Notes:

- `arch` uses Go nomenclature: `amd64` for x86_64 (nfpm translates it to
  `amd64` for deb and `x86_64` for Arch automatically). An `arm64` Linux job
  would pass `GOARCH=arm64`.
- No `depends:`. The musl binary is static; `ldd` in the smoke test already
  proves it.
- No `postinstall` script: packages must not need root-only setup or create a
  database as root. The DB is created on first `kb` run by the invoking user
  (`~/.local/share/knowledgebase/kb.db`).
- Debian's `lintian` will report `statically-linked-binary`; that is expected
  and intentional. Do not "fix" it by switching to glibc linking.
- `scripts.postremove` is the **only** maintainer script. On Debian it is the
  `postrm`; nfpm maps it to Arch's `.INSTALL` `post_remove` (confirm the exact
  mapping for the pinned nfpm version; use an `archlinux:` override if the
  mapping differs). Upgrades pass `upgrade`/`post_upgrade` and must be a
  no-op (D11).
- Arch `pacman -R` also removes the package's `.INSTALL` after files are gone,
  so the last-one-out sibling check in the script (D9) uses `command -v`,
  not the package DB.

### `dist/package-linux.sh` (new)

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUT="${OUT:-$ROOT/dist}"
BASE="${BASE_VERSION:?set BASE_VERSION}"
PRE="${PRERELEASE:-}"
NFPM_CONFIG="$ROOT/dist/nfpm.yaml"

for entry in "kb:kb-user" "kb-hardened:kb-hardened"; do
  BIN="${entry%%:*}"; PKG_NAME="$BIN"
  SRC="$OUT/$BIN-x86_64-linux-musl"
  [ -f "$SRC" ] || { echo "missing $SRC (run dist/build-linux.sh first)" >&2; exit 1; }

  # deb: tilde-prerelease sorts before a release
  DEB_VER="$BASE"; [ -n "$PRE" ] && DEB_VER="$BASE~$PRE"
  BIN="$BIN" PKG_NAME="$PKG_NAME" PKG_SRC="$SRC" GOARCH=amd64 \
  PKG_VERSION="$DEB_VER" PKG_RELEASE=1 \
  PKG_DESCRIPTION="$(cargo metadata --no-deps --format-version 1 \
     | jq -r --arg p "$PKG_NAME" '.packages[] | select(.name==$p) | .description')" \
    nfpm package -f "$NFPM_CONFIG" -p deb -t "$OUT"

  # Arch: dots only in pkgver
  ARCH_VER="$BASE"; [ -n "$PRE" ] && ARCH_VER="$BASE.$PRE"
  BIN="$BIN" PKG_NAME="$PKG_NAME" PKG_SRC="$SRC" GOARCH=amd64 \
  PKG_VERSION="$ARCH_VER" PKG_RELEASE=1 \
  PKG_DESCRIPTION="..." \
    nfpm package -f "$NFPM_CONFIG" -p archlinux -t "$OUT"
done
```

Outputs (`$OUT`):

```
kb_1.2.3-1_amd64.deb
kb-hardened_1.2.3-1_amd64.deb
kb-1.2.3-1-x86_64.pkg.tar.zst
kb-hardened-1.2.3-1-x86_64.pkg.tar.zst
```

(`package-linux.sh` is the only place that knows the per-format version
mutation; the config stays format-agnostic.)

### Workflow addition (Linux job)

After the existing `Build + stage artifacts` and `Smoke test` steps:

```yaml
- name: Package (.deb, .pkg.tar.zst)
  if: matrix.os == 'ubuntu-latest'
  shell: bash
  run: |
    set -euo pipefail
    source ./dist/release-version.sh
    # Pin the tool; verify the checksum rather than trusting "latest".
    GOARCH=amd64 nfpm version   # (installed in the step below)
    ./dist/package-linux.sh
```

`nfpm` install step (pinned):

```yaml
- name: Install nfpm (pinned)
  if: matrix.os == 'ubuntu-latest'
  run: |
    NFPM_VERSION=2.46.3
    curl -sSfL "https://github.com/goreleaser/nfpm/releases/download/v${NFPM_VERSION}/nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz" \
      | tar -xz -C /usr/local/bin nfpm
    nfpm version
```

## Linux: `.AppImage` via `appimagetool`

AppImage is the "just give me one file that runs on anything" format — which
is exactly what the static musl binary already is. We still ship it because
users ask for it and it carries a desktop entry/icon, at essentially zero
extra size. It is **not** a second build (D12): the AppImage embeds the same
`dist/kb-x86_64-linux-musl` / `dist/kb-hardened-x86_64-linux-musl` bytes.

Two platform facts to state up front:

- The AppImage **runtime** (the ELF preamble, not our binary) needs FUSE 2 on
  many distributions. Ubuntu 22.04+/Debian 12+ no longer install `libfuse2`
  by default, so running the AppImage fails with
  `dlopen(): error loading libfuse.so.2` until the user installs `libfuse2`
  / `libfuse2t64` or runs it with `--appimage-extract-and-run`. The README
  must mention both. The static musl `.tar.zst` remains the zero-dependency
  path.
- An AppImage is a **single file with no installer, package database, or
  uninstall hook**. `rm kb-*.AppImage` is the uninstall. We therefore add an
  explicit `--uninstall` action to `AppRun` (D13) that reuses the shared
  cleanup body, and document deleting the file separately.

### AppDir layout (per variant)

```
kb.AppDir/
├── AppRun                       # shell wrapper (see below)
├── kb.desktop                   # Name/Exec/Icon metadata
├── kb.png                       # 256x256, committed under dist/appimage/
└── usr/bin/
    ├── kb                       # the static musl binary
    └── uninstall-data.sh        # shared cleanup body (see Uninstall)
```

`kb-hardened.AppDir` is identical except for the names. Source the icon from
`dist/appimage/kb.png` (one PNG reused by both variants; `Icon=kb` even for
`kb-hardened` is fine, or add a second PNG — D14).

### `AppRun`

```sh
#!/bin/sh
set -eu
APPDIR="${APPDIR:-$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)}"

case "${1:-}" in
  --uninstall)
    # D9: keep the shared DB if a sibling install remains.
    self="$(basename "$APPDIR")"; self="${self%.AppDir}"   # kb | kb-hardened
    if [ "$self" = kb ]; then other=kb-hardened; else other=kb; fi

    sibling=""
    # (a) A real package/raw install on PATH.
    if command -v "$other" >/dev/null 2>&1; then sibling="$other"; fi
    # (b) A sibling AppImage sitting next to this one (not on PATH).
    if [ -n "${APPIMAGE:-}" ]; then
      d="$(dirname "$APPIMAGE")"
      for f in "$d/$other.AppImage" "$d/$other"-*.AppImage; do
        if [ -f "$f" ] && [ "$f" != "$APPIMAGE" ]; then sibling="$f"; fi
      done
    fi

    exec "$APPDIR/usr/bin/uninstall-data.sh" \
      "${XDG_DATA_HOME:-$HOME/.local/share}" ${sibling:+"$sibling"}
    ;;
  --)
    shift
    ;;
esac

exec "$APPDIR/usr/bin/kb" "$@"
```

`--uninstall` is an AppImage-level action, not a `kb` subcommand; it is
intercepted before clap sees it. Run it as the user (never with `sudo`, or
`$HOME` is root's). For `kb-hardened.AppImage`, `exec` the hardened binary at
the end.

### `dist/package-appimage.sh` (new)

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"
OUT="${OUT:-$ROOT/dist}"
TOOL="${APPIMAGETOOL:?set APPIMAGETOOL to the pinned appimagetool binary}"
SUFFIX="x86_64-linux"

for entry in "kb:kb-user" "kb-hardened:kb-hardened"; do
  BIN="${entry%%:*}"
  APPDIR="$(mktemp -d)/$BIN.AppDir"
  mkdir -p "$APPDIR/usr/bin"
  install -m 0755 "$OUT/$BIN-x86_64-linux-musl" "$APPDIR/usr/bin/$BIN"
  install -m 0755 dist/scripts/uninstall-data.sh "$APPDIR/usr/bin/uninstall-data.sh"
  install -m 0644 dist/appimage/kb.png "$APPDIR/$BIN.png"

  cat > "$APPDIR/$BIN.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$BIN
Comment=Markdown knowledge base over Turso + FTS
Exec=$BIN
Icon=$BIN
Terminal=true
Categories=Utility;
EOF

  # AppRun wrapper: --uninstall action + exec the variant binary.
  sed "s/usr\\/bin\\/kb\"/usr\\/bin\\/$BIN\"/" dist/appimage/AppRun > "$APPDIR/AppRun"
  chmod 0755 "$APPDIR/AppRun"

  # --appimage-extract-and-run: do not require FUSE in CI.
  ARCH=x86_64 "$TOOL" --appimage-extract-and-run --no-appstream \
    "$APPDIR" "$OUT/$BIN-$SUFFIX.AppImage"
done
```

Outputs:

```
dist/kb-x86_64-linux.AppImage
dist/kb-hardened-x86_64-linux.AppImage
```

### Workflow addition (Linux job)

Extend the existing packaging step rather than adding a second rebuild:

```yaml
- name: Install appimagetool (pinned by digest)
  if: matrix.os == 'ubuntu-latest'
  run: |
    set -euo pipefail
    # appimagetool publishes a rolling `continuous` tag; pin by SHA256.
    curl -sSfL -o /tmp/appimagetool \
      https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    echo "<pinned-sha256>  /tmp/appimagetool" | sha256sum -c -
    chmod +x /tmp/appimagetool

- name: Package (.deb, .pkg.tar.zst, .AppImage)
  if: matrix.os == 'ubuntu-latest'
  shell: bash
  run: |
    set -euo pipefail
    source ./dist/release-version.sh
    ./dist/package-linux.sh
    APPIMAGETOOL=/tmp/appimagetool ./dist/package-appimage.sh
```

`appimagetool` is itself an AppImage; `--appimage-extract-and-run` above means
the CI runner does not need FUSE. `ARCH=x86_64` is required when running it
outside its own AppImage context.

## Windows: `.msi` via `cargo-wix`

`cargo-wix` wraps WiX Toolset v3. Each workspace package gets its own WiX
source tree (`kb-user/wix/main.wxs`, `kb-hardened/wix/main.wxs`) so the two
MSIs can differ in `ProductName`/`UpgradeCode`/binary.

### One-time authoring (commit the generated files)

```powershell
cargo install cargo-wix --locked --version 0.3.9
cd kb-user;     cargo wix init    # writes kb-user/wix/main.wxs + metadata
cd ../kb-hardened; cargo wix init
```

Edit both `main.wxs` files:

- `Package/@UpgradeCode` — a fixed GUID per binary, never reused across
  binaries; stable across releases so MSI performs upgrades.
- Install scope: **per-user** (recommended, see R5). The binary installs to
  e.g. `%LOCALAPPDATA%\Programs\knowledgebase\`, while the DB stays at
  `%LOCALAPPDATA%\knowledgebase\`. Keeping program files separate from data
  makes the uninstall rule unambiguous and lets a non-elevated uninstaller
  delete the user's own DB without an admin prompt. Lock this before shipping;
  changing scope later breaks upgrades (MSI does not major-upgrade across
  per-user/per-machine contexts).
- `Package/@Platform="x64"`.
- Add the install dir to the **user** `PATH` via an `Environment` element.

### MSI uninstall: remove the DB, keep the pages

The data dir is deliberately *not* the install dir. Add a component that owns
the data directory and removes only the known files, guarded against upgrades
and against the sibling product still being installed:

```xml
<Directory Id="LocalAppDataFolder">
  <Directory Id="KBDataDir" Name="knowledgebase">
    <Directory Id="KBDataDirPlaceholder" />
  </Directory>
</Directory>

<DirectoryRef Id="KBDataDir">
  <Component Id="KBDataCleanup" Guid="*">
    <!-- KeyPath: a registry marker so the component is installable at all. -->
    <RegistryValue Root="HKCU" Key="Software\CZython\kb-data"
                   Name="present" Type="integer" Value="1" KeyPath="yes" />
    <RemoveFile Id="RmKbDb"     Name="kb.db"         On="uninstall" />
    <RemoveFile Id="RmKbDbWal"  Name="kb.db-wal"     On="uninstall" />
    <RemoveFile Id="RmKbDbShm"  Name="kb.db-shm"     On="uninstall" />
    <RemoveFile Id="RmKbDbJrnl" Name="kb.db-journal" On="uninstall" />
    <RemoveFile Id="RmKbDbTshm" Name="kb.db-tshm"    On="uninstall" />
    <RemoveFolder Id="RmKBDataDir" On="uninstall" />
  </Component>
</DirectoryRef>
```

`RemoveFolder` removes the directory only if it is empty after the five
`RemoveFile`s — the MSI equivalent of `rmdir`, so an unexpected file (e.g. a
page tree) keeps the folder (D10).

Two conditions the generated `main.wxs` must express (cargo-wix's default does
not):

- **No cleanup on upgrade.** `RemoveFile` inside a component uninstalled by a
  major upgrade would fire for the outgoing product. Add a condition so the
  component/cleanup only runs on a real uninstall, e.g. guard the component
  with `NOT UPGRADINGPRODUCTCODE` (or move cleanup into a deferred custom
  action whose condition includes `NOT UPGRADINGPRODUCTCODE`).
- **Last-one-out (D9).** Uninstalling `kb` must not delete the DB while
  `kb-hardened` is installed. Use a registry search for the sibling's install
  marker (each MSI writes `HKCU\Software\CZython\<name>` on install) and
  condition the cleanup on `NOT SIBLING_INSTALLED`. A deferred custom action
  is the straightforward place to combine both conditions.

The two MSIs **must not** both own the data directory as a normally-installed
component (that would make MSI repair/remove behaviour depend on which product
is being serviced); reference it only for `RemoveFile`/`RemoveFolder`.

### Workflow addition (Windows job)

```yaml
- name: Install WiX + cargo-wix
  if: matrix.os == 'windows-latest'
  shell: pwsh
  run: |
    choco install wixtoolset --version=3.14.1 --no-progress -y
    cargo install cargo-wix --locked --version 0.3.9

- name: Build MSIs
  if: matrix.os == 'windows-latest'
  shell: pwsh
  run: |
    cargo wix --nocapture -p kb-user
    cargo wix --nocapture -p kb-hardened
    Copy-Item target\wix\kb-*.msi        dist\
    Copy-Item target\wix\kb-hardened-*.msi dist\
```

Outputs:

```
dist/kb-1.2.3-x86_64.msi
dist/kb-hardened-1.2.3-x86_64.msi
```

`cargo-wix` runs its own `cargo build --release`; on the `windows-latest`
runner that is the same MSVC target `dist/build-windows.sh` just built, so it
is incremental and cheap. Alternatively add a `--no-build` path later if the
extra invocation proves slow.

### MSI smoke test (install + uninstall)

Run on the Windows runner after the MSI is produced (do not rely only on the
raw `.exe` smoke test):

```powershell
$msi = Get-ChildItem dist\kb-1.2.3-x86_64.msi
Start-Process msiexec.exe -Wait -ArgumentList "/i `"$($msi.FullName)`" /qn /norestart"
# Resolve the install dir from the WiX/source, then:
& "$installDir\kb.exe" --version

# Create the default DB, and a Markdown page *outside* the data dir.
$data = Join-Path $env:LOCALAPPDATA "knowledgebase"
$pages = Join-Path $env:TEMP "kb-pages"
New-Item -ItemType Directory -Force $pages | Out-Null
'dummy' | Set-Content (Join-Path $pages "keep.md")
& "$installDir\kb.exe" add $pages            # creates $data\kb.db (+ WAL sidecars)

Start-Process msiexec.exe -Wait -ArgumentList "/x `"$($msi.FullName)`" /qn /norestart"

# Uninstall must delete DB + sidecars + (now-empty) data dir, keep pages.
if (Test-Path $data) { throw "data dir survived uninstall: $data" }
if (-not (Test-Path (Join-Path $pages "keep.md"))) { throw "markdown page was deleted" }
```

If per-machine, the step needs `shell: pwsh` on the admin-capable GHA runner.
(With the recommended per-user scope it does not.)

## macOS: `.pkg` via `pkgbuild` + CI matrix entry

macOS is missing from the matrix today. Add two entries (native arch each):

```yaml
- os: macos-14          # Apple Silicon
  name: aarch64-apple-darwin
  script: ./dist/build-macos.sh
- os: macos-13          # Intel
  name: x86_64-apple-darwin
  script: ./dist/build-macos.sh
```

First, extend `dist/build-macos.sh` to stage into `dist/` like the Linux
script (copy the two binaries to `dist/kb-<target>` / `dist/kb-hardened-<target>`,
add `.tar.zst` + `.sha256`). Then add `dist/package-macos.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"
OUT="${OUT:-$ROOT/dist}"
VERSION="${BASE_VERSION:?}"
SUFFIX="${SUFFIX:?}"   # aarch64-apple-darwin | x86_64-apple-darwin

for BIN in kb kb-hardened; do
  SRC="$OUT/$BIN-$SUFFIX"
  [ -f "$SRC" ] || { echo "missing $SRC" >&2; exit 1; }
  STAGE="$(mktemp -d)"
  mkdir -p "$STAGE/usr/local/bin" "$STAGE/usr/local/share/kb"
  install -m 0755 "$SRC" "$STAGE/usr/local/bin/$BIN"
  # Bundled uninstaller (macOS has no uninstall hook); both payloads carry it.
  install -m 0755 dist/scripts/kb-uninstall "$STAGE/usr/local/bin/kb-uninstall"
  install -m 0755 dist/scripts/uninstall-data.sh \
                   "$STAGE/usr/local/share/kb/uninstall-data.sh"
  pkgbuild \
    --root "$STAGE" \
    --identifier "com.czython.knowledgebase.$BIN" \
    --version "$VERSION" \
    --install-location / \
    "$OUT/$BIN-$SUFFIX.pkg"
  pkgutil --payload-files "$OUT/$BIN-$SUFFIX.pkg"
  rm -rf "$STAGE"
done
```

Outputs:

```
dist/kb-aarch64-apple-darwin.pkg
dist/kb-hardened-aarch64-apple-darwin.pkg
dist/kb-x86_64-apple-darwin.pkg
dist/kb-hardened-x86_64-apple-darwin.pkg
```

Install-location is `/` because the payload already contains
`usr/local/bin/<bin>`; this avoids a stray `usr/local/usr/local/bin`.

### pkg uninstall: a hard platform limitation

**Flat macOS packages have no uninstaller and no uninstall hook.** `pkgbuild`
provides `preinstall`/`postinstall` only; dragging the receipt away or running
`pkgutil --forget` removes *metadata* but never files. There is no OS-native
"uninstall" action that our package can register. Therefore the requirement
cannot be met by a "default uninstaller" on macOS the way it can on
Windows/deb/Arch. The design must choose one of:

1. **Bundle an uninstaller (recommended).** Install
   `/usr/local/bin/kb-uninstall` in the payload (a small shell script that
   removes the binaries, the DB + sidecars, and the now-empty data dir, after
   the D9 sibling check), and document it in the README. This is the closest
   equivalent to a native uninstaller and works for both `kb` and
   `kb-hardened`. Optionally also publish a `kb-uninstall.pkg` whose
   `preinstall` runs the same script, for users who expect a package-shaped
   uninstaller.
2. **Ship a `.dmg` with a clickable uninstall script.** Same script, more
   packaging work; not justified for a CLI.
3. **Accept manual cleanup.** Rejected — it contradicts the stated
   requirement.

The shared uninstaller logic lives in `dist/scripts/uninstall-data.sh` (see
below) so the macOS script and the Linux maintainer script cannot drift.

### pkg smoke test

```bash
sudo installer -pkg dist/kb-aarch64-apple-darwin.pkg -target /
/usr/local/bin/kb --version

# Create the default DB and a Markdown tree outside the data dir.
PAGES="$(mktemp -d)"; echo 'dummy' > "$PAGES/keep.md"
/usr/local/bin/kb add "$PAGES"

# Uninstall via the bundled helper; must remove DB + sidecars + data dir,
# must leave the Markdown page.
/usr/local/bin/kb-uninstall
[ ! -e "$HOME/Library/Application Support/knowledgebase" ] || exit 1
[ -f "$PAGES/keep.md" ] || exit 1
```

Unsigned `.pkg` files are subject to Gatekeeper quarantine
(`com.apple.quarantine`); see Signing below.

## Workflow shape (conceptual diff)

`release.yml` keeps its current structure. Per job:

```
gate → build(matrix: ubuntu, windows, macos-14, macos-13)
         ├── build + stage (dist/build-<os>.sh)
         ├── smoke-test raw binary
         ├── package    (OS-specific steps above)
         ├── verify package (dpkg-deb/bsdtar, msiexec, installer, AppImage run)
         ├── verify uninstall (DB gone, page fixture intact)
         └── upload assets → same Release
       checksums (needs: build) → SHA256SUMS → Release
```

### Upload globs

Replace the two current globs in both upload steps
(`.github/workflows/release.yml`, stable and unstable) with an expanded list.
Keep the raw-binary glob first so existing consumers are unaffected:

```yaml
files: |
  dist/kb-${{ matrix.name }}*
  dist/kb-hardened-${{ matrix.name }}*
  dist/*.deb
  dist/*.pkg.tar.zst
  dist/*.pkg
  dist/*.msi
  dist/*.AppImage
```

The AppImage glob is capitalised exactly (`*.AppImage`); a `*.appimage` glob
matches nothing on a case-sensitive filesystem.

`matrix.name` is `x86_64-linux-musl` / `x86_64-pc-windows` /
`aarch64-apple-darwin` / `x86_64-apple-darwin`, so the raw globs do not
accidentally match a package. Each job uploads only what it built; uploading
the same tag from several jobs is append-only and already the case today.

### Checksums job (new, after all builds)

One canonical `SHA256SUMS` asset is easier for users than dozens of
`.sha256` files. A final job downloads all assets and re-emits aggregates:

```yaml
checksums:
  needs: [ build ]
  runs-on: ubuntu-latest
  permissions: { contents: write }
  steps:
    - uses: actions/checkout@v4
    - name: Work out the release tag
      id: tag
      shell: bash
      run: |
        set -euo pipefail
        if [ "${GITHUB_REF_TYPE}" = tag ]; then
          echo "value=${GITHUB_REF_NAME}" >> "$GITHUB_OUTPUT"
        else
          echo "value=unstable-$(date -u +%Y%m%d)-${GITHUB_SHA::8}" >> "$GITHUB_OUTPUT"
        fi
    - name: Aggregate SHA256SUMS
      env:
        GH_TOKEN: ${{ github.token }}
      run: |
        set -euo pipefail
        mkdir -p artifacts
        gh release download "${{ steps.tag.outputs.value }}" -D artifacts --clobber
        ( cd artifacts && sha256sum * | grep -v '^.*  SHA256SUMS$' > SHA256SUMS )
        gh release upload "${{ steps.tag.outputs.value }}" artifacts/SHA256SUMS --clobber
```

Keep the existing per-format `.sha256` files too; `SHA256SUMS` is additive.

## Verification matrix

Every package must be **installed and executed**, not merely produced.

| Format | Static checks | Install test | Run test | Uninstall test |
|--------|---------------|--------------|----------|----------------|
| `.deb` | `dpkg-deb -I`, `dpkg-deb -c`, `dpkg-deb --contents` shows `/usr/bin/kb` mode 0755 | `docker run --rm -v $PWD/dist:/d debian:stable dpkg -i /d/kb_*.deb` | `docker run … kb --version` | create DB, `dpkg -r kb`, assert data dir gone + page fixture intact |
| `.pkg.tar.zst` | `bsdtar -tf`, `.PKGINFO` present, mode 0755 | `docker run --rm -v $PWD/dist:/d archlinux:base pacman -U --noconfirm /d/kb-*.pkg.tar.zst` | `docker run … kb --version` | create DB, `pacman -R kb`, assert data dir gone + page intact |
| `.msi` | `msiexec /i /qn`, exit 0 | install then run staged `kb.exe --version` | `msiexec /x /qn` | create DB, uninstall, assert `%LOCALAPPDATA%\knowledgebase` gone + page intact |
| `.pkg` | `pkgutil --payload-files` lists `/usr/local/bin/kb` | `sudo installer -pkg … -target /` | `/usr/local/bin/kb --version` | `kb-uninstall`, assert `~/Library/Application Support/knowledgebase` gone + page intact |
| `.AppImage` | `file` says ELF, `./… --appimage-extract-and-run --version` works even without FUSE; embedded `.desktop`/icon extractable | `chmod +x`, run from a dir with no `kb` on PATH | `./kb-x86_64-linux.AppImage --version` | create DB, `./kb-*.AppImage --uninstall`, assert data dir gone + page intact; then `rm` the file |

The Linux install tests can live in the `ubuntu-latest` job (Docker is
preinstalled) as an extra step; macOS/Windows install tests run on their own
runner. Keep the existing raw-binary smoke test unchanged. For AppImage, test
both the FUSE path (if the runner has `libfuse2`) and the
`--appimage-extract-and-run` fallback, since the fallback is what CI and many
modern distros will use.

## Uninstall: delete the DB, keep the pages

This is the requirement that needs the most care, because "the uninstaller"
means something different on every platform and because deleting user data is
destructive.

### What is deleted

Only the files `kb` itself created under the **default** per-user data
directory, and only once no sibling binary remains (D9):

```
<data-local>/knowledgebase/kb.db
<data-local>/knowledgebase/kb.db-wal
<data-local>/knowledgebase/kb.db-shm
<data-local>/knowledgebase/kb.db-journal
<data-local>/knowledgebase/kb.db-tshm      # Turso's temp shm sidecar
```

`<data-local>` is `$XDG_DATA_HOME`/`~/.local/share` on Linux,
`%LOCALAPPDATA%` on Windows, and `~/Library/Application Support` on macOS
(`Db::default_db_path()`, kb-db/src/lib.rs:126). Then `rmdir` the
`knowledgebase/` directory **only if it is now empty** (D10). The user's
Markdown roots are never inside this directory by default and are never
enumerated — uninstall never reads the DB to find pages, so it cannot follow a
root pointer and delete a page tree.

Not deleted (explicitly out of scope): files under a user-supplied `--db`
path (the uninstaller cannot know them), pages anywhere else, and a `kb cron`
crontab entry (document removal separately, or add a future
`kb cron --uninstall`).

### Shared cleanup body (Linux + macOS)

`dist/scripts/uninstall-data.sh` is the single implementation. It is written
as a **function** so it can either be executed directly (macOS helper) or
inlined into a generated maintainer script (Linux). This matters: nfpm embeds
the *contents* of `scripts.postremove` into the package control archive; it
does not ship companion files, and on `dpkg -r` the package's own files are
removed **before** `postrm` runs. A maintainer script therefore cannot call a
helper from `/usr/share/kb/`.

```sh
#!/bin/sh
# Canonical cleanup body. Run directly (macOS) or inline the function (Linux
# maintainer script). Never removes Markdown pages: only the known DB files,
# then rmdir-if-empty.
kb_uninstall_data() {
  base="${1:?usage: kb_uninstall_data <data-local-parent> [sibling-binary]}"
  sibling="${2:-}"
  dir="$base/knowledgebase"

  # D9: a caller-detected sibling means the shared DB must survive.
  # `sibling` is either a binary name on PATH (Linux maintainer script) or a
  # filesystem path (AppImage, whose sibling is not on PATH).
  if [ -n "$sibling" ]; then
    if command -v "$sibling" >/dev/null 2>&1 || [ -e "$sibling" ]; then
      return 0
    fi
  fi

  for f in kb.db kb.db-wal kb.db-shm kb.db-journal kb.db-tshm; do
    rm -f "$dir/$f"
  done
  rmdir "$dir" 2>/dev/null || true   # D10: only if empty; unknown files win
}

# Direct invocation (macOS). Use `if`, not `cond &&`, so the inlined copy
# under `set -e` does not exit 1 when the guard is false.
if [ "${0##*/}" = "uninstall-data.sh" ]; then
  kb_uninstall_data "$@"
fi
```

### Linux: generated `postrm` / Arch `.INSTALL`

`dpkg` and `pacman` run maintainer scripts as **root**, so `$HOME` is root's,
not the invoking user's. Target the user who invoked `sudo`:

`dist/package-linux.sh` generates `dist/scripts/postremove.generated.sh` by
prepending the action guard and host logic to the inlined
`uninstall-data.sh` body (use `sed -n '/^kb_uninstall_data()/,/^}/p'`):

```sh
#!/bin/sh
# GENERATED — do not edit; source is dist/scripts/uninstall-data.sh.
set -eu
case "${1:-}" in
  upgrade|failed-upgrade|abort-upgrade|abort-install|disappear) exit 0 ;; # D11
esac
if [ -n "${SUDO_USER:-}" ]; then
  home="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
else
  home="${HOME:?}"
fi

# <inlined kb_uninstall_data() function from uninstall-data.sh>

# dpkg/pacman removed THIS package's files before postrm/post_remove runs, so
# any remaining kb/kb-hardened is the sibling (D9).
sibling=""
for t in kb kb-hardened; do
  if command -v "$t" >/dev/null 2>&1; then sibling="$t"; fi
done

kb_uninstall_data "${XDG_DATA_HOME:-$home/.local/share}" ${sibling:+"$sibling"}
```

Arch note: pacman's `.INSTALL` no-ops on upgrade through `pre_upgrade` /
`post_upgrade`; nfpm only wires `post_remove`, so no extra function is needed.
Verify the generated `.INSTALL` contains the body (`pacman -Qip`/`bsdtar -xOf`
the `.pkg.tar.zst` and read `.INSTALL`).

Limitations to document: if several users installed and used `kb`, only the
`sudo` caller's DB is removed (R9); a user who set `XDG_DATA_HOME` to a
non-default location is not cleaned. `apt remove` deleting user data is
unconventional (Debian convention is to keep it until `purge`); this design
honours the stated requirement and documents it, but if the project prefers
convention, gate cleanup on `"$1" = purge` instead of `remove` — an explicit
open question (Q1).

### Windows: MSI cleanup

Implemented in WiX via `RemoveFile` + `RemoveFolder` and the sibling/upgrade
conditions described in the MSI section above. Per-user scope means no
elevation and `%LOCALAPPDATA%` belongs to the uninstalling user. `RemoveFile`
is exact-name (no globbing), so all five sidecar names are listed explicitly.
MSI's `RemoveFolder` only removes an empty directory, satisfying D10.

### macOS: bundled `kb-uninstall`

Because macOS flat packages have no uninstall hook, the payload installs
`/usr/local/bin/kb-uninstall`:

```sh
#!/bin/sh
# /usr/local/bin/kb-uninstall [kb|kb-hardened]
# Run as the target user, NOT via `sudo`: $HOME decides whose DB is removed.
# The helper elevates only for /usr/local/bin and the receipt (`pkgutil`).
set -eu
id="com.czython.knowledgebase"
target="${1:-kb}"
[ "$target" = kb ] && other_pkg="$id.kb-hardened" || other_pkg="$id.kb"

# D9: use receipts, not `command -v` — both binaries are still on disk here.
# A lingering sibling receipt keeps the shared DB (safe: leftover, not loss).
if pkgutil --pkg-info "$other_pkg" >/dev/null 2>&1; then
  echo "the other kb variant is still installed; keeping the shared DB"
  exit 0
fi

data="$HOME/Library/Application Support"
/usr/local/share/kb/uninstall-data.sh "$data"

sudo rm -f "/usr/local/bin/$target"
sudo pkgutil --forget "$id.$target" 2>/dev/null || true

# Remove the shared helper only when no kb package remains at all.
if ! pkgutil --pkg-info "$id.kb" >/dev/null 2>&1 \
   && ! pkgutil --pkg-info "$id.kb-hardened" >/dev/null 2>&1; then
  sudo rm -f /usr/local/bin/kb-uninstall /usr/local/share/kb/uninstall-data.sh
fi
```

(The final script should live entirely in the payload and not shell out via
`sudo`; the snippet only shows the removal set. Decide Q2: ship the optional
`kb-uninstall.pkg` wrapper as well.)

### Linux AppImage: `--uninstall`, then delete the file

An AppImage is not installed, so there is nothing for an OS uninstaller to
remove. The closest equivalent is:

```sh
./kb-x86_64-linux.AppImage --uninstall   # DB + sidecars + empty data dir only
rm ./kb-x86_64-linux.AppImage            # "uninstall" the app file itself
```

`AppRun` intercepts `--uninstall` before `kb` sees it and calls the same
`uninstall-data.sh` body shipped inside the image. The D9 sibling check covers
both forms a sibling can take: a binary on `PATH`, and another AppImage **in
the same directory** (located via the `$APPIMAGE` path the runtime exports).
The AppImage's own `kb` lives inside the mounted image, not on `PATH`, so it
does not count as a sibling. Both variants' files can therefore coexist, and
the DB is deleted only when the last one is uninstalled.

The AppImage file itself is never removed by `--uninstall` (a running
AppImage cannot reliably delete its own backing file); the README says to
`rm` it afterwards.

### Required uninstall tests

Every format's install test must create the DB (`kb add <fixture>`) and a
Markdown fixture **outside** the data dir, then uninstall and assert:

1. `kb.db` and all four sidecars are gone;
2. the `knowledgebase/` directory is gone (when nothing else was placed in it);
3. the Markdown fixture still exists;
4. reinstalling works and creates a fresh DB.

Add a negative test too: put an unrelated file (e.g. `pages/keep.md`) inside
the `knowledgebase/` directory and assert uninstall leaves **both** the file
and the directory (D10). Add a shared-DB test: install both `kb` and
`kb-hardened`, uninstall one, assert the DB survives (D9).

## Signing and notarization (Phase 4, needs secrets)

Unsigned packages install, but each OS surfaces friction. Design the hooks
now, gate on repository secrets later:

- **Debian/Arch**: `nfpm` can sign the deb (`deb.signature.key_file`) and
  emit detached PGP signatures for Arch. Requires a maintainer GPG key in
  CI (`GPG_PRIVATE_KEY` + passphrase). Optional but expected by some users.
- **MSI**: Authenticode via `signtool sign /fd SHA256 /tr <timestamp> /td SHA256`
  with an OV/EV cert or Azure Trusted Signing. Without it, SmartScreen warns.
- **macOS `.pkg`**: `productsign --sign "Developer ID Installer: …"`,
  then `xcrun notarytool submit --wait`, then `xcrun stapler staple`.
  Without a Developer ID, Gatekeeper blocks the download by default.

Add a `packages` job permission guard so signing steps are skipped when
secrets are absent (fork PRs, local runs), never failing the release.

## Reproducibility

- Export `SOURCE_DATE_EPOCH="$(git log -1 --format=%ct)"` before packaging;
  `nfpm` honours it for internal mtimes. `pkgbuild`/`cargo-wix` are only
  partially deterministic — record their outputs' hashes but do not promise
  bit-identical rebuilds.
- Pin `nfpm`, `cargo-wix`, and WiX versions (D7) and verify downloaded
  tarballs (`sha256sum -c`) rather than piping `latest` to `tar`.
  `appimagetool` is pinned by SHA256 (R15).
- Keep `Cargo.lock` committed (already true) so the packaged binaries match.
- AppImage output is a squashfs image and is not bit-reproducible across
  `appimagetool` runs even with `SOURCE_DATE_EPOCH`; publish the SHA256 and
  treat each rebuild as a distinct artifact (as with the raw binaries today).

## Risks / open questions

- **R1 — Static musl inside `.deb`.** `lintian` flags
  `statically-linked-binary`; acceptable. Do not introduce a dependency on
  system glibc just to silence lint.
- **R2 — `.pkg` vs `.pkg.tar.zst` globs.** Separate globs are used; a bare
  `dist/*.pkg*` would sweep both together. Do not use one.
- **R3 — `macos-latest` arch drift.** GitHub's `macos-latest` moves between
  Intel and ARM images. Pin explicit labels (`macos-13` Intel,
  `macos-14`/`15` ARM) so `name`/`SUFFIX` always match the built arch.
- **R4 — MSI version monotonicity.** WiX versions are `major.minor.build`;
  two nightly MSIs at the same `1.2.3` are the same product version and MSI
  will refuse/repair rather than upgrade. Acceptable for prerelease
  binaries; if nightly MSI upgrades matter, encode the date/build into the
  third field (e.g. `1.2.<yyddd>`) and document it.
- **R5 — Per-user vs per-machine MSI.** Lock it before the first public MSI;
  switching later breaks upgrades. Recommendation: **per-user install +
  user PATH**, so the uninstaller can remove the user's DB without elevation.
  Install the binary to `%LOCALAPPDATA%\Programs\knowledgebase\`, not to the
  data dir `%LOCALAPPDATA%\knowledgebase\`, so program files and user data can
  be removed independently (and the existing `dist/build-windows.ps1 -Install`
  directory is data-only going forward).
- **R6 — Arch package naming.** `nfpm` output is
  `<name>-<version>-<pkgrel>-x86_64.pkg.tar.zst`; the package `name` is `kb`,
  not `kb-x86_64-linux-musl`. Users installing the raw binary and the Arch
  package get the same `/usr/bin/kb`; document that they must pick one.
- **R7 — Parallel uploads to one tag.** Four jobs append assets concurrently.
  `softprops/action-gh-release` tolerates this (today two jobs already do).
  The checksums job must `needs: build` so it runs only after all uploads.
- **R8 — macOS binary suffix.** `build-macos.sh` currently derives the target
  from `uname -m`; the CI job must pass/verify `SUFFIX` matches the matrix
  `name` so a runner-image change cannot mislabel a package.
- **R9 — Root-context Linux uninstall.** `postrm` runs as root and cannot know
  which user(s) used `kb`. The design cleans `$SUDO_USER`'s default data dir
  only; other users and `XDG_DATA_HOME` overrides are left alone (documented,
  not silently lost). Multi-user machines need a manual cleanup note.
- **R10 — macOS has no native uninstaller.** Flat `.pkg` packages cannot run
  code on removal. The requirement is met by a bundled `kb-uninstall` (and an
  optional `kb-uninstall.pkg` wrapper), not by the OS "default uninstaller".
  This must be an explicit, documented exception (Q2).
- **R11 — Custom `--db` paths are not cleaned.** Only the default
  `<data-local>/knowledgebase/` is known to the uninstaller. A user who ran
  `kb --db /some/where/kb.db` keeps that file; README must say so.
- **R12 — Sibling detection is heuristic.** The "last one out" check uses
  `command -v kb`/`kb-hardened` (Linux/macOS) or a registry marker (MSI). A
  user who installed the other binary manually can make the heuristic think a
  sibling exists (DB kept) — the safe failure mode; or point the other binary
  at the same DB and the opposite case cannot be detected. Never delete more
  than the known files, so the worst case is leftover data, not lost pages.
- **R13 — AppImage runtime needs FUSE 2.** Ubuntu 22.04+/Debian 12+ often lack
  `libfuse2`; the AppImage then fails with `error loading libfuse.so.2`.
  Document `libfuse2`/`libfuse2t64` and the `--appimage-extract-and-run`
  fallback. The static musl `.tar.zst` remains the no-FUSE path; this is why
  the existing `build-linux.sh` comment says "no FUSE (unlike AppImage)".
- **R14 — AppImage has no install database.** `--uninstall` is an explicit
  action; the sibling-AppImage check depends on the runtime exporting
  `$APPIMAGE`. If an older runtime does not, the check falls back to `PATH`
  only and may delete the DB while a sibling AppImage file remains (pages are
  untouched and the index is rebuildable, so the failure is recoverable).
  Document "delete both AppImages, then run `--uninstall` on the last one".
- **R15 — `appimagetool` has no stable semver.** Its release is the rolling
  `continuous` tag and the tool is itself an AppImage. Pin the downloaded
  binary by SHA256 (D7), not by URL; run it with `--appimage-extract-and-run`
  in CI so FUSE is not a build dependency.
- **R16 — desktop/icon validation.** `appimagetool` requires a square PNG
  matching the `.desktop` `Icon=` name and a valid `Exec=`. The generated
  metadata (D14) must be validated in CI (`desktop-file-validate` if
  available); a missing icon makes `appimagetool` fail or produces an
  iconless launcher.
- **Q1 — Delete on `apt remove` or only `apt purge`?** The requirement says
  the default uninstaller deletes; Debian convention says user data survives
  `remove` and is deleted on `purge`. Recommend honouring the requirement
  (delete on `remove`) but calling it out prominently in the README.
- **Q2 — macOS uninstaller shape.** Ship only `kb-uninstall` in the payload,
  or also publish a separate `kb-uninstall.pkg` whose `preinstall` runs it?
  Recommend both: the binary-adjacent script for normal users, the wrapper for
  users who expect a package-shaped uninstaller.

## Phased rollout

1. **Infra (no new assets yet):** add `dist/release-version.sh` and the shared
   `dist/scripts/uninstall-data.sh` + `dist/scripts/postremove.sh` +
   `dist/scripts/kb-uninstall`; make
   `dist/build-macos.sh` stage/archive/checksum like Linux; add
   `dist/nfpm.yaml` + `dist/package-linux.sh`. Verify locally with
   `nfpm package` for both formats, including `dpkg-deb -e` to confirm the
   `postrm` maintainer script is embedded.
2. **Linux packages:** install pinned `nfpm` in CI, run `package-linux.sh`,
   add `.deb`/`.pkg.tar.zst` to upload globs, install/run/uninstall-test in
   `debian:stable`/`archlinux:base` containers (assert DB gone, page intact,
   sibling-installed case keeps the DB). In the same phase add
   `dist/appimage/AppRun` + `dist/appimage/kb.png`, `package-appimage.sh`,
   the digest-pinned `appimagetool`, the `*.AppImage` glob, and the
   run/`--uninstall` test (both FUSE and `--appimage-extract-and-run`).
3. **macOS packages:** add `macos-13`/`macos-14` matrix entries, run
   `package-macos.sh`, install `kb-uninstall` in the payload, upload `.pkg`,
   install/run/uninstall-test on the runner.
4. **Windows MSI:** commit `kb-user/wix` + `kb-hardened/wix` with the
   data-cleanup component (`RemoveFile`/`RemoveFolder`, upgrade + sibling
   conditions), install WiX + `cargo-wix` in CI, build both MSIs, upload,
   install/run/uninstall test.
5. **Checksums + signing:** add the `checksums` job; wire optional
   `productsign`/`notarytool`, `signtool`, and `nfpm` PGP signing behind
   secret-presence guards.
6. **Docs:** update README.md "Prebuilt binaries (Releases)" with a package
   table and install examples (`apt install ./kb_*.deb`,
   `pacman -U kb-*.pkg.tar.zst`, `msiexec /i`, `installer -pkg`,
   `chmod +x kb-*.AppImage && ./kb-*.AppImage`), plus an **Uninstall**
   subsection stating exactly what is deleted (DB + sidecars + empty data
   dir), that Markdown pages, custom `--db` files, and other users' data are
   preserved, and the two no-native-uninstaller exceptions: macOS
   `kb-uninstall`, and AppImage `--uninstall` followed by `rm` (with the
   `libfuse2`/`--appimage-extract-and-run` note). Note that raw binaries
   remain available.

Each phase is independently shippable; phases 2–4 only add assets and never
change the existing raw-binary behavior.
