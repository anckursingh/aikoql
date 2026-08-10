#!/usr/bin/env node
// Mnemosyne — download + run the platform binary from GitHub Releases.
const { execSync, spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

const BIN_DIR = path.join(__dirname, 'bin');
const exe = process.platform === 'win32' ? 'mnemosyne.exe' : 'mnemosyne';
const binPath = path.join(BIN_DIR, exe);

const PLATFORM_MAP = {
  'win32-x64':  'mnemosyne-mcp.exe',
  'linux-x64':  'mnemosyne-mcp-linux',
  'darwin-x64': 'mnemosyne-mcp-macos',
  'darwin-arm64': 'mnemosyne-mcp-macos-arm64',
};

function download() {
  const platform = `${process.platform}-${process.arch}`;
  const file = PLATFORM_MAP[platform];
  if (!file) {
    console.error(`mnemosyne: no prebuilt binary for ${platform}. Build from source.`);
    process.exit(1);
  }

  const url = `https://github.com/anckursingh/mnemosyne/releases/latest/download/${file}`;
  console.error(`mnemosyne: downloading ${file}...`);

  fs.mkdirSync(BIN_DIR, { recursive: true });

  // Download to temp file first for atomicity
  const tmpPath = binPath + '.tmp';
  try {
    if (fs.existsSync(tmpPath)) fs.unlinkSync(tmpPath);
    if (process.platform === 'win32') {
      execSync(`powershell -c "$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -Uri '${url}' -OutFile '${tmpPath}'"`, { stdio: 'inherit' });
    } else {
      execSync(`curl -fsSL '${url}' -o '${tmpPath}' && chmod +x '${tmpPath}'`, { stdio: 'inherit' });
    }
  } catch (e) {
    try { fs.unlinkSync(tmpPath); } catch (_) {}
    console.error('mnemosyne: download failed. Build from source: cargo install mnemosyne-mcp');
    process.exit(1);
  }

  // Verify download isn't empty/corrupt (binary is ~20MB)
  try {
    const stat = fs.statSync(tmpPath);
    if (stat.size < 1000000) {
      throw new Error(`Downloaded file too small (${stat.size} bytes), likely corrupted`);
    }
  } catch (e) {
    try { fs.unlinkSync(tmpPath); } catch (_) {}
    console.error(`mnemosyne: ${e.message}. Build from source: cargo install mnemosyne-mcp`);
    process.exit(1);
  }

  fs.renameSync(tmpPath, binPath);
  if (process.platform !== 'win32') {
    try { fs.chmodSync(binPath, 0o755); } catch (_) {}
  }
}

if (!fs.existsSync(binPath)) {
  download();
}

if (!fs.existsSync(binPath)) {
  console.error('mnemosyne: binary not found after download.');
  process.exit(1);
}

// Forward all args to the binary via long-lived spawn (MCP needs persistent stdio)
const args = process.argv.slice(2);
const child = spawn(binPath, args, { stdio: 'inherit' });

child.on('exit', (code, signal) => {
  if (signal) {
    process.exit(128 + (signal === 'SIGTERM' ? 15 : 9));
  }
  process.exit(code || 0);
});

// Forward parent signals to child
['SIGTERM', 'SIGINT', 'SIGHUP'].forEach(sig => {
  process.on(sig, () => child.kill(sig));
});
