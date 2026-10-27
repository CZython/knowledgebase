# build-windows.ps1 — release build of `kb` (package `kb-user`) *and*
# `kb-hardened` (package `kb-hardened`) for Windows.
#
# Usage (from PowerShell):
#   .\dist\build-windows.ps1                  # build both + stage to dist\
#   .\dist\build-windows.ps1 -Install         # build + install kb.exe + kb-hardened.exe
#                                             #                              to %LOCALAPPDATA%\knowledgebase\
#   .\dist\build-windows.ps1 -Target x86_64-pc-windows-gnu   # force a specific target
#   .\dist\build-windows.ps1 -Install -Out D:\kb             # build + stage+install into D:\kb
#
# The "-Install" step mirrors what a user would do by hand:
#   1. Copy kb.exe + kb-hardened.exe to %LOCALAPPDATA%\knowledgebase\
#   2. Create %LOCALAPPDATA%\knowledgebase\ (so the DB at kb.db has a directory)
#   3. (Optional, if %LOCALAPPDATA%\knowledgebase is not on PATH) append it to
#      the *user's* PATH via setx so subsequent `kb` / `kb-hardened` invocations work.
#
# Prereqs: a Rust toolchain (`rustup`), and the target std installed
#   (rustup target add x86_64-pc-windows-msvc, or x86_64-pc-windows-gnu with
#    a mingw-w64 gnu toolchain on PATH for the gnu target).

[CmdletBinding()]
param(
    [string]$Target = "",
    [string]$Out = "",
    [switch]$Install
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

# ---- pick a target ------------------------------------------------------------
if (-not $Target) {
    $installed = & rustup target list --installed 2>$null
    if ($installed -contains "x86_64-pc-windows-msvc") {
        $Target = "x86_64-pc-windows-msvc"
    } elseif ($installed -contains "x86_64-pc-windows-gnu") {
        $Target = "x86_64-pc-windows-gnu"
    } else {
        throw "Neither x86_64-pc-windows-msvc nor x86_64-pc-windows-gnu is installed. Run: rustup target add x86_64-pc-windows-msvc"
    }
}

# Build the expected output path with platform-correct separators
# (Join-Path, not a hardcoded backslash string — so this also works if the
# script is ever run under pwsh on a POSIX host with a cross-compiler).
$releaseRoot = Join-Path (Join-Path (Join-Path $Root "target") $Target) "release"

# How to smoke-run a .exe: natively on Windows; under wine when we're on a
# POSIX cross host (parity with build-windows.sh). "none" = just report size,
# so a plain build still exits 0.
$verifyMode = if ($IsWindows) { "native" }
              elseif (Get-Command wine -ErrorAction SilentlyContinue) { "wine" }
              else { "none" }

function Build-One([string]$Pkg, [string]$BinName, [string]$Tgt) {
    Write-Host "==> cargo build --release --target $Tgt -p $Pkg"
    & cargo build --release --target $Tgt -p $Pkg
    if ($LASTEXITCODE -ne 0) { throw "cargo build -p $Pkg failed ($LASTEXITCODE)" }

    $Src = Join-Path $releaseRoot "$BinName.exe"
    if (-not (Test-Path $Src)) { throw "expected build output $Src was not produced" }

    $Dest = Join-Path $OutDir "$BinName-x86_64-pc-windows.exe"
    Copy-Item -Force $Src $Dest
    Write-Host "==> staged $Dest"

    if ($verifyMode -eq "native") {
        & $Dest --version
        if ($LASTEXITCODE -ne 0) { throw "$Pkg --version failed" }
    } elseif ($verifyMode -eq "wine") {
        Write-Host "==> (POSIX host) smoke-running the staged .exe under wine"
        & wine $Dest --version
        if ($LASTEXITCODE -ne 0) { throw "$Pkg --version failed" }
    } else {
        Write-Host "==> wine not found; skipping smoke-run (the .exe will run natively on Windows)"
        Write-Host "    staged $($Dest) = $((Get-Item $Dest).Length) bytes"
    }
    return $Dest
}

$OutDir = if ($Out) { $Out } else { Join-Path $Root "dist" }
if (-not (Test-Path $OutDir)) { New-Item -ItemType Directory $OutDir | Out-Null }
$destUser     = Build-One "kb-user"     "kb"          $Target
$destHardened = Build-One "kb-hardened" "kb-hardened" $Target

# ---- optional install ---------------------------------------------------------
# Install both binaries side-by-side so `kb` and `kb-hardened` are both on
# the user's PATH from the same dir (they share one DB anyway).
if ($Install) {
    $InstallDir = Join-Path $env:LOCALAPPDATA "knowledgebase"
    if (-not (Test-Path $InstallDir)) { New-Item -ItemType Directory $InstallDir | Out-Null }
    foreach ($pair in @(@($destUser, "kb.exe"), @($destHardened, "kb-hardened.exe"))) {
        $Installed = Join-Path $InstallDir $pair[1]
        Copy-Item -Force $pair[0] $Installed
        Write-Host "==> installed $Installed"
    }

    # Ensure the dir is on the user's PATH (persist via the Win32 env API).
    # [Environment]::SetEnvironmentVariable (unlike the DOS `setx` command)
    # is not subject to the ~1024-character limit, so it won't silently
    # truncate an existing long PATH.
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not ($UserPath -split ";" -contains $InstallDir)) {
        $NewPath = if ($UserPath) { "$UserPath;$InstallDir" } else { $InstallDir }
        [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
        Write-Host "==> added $InstallDir to the user PATH (open a NEW shell to see it)"
    } else {
        Write-Host "==> $InstallDir already on the user PATH"
    }

    # One-line usage hint.
    Write-Host ""
    Write-Host "Default DB location: $([Environment]::ExpandEnvironmentVariables('%LOCALAPPDATA%'))\knowledgebase\kb.db"
}

Write-Host ""
Write-Host "==> done."
