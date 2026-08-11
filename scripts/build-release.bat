@echo off
REM Aikoql Windows Release Build
REM Produces: build\windows\  (binary + checksum + archive)
REM Requires: Rust toolchain (https://rustup.rs)

echo === Aikoql Windows Release Build ===
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

echo [1/4] Running tests...
cargo test -p aikoql-mcp -- --test-threads=1 2>&1 | findstr /C:"test result"
if %ERRORLEVEL% NEQ 0 (
    echo WARNING: Tests had issues, continuing build...
)

echo [2/4] Building release binary...
cargo build --release -p aikoql-mcp
if %ERRORLEVEL% NEQ 0 (
    echo ERROR: Build failed
    exit /b 1
)

echo [3/4] Collecting artifacts...
set OUTDIR=build\windows
if exist %OUTDIR% rmdir /s /q %OUTDIR%
mkdir %OUTDIR%

copy target\release\aikoql-mcp.exe %OUTDIR%\
copy QUICKSTART.md %OUTDIR%\
copy aikoql.toml %OUTDIR%\

REM Version stamp
echo %VERSION% > %OUTDIR%\VERSION
echo Built: %DATE% %TIME% > %OUTDIR%\BUILD_INFO.txt

echo [4/4] Generating checksum and archive...
REM SHA256 checksum
certutil -hashfile %OUTDIR%\aikoql-mcp.exe SHA256 | findstr /v "SHA256" | findstr /v "CertUtil" > %OUTDIR%\aikoql-mcp.exe.sha256

REM Distribution zip
powershell -Command "Compress-Archive -Path '%OUTDIR%\*' -DestinationPath 'build\aikoql-windows-x86_64-%VERSION%.zip' -Force"

echo.
echo === Build complete ===
echo Version: %VERSION%
echo Binary: %OUTDIR%\aikoql-mcp.exe
echo Archive: build\aikoql-windows-x86_64-%VERSION%.zip
echo.
echo Usage:
echo   aikoql-mcp.exe shell               Interactive shell
echo   aikoql-mcp.exe                     Start server (MCP TCP + HTTP metrics)
echo   aikoql-mcp.exe --metrics-addr 127.0.0.1:9181 my.db
echo   aikoql-mcp.exe import --help       Import data
