#!/usr/bin/env node
// Mnemosyne — download + verify + run the platform binary from GitHub Releases.
const { execSync, spawn } = require('child_process');
const path = require('path');
const fs = require('fs');
const crypto = require('crypto');

const BIN_DIR = path.join(__dirname, 'bin');
const exe = process.platform === 'win32' ? 'mnemosyne.exe' : 'mnemosyne';
const binPath = path.join(BIN_DIR, exe);

const PLATFORM_MAP = {
  'win32-x64':  'mnemosyne-mcp.exe',
  'linux-x64':  'mnemosyne-mcp-linux',
  'darwin-x64': 'mnemosyne-mcp-macos',
  'darwin-arm64': 'mnemosyne-mcp-macos-arm64',
};

function fail(msg) {
  console.error(`mnemosyne: ${msg}\nBuild from source: cargo install mnemosyne-mcp`);
  process.exit(1);
}

function download() {
  const platform = `${process.platform}-${process.arch}`;
  const file = PLATFORM_MAP[platform];
  if (!file) {
    fail(`no prebuilt binary for ${platform}.`);
  }

  const base = `https://github.com/anckursingh/mnemosyne/releases/latest/download`;
  const binUrl = `${base}/${file}`;
  const chkUrl = `${base}/${file}.sha256`;

  console.error(`mnemosyne: downloading ${file}...`);

  fs.mkdirSync(BIN_DIR, { recursive: true });

  const tmpPath = binPath + '.tmp';
  const clean = () => { try { fs.unlinkSync(tmpPath); } catch (_) {} };

  // ── Download binary ──────────────────────────────────────────
  try {
    clean();
    if (process.platform === 'win32') {
      execSync(`powershell -c "$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -Uri '${binUrl}' -OutFile '${tmpPath}'"`, { stdio: 'inherit' });
    } else {
      execSync(`curl -fsSL '${binUrl}' -o '${tmpPath}' && chmod +x '${tmpPath}'`, { stdio: 'inherit' });
    }
  } catch (_) {
    clean();
    fail('download failed.');
  }

  // ── Size sanity check ────────────────────────────────────────
  try {
    const stat = fs.statSync(tmpPath);
    if (stat.size < 1_000_000) {
      throw new Error(`file too small (${stat.size} bytes), likely corrupted`);
    }
  } catch (e) {
    clean();
    fail(e.message);
  }

  // ── SHA-256 verification ─────────────────────────────────────
  console.error('mnemosyne: verifying checksum...');
  try {
    const checksumUrl = chkUrl;
    let expected;
    if (process.platform === 'win32') {
      expected = execSync(`powershell -c "$ProgressPreference='SilentlyContinue'; (Invoke-WebRequest -Uri '${checksumUrl}').Content.Trim()"`, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
    } else {
      expected = execSync(`curl -fsSL '${checksumUrl}'`, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
    }

    const actual = crypto.createHash('sha256').update(fs.readFileSync(tmpPath)).digest('hex');

    // The .sha256 file may be "<hash>  <filename>" or just "<hash>"
    const expectedHash = expected.split(/\s+/)[0].toLowerCase();
    if (expectedHash !== actual) {
      throw new Error(`checksum mismatch\n  expected: ${expectedHash}\n  actual:   ${actual}`);
    }
    console.error(`mnemosyne: checksum OK (${actual.substring(0, 12)}...)`);
  } catch (e) {
    clean();
    if (e.message.includes('checksum mismatch')) {
      fail(e.message);
    }
    fail(`checksum verification failed — unable to fetch or verify integrity.`);
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
  fail('binary not found after download.');
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
