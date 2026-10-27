@ECHO OFF
REM build-windows.bat — release build of `kb` (package `kb-user`) and
REM `kb-hardened` (package `kb-hardened`) for Windows.
REM
REM Run from cmd.exe (no Git Bash needed). Two branches:
REM   * If the MSVC toolchain is present (the GHA windows-latest default, and
REM     the outcome of `rustup default stable-x86_64-pc-windows-msvc`) we build
REM     the native `-pc-windows-msvc` target.
REM   * Otherwise we build the `x86_64-pc-windows-gnu` cross target (needs the
REM     mingw-w64 gnu toolchain on PATH).
REM
REM On a stock Windows machine the easiest path is the MSVC one:
REM     rustup target add x86_64-pc-windows-msvc
REM     .\dist\build-windows.bat
REM
REM Output: dist\kb-x86_64-pc-windows.exe and dist\kb-hardened-x86_64-pc-windows.exe (each + .sha256)
REM
REM For a build *and* an install (copy to %LOCALAPPDATA%\knowledgebase\ and
REM create the default DB dir), use the PowerShell script instead:
REM     powershell -ExecutionPolicy Bypass -File .\dist\build-windows.ps1 -Install

setlocal EnableExtensions EnableDelayedExpansion
set "ROOT=%~dp0.."
pushd "%ROOT%" || goto :err

REM ---- pick a target --------------------------------------------------------
REM Prefer the MSVC target if its rust std is installed (matches the GHA
REM default); otherwise fall back to the gnu cross target.
for /f "delims=" %%t in ('rustup target list --installed 2^>nul') do (
  if "%%t"=="x86_64-pc-windows-msvc" (
    if not defined TARGET set "TARGET=x86_64-pc-windows-msvc"
  )
  if "%%t"=="x86_64-pc-windows-gnu" (
    if not defined TARGET set "TARGET=x86_64-pc-windows-gnu"
  )
)

if not defined TARGET (
  ECHO build-windows.bat: neither x86_64-pc-windows-msvc nor x86_64-pc-windows-gnu is installed.
  ECHO     rustup target add x86_64-pc-windows-msvc
  goto :err
)

ECHO ==&gt; cargo build --release --target %TARGET% -p kb-user
cargo build --release --target "%TARGET%" -p kb-user || goto :err
ECHO ==&gt; cargo build --release --target %TARGET% -p kb-hardened
cargo build --release --target "%TARGET%" -p kb-hardened || goto :err

set "OUT=%ROOT%\dist"
if not exist "%OUT%" mkdir "%OUT%"

REM ---- stage + checksum + verify: kb -----------------------------------------
if not exist "target\%TARGET%\release\kb.exe" (
  ECHO build-windows.bat: kb.exe not found in target\%TARGET%\release\ 1>&2
  goto :err
)
copy /Y "target\%TARGET%\release\kb.exe" "%OUT%\kb-x86_64-pc-windows.exe" || goto :err
where sha256sum >nul 2>nul && (
  pushd "%OUT%" && sha256sum kb-x86_64-pc-windows.exe > kb-x86_64-pc-windows.exe.sha256 && popd
)
"%OUT%\kb-x86_64-pc-windows.exe" --version || goto :err
ECHO ==&gt; staged %OUT%\kb-x86_64-pc-windows.exe

REM ---- stage + checksum + verify: kb-hardened --------------------------------
if not exist "target\%TARGET%\release\kb-hardened.exe" (
  ECHO build-windows.bat: kb-hardened.exe not found in target\%TARGET%\release\ 1>&2
  goto :err
)
copy /Y "target\%TARGET%\release\kb-hardened.exe" "%OUT%\kb-hardened-x86_64-pc-windows.exe" || goto :err
where sha256sum >nul 2>nul && (
  pushd "%OUT%" && sha256sum kb-hardened-x86_64-pc-windows.exe > kb-hardened-x86_64-pc-windows.exe.sha256 && popd
)
"%OUT%\kb-hardened-x86_64-pc-windows.exe" --version || goto :err
ECHO ==&gt; staged %OUT%\kb-hardened-x86_64-pc-windows.exe

ECHO.
ECHO ==&gt; done. Artifacts in: %OUT%
popd
exit /b 0

:err
popd
echo build-windows.bat: FAILED >&2
exit /b 1
