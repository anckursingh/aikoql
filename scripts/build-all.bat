@echo off
REM Mnemosyne — Build All Platforms
REM Produces: build/windows/  build/linux/
REM Requires: Rust toolchain (https://rustup.rs)

echo ========================================
echo   Mnemosyne Build — All Platforms
echo ========================================
echo.

REM Verify Rust is installed
where cargo >nul 2>nul
if %ERRORLEVEL% NEQ 0 (
    echo ERROR: Rust/Cargo not found. Install from https://rustup.rs
    exit /b 1
)

REM ------------------------------------------------------------------
REM Windows (native)
REM ------------------------------------------------------------------
echo.
echo === [1/5] Updating dependencies ===
cargo update

echo.
echo === [2/5] Building Windows release ===
cargo build --release -p mnemosyne-mcp
if %ERRORLEVEL% NEQ 0 (
    echo ERROR: Windows build failed
    exit /b 1
)

set WIN_OUT=build\windows
if not exist %WIN_OUT% mkdir %WIN_OUT%
copy /Y target\release\mnemosyne-mcp.exe %WIN_OUT%\ >nul
echo   Windows binary: %WIN_OUT%\mnemosyne-mcp.exe

REM ------------------------------------------------------------------
REM Linux (cross-compile via musl)
REM ------------------------------------------------------------------
echo.
echo === [3/5] Setting up Linux cross-compilation target ===
rustup target add x86_64-unknown-linux-musl >nul 2>nul

echo.
echo === [4/5] Building Linux release (static musl) ===
cargo build --release --target x86_64-unknown-linux-musl -p mnemosyne-mcp --config "target.x86_64-unknown-linux-musl.linker = \"rust-lld\""
if %ERRORLEVEL% NEQ 0 (
    echo ERROR: Linux build failed
    exit /b 1
)

set LINUX_OUT=build\linux
if not exist %LINUX_OUT% mkdir %LINUX_OUT%
copy /Y target\x86_64-unknown-linux-musl\release\mnemosyne-mcp %LINUX_OUT%\ >nul
echo   Linux binary: %LINUX_OUT%\mnemosyne-mcp

REM ------------------------------------------------------------------
REM Shared files
REM ------------------------------------------------------------------
echo.
echo === [5/5] Copying shared files ===
copy /Y QUICKSTART.md %WIN_OUT%\ >nul
copy /Y mnemosyne.toml %WIN_OUT%\ >nul
copy /Y QUICKSTART.md %LINUX_OUT%\ >nul
copy /Y mnemosyne.toml %LINUX_OUT%\ >nul

REM ------------------------------------------------------------------
REM Summary
REM ------------------------------------------------------------------
echo.
echo ========================================
echo   Build Complete
echo ========================================
echo.
echo Windows: build\windows\
echo   mnemosyne-mcp.exe  (Windows 10/11, x86-64)
echo   mnemosyne.toml     (config template)
echo   QUICKSTART.md      (getting started)
echo.
echo Linux:   build\linux\
echo   mnemosyne-mcp      (Linux x86-64, static musl — no glibc needed)
echo   mnemosyne.toml     (config template)
echo   QUICKSTART.md      (getting started)
echo.
echo Run (Windows): build\windows\mnemosyne-mcp.exe shell
echo Run (Linux):   build/linux/mnemosyne-mcp shell
