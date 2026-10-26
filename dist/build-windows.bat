@ECHO OFF
REM build-windows.bat — release build of `kb` (package `kb-user`) for Windows.
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
REM Output: dist\kb-x86_64-pc-windows.exe   (+ .sha256)
REM
REM For a build *and* an install (copy to %LOCALAPPDATA%\knowledgebase\ and
REM create the default DB dir), use the PowerShell script instead:
REM     powershell -ExecutionPolicy Bypass -File .\dist\build-windows.ps1 -Install

setlocal EnableExtensions EnableDelayedExpansion
set "ROOT=%~dp0.."
pushd "%ROOT%" || goto :err

set "BIN=kb"

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

ECHO ==&gt; cargo build --release --target %TARGET% -p %BIN%-user
cargo build --release --target "%TARGET%" -p kb-user || goto :err

REM ---- stage ----------------------------------------------------------------
set "OUT=%ROOT%\dist"
if not exist "%OUT%" mkdir "%OUT%"
set "DEST=%OUT%\kb-x86_64-pc-windows.exe"
copy /Y "target\%TARGET%\release\%BIN%.exe" "%DEST%" || goto :err

REM ---- checksum (if sha256sum is on PATH — git-bash provides it) -------------
where sha256sum >nul 2>nul && (
  pushd "%OUT%" && sha256sum kb-x86_64-pc-windows.exe > kb-x86_64-pc-windows.exe.sha256 && popd
)

REM ---- verify ----------------------------------------------------------------
ECHO ==&gt; verify:
"%DEST%" --version || goto :err

ECHO.
ECHO ==&gt; done. Artifact in: %OUT%
popd
exit /b 0

:err
popd
echo build-windows.bat: FAILED >&2
exit /b 1
