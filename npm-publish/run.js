#!/usr/bin/env node
// Mnemosyne — download + run the platform binary from GitHub Releases.
const { execSync } = require('child_process');
const path = require('path');
const fs = require('fs');

const BIN_DIR = path.join(__dirname, 'bin');
const exe = process.platform === 'win32' ? 'mnemosyne.exe' : 'mnemosyne';
const binPath = path.join(BIN_DIR, exe);

const PLATFORM_MAP = {
  'win32-x64':  'mnemosyne-windows.exe',
  'linux-x64':  'mnemosyne-linux',
  'darwin-x64': 'mnemosyne-macos',
  'darwin-arm64': 'mnemosyne-macos-arm64',
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

  try {
    // Use platform-native download
    if (process.platform === 'win32') {
      execSync(`powershell -c "Invoke-WebRequest -Uri '${url}' -OutFile '${binPath}'"`, { stdio: 'inherit' });
    } else {
      execSync(`curl -fsSL '${url}' -o '${binPath}' && chmod +x '${binPath}'`, { stdio: 'inherit' });
    }
  } catch (e) {
    console.error('mnemosyne: download failed. Build from source: cargo install mnemosyne-mcp');
    process.exit(1);
  }

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

// Forward all args to the binary
const args = process.argv.slice(2);
try {
  execSync(`"${binPath}" ${args.map(a => `"${a}"`).join(' ')}`, { stdio: 'inherit' });
} catch (e) {
  process.exit(e.status || 1);
}
