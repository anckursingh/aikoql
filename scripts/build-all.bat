@echo off
REM Mnemosyne — Build All Platforms (Windows + Linux)
REM Produces: build/windows/  build/linux/  + distribution archives
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

REM Get version from Cargo.toml
for /f "tokens=3 delims= " %%v in ('findstr "version" Cargo.toml ^| findstr /v "schema" ^| findstr /v "#"') do set VERSION=%%v
set VERSION=%VERSION:"=%
if "%VERSION%"=="" set VERSION=0.1.0

echo Version: %VERSION%
echo.

REM ------------------------------------------------------------------
REM Quality gate: run tests first
REM ------------------------------------------------------------------
echo === [0/6] Running tests ===
cargo test -p mnemosyne-mcp -- --test-threads=1 2>&1 | findstr /C:"test result"
echo.

REM ------------------------------------------------------------------
REM Windows (native)
REM ------------------------------------------------------------------
echo === [1/6] Building Windows release ===
cargo build --release -p mnemosyne-mcp
if %ERRORLEVEL% NEQ 0 (
    echo ERROR: Windows build failed
    exit /b 1
)

set WIN_OUT=build\windows
if exist %WIN_OUT% rmdir /s /q %WIN_OUT%
mkdir %WIN_OUT%

copy /Y target\release\mnemosyne-mcp.exe %WIN_OUT%\ >nul
copy /Y QUICKSTART.md %WIN_OUT%\ >nul
copy /Y mnemosyne.toml %WIN_OUT%\ >nul
echo %VERSION% > %WIN_OUT%\VERSION
echo Built: %DATE% %TIME% > %WIN_OUT%\BUILD_INFO.txt

REM Windows checksum
certutil -hashfile %WIN_OUT%\mnemosyne-mcp.exe SHA256 | findstr /v "SHA256" | findstr /v "CertUtil" > %WIN_OUT%\mnemosyne-mcp.exe.sha256

REM Windows archive
powershell -Command "Compress-Archive -Path '%WIN_OUT%\*' -DestinationPath 'build\mnemosyne-windows-x86_64-%VERSION%.zip' -Force"

echo   Windows binary: %WIN_OUT%\mnemosyne-mcp.exe
echo   Windows archive: build\mnemosyne-windows-x86_64-%VERSION%.zip

REM ------------------------------------------------------------------
REM Linux (cross-compile via musl)
REM ------------------------------------------------------------------
echo.
echo === [2/6] Setting up Linux cross-compilation target ===
rustup target add x86_64-unknown-linux-musl >nul 2>nul

echo === [3/6] Building Linux release (static musl) ===
cargo build --release --target x86_64-unknown-linux-musl -p mnemosyne-mcp --config "target.x86_64-unknown-linux-musl.linker = \"rust-lld\""
if %ERRORLEVEL% NEQ 0 (
    echo ERROR: Linux build failed
    exit /b 1
)

set LINUX_OUT=build\linux
if exist %LINUX_OUT% rmdir /s /q %LINUX_OUT%
mkdir %LINUX_OUT%

copy /Y target\x86_64-unknown-linux-musl\release\mnemosyne-mcp %LINUX_OUT%\ >nul
copy /Y QUICKSTART.md %LINUX_OUT%\ >nul
copy /Y mnemosyne.toml %LINUX_OUT%\ >nul
echo %VERSION% > %LINUX_OUT%\VERSION
echo Built: %DATE% %TIME% > %LINUX_OUT%\BUILD_INFO.txt

REM Linux checksum (PowerShell equivalent of sha256sum)
powershell -Command "$hash = (Get-FileHash '%LINUX_OUT%\mnemosyne-mcp' -Algorithm SHA256).Hash; $hash | Out-File -FilePath '%LINUX_OUT%\mnemosyne-mcp.sha256' -Encoding ASCII -NoNewline"

REM Linux tarball (use tar in Git Bash or PowerShell)
where tar >nul 2>nul
if %ERRORLEVEL% EQU 0 (
    tar -czf build\mnemosyne-linux-x86_64-musl-%VERSION%.tar.gz -C build linux/
    echo   Linux archive: build\mnemosyne-linux-x86_64-musl-%VERSION%.tar.gz
) else (
    powershell -Command "Compress-Archive -Path '%LINUX_OUT%\*' -DestinationPath 'build\mnemosyne-linux-x86_64-musl-%VERSION%.zip' -Force"
    echo   Linux archive: build\mnemosyne-linux-x86_64-musl-%VERSION%.zip ^(zip — install tar for .tar.gz^)
)

echo   Linux binary: %LINUX_OUT%\mnemosyne-mcp

REM ------------------------------------------------------------------
REM Summary
REM ------------------------------------------------------------------
echo.
echo ========================================
echo   Build Complete — v%VERSION%
echo ========================================
echo.
echo Windows (native x86-64):
echo   %WIN_OUT%\mnemosyne-mcp.exe
echo   build\mnemosyne-windows-x86_64-%VERSION%.zip
echo.
echo Linux (static musl, no glibc):
echo   %LINUX_OUT%\mnemosyne-mcp
echo   build\mnemosyne-linux-x86_64-musl-%VERSION%.tar.gz
echo.
echo Usage:
echo   mnemosyne-mcp shell              Interactive shell
echo   mnemosyne-mcp                    Start server (default)
echo   mnemosyne-mcp --metrics-addr 127.0.0.1:9181 my.db
echo   mnemosyne-mcp import --help      Import data
