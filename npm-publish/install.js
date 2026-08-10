#!/usr/bin/env node
// Mnemosyne binary downloader — downloads the right binary for this platform from GitHub Releases.
const fs = require('fs');
const path = require('path');
const https = require('https');
const { execSync } = require('child_process');

const REPO = 'anckursingh/mnemosyne';
const VERSION = '0.1.0';
const BIN_DIR = path.join(__dirname, 'bin');
const PLATFORM_MAP = {
  'win32-x64':  { file: 'mnemosyne-windows.exe',     exe: 'mnemosyne.exe' },
  'linux-x64':  { file: 'mnemosyne-linux',            exe: 'mnemosyne' },
  'darwin-x64': { file: 'mnemosyne-macos',            exe: 'mnemosyne' },
  'darwin-arm64': { file: 'mnemosyne-macos-arm64',    exe: 'mnemosyne' },
};

const platform = `${process.platform}-${process.arch}`;
const target = PLATFORM_MAP[platform];

if (!target) {
  console.error(`mnemosyne: unsupported platform ${platform}. Build from source: https://github.com/${REPO}`);
  process.exit(1);
}

const binPath = path.join(BIN_DIR, target.exe);

if (fs.existsSync(binPath)) {
  process.exit(0); // already installed
}

console.log(`mnemosyne: downloading ${target.file} for ${platform}...`);

const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${target.file}`;

fs.mkdirSync(BIN_DIR, { recursive: true });

https.get(url, { followRedirects: true }, (res) => {
  if (res.statusCode === 302 || res.statusCode === 301) {
    https.get(res.headers.location, (r2) => download(r2), (err) => {
      console.error('mnemosyne: download failed:', err.message);
      console.error('Build from source: cargo install mnemosyne-mcp');
      process.exit(1);
    });
    return;
  }
  download(res);
}).on('error', (err) => {
  console.error('mnemosyne: download failed:', err.message);
  process.exit(1);
});

function download(res) {
  if (res.statusCode !== 200) {
    console.error(`mnemosyne: GitHub release not found (HTTP ${res.statusCode}). Build from source.`);
    process.exit(1);
  }
  const file = fs.createWriteStream(binPath);
  res.pipe(file);
  file.on('finish', () => {
    file.close();
    if (process.platform !== 'win32') {
      fs.chmodSync(binPath, 0o755);
    }
    console.log('mnemosyne: installed successfully');
  });
}
