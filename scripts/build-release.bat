@echo off
REM Mnemosyne Windows Release Build
REM Produces: target\release\mnemosyne-mcp.exe
REM Requires: Rust toolchain (https://rustup.rs)

echo === Mnemosyne Release Build ===
echo.

REM Verify Rust is installed
where cargo >nul 2>nul
if %ERRORLEVEL% NEQ 0 (
    echo ERROR: Rust/Cargo not found. Install from https://rustup.rs
    exit /b 1
)

echo [1/3] Updating dependencies...
cargo update

echo [2/3] Building release binary...
cargo build --release -p mnemosyne-mcp

if %ERRORLEVEL% NEQ 0 (
    echo ERROR: Build failed
    exit /b 1
)

echo [3/3] Collecting artifacts...
set OUTDIR=build\windows
if not exist %OUTDIR% mkdir %OUTDIR%

copy target\release\mnemosyne-mcp.exe %OUTDIR%\
copy QUICKSTART.md %OUTDIR%\
copy mnemosyne.toml %OUTDIR%\

echo.
echo === Build complete ===
echo Binary: %OUTDIR%\mnemosyne-mcp.exe
echo Run: %OUTDIR%\mnemosyne-mcp.exe
echo Or: %OUTDIR%\mnemosyne-mcp.exe --listen 127.0.0.1:9090 --metrics-addr 127.0.0.1:9091
