// AG-62 gate — the one-command public trial (scripts/public-trial.sh)
// wires the verified Cloudflare-tunnel recipe: a cloudflared quick tunnel
// to the loopback serve surface, aikoql-mcp started under it with
// AIKOQL_PUBLIC_URL = the tunnel URL, the connector URL printed, and an
// admin password generated when none is set.
// Shims stand in for cloudflared/aikoql-mcp (a real tunnel needs a public
// endpoint and a login), so the assertions pin the WIRING, not the tunnel.
// Usage: node scripts/e2e-public-trial.js
const { spawn, spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

setTimeout(() => {
  console.error('e2e-public-trial: watchdog timeout (60s) — aborting');
  process.exit(1);
}, 60000).unref();

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aikoql-trial-'));
const db = path.join(dir, 'kb');
const url = 'https://fake123.trycloudflare.com';

function shim(name, body) {
  const p = path.join(dir, name);
  fs.writeFileSync(p, '#!/bin/sh\n' + body);
  fs.chmodSync(p, 0o755);
  return p;
}

// cloudflared: log its args, print the URL the way the real one does, hang
// until killed (the script must tear it down on exit).
shim(
  'cloudflared',
  [
    `echo "$@" > "${dir}/cloudflared.args"`,
    `echo "INF tunnel ready: ${url}" >&2`,
    'while true; do sleep 1; done',
  ].join('\n'),
);
// aikoql-mcp: record args + the env the script exported, then exit — the
// script should then clean up and exit 0.
shim(
  'aikoql-mcp',
  [
    `echo "$@" > "${dir}/serve.args"`,
    `echo "\${AIKOQL_PUBLIC_URL:-}" > "${dir}/public-url"`,
    `echo "\${AIKOQL_ADMIN_PASSWORD:-}" > "${dir}/admin-pw"`,
    'exit 0',
  ].join('\n'),
);

const env = {
  ...process.env,
  PATH: `${dir}:${process.env.PATH}`,
  // Empty admin password: the script must generate and print one.
  AIKOQL_ADMIN_PASSWORD: '',
};
const r = spawnSync('bash', [path.join(__dirname, 'public-trial.sh'), db], {
  encoding: 'utf8',
  env,
});
if (r.error) {
  console.error(`spawn failed: ${r.error.message}`);
  process.exit(1);
}
const out = `${r.stdout || ''}\n${r.stderr || ''}`;
const fail = (msg) => {
  console.error(`e2e-public-trial: ${msg}\n--- script output ---\n${out}`);
  process.exit(1);
};
if (r.status !== 0) fail(`public-trial.sh exited ${r.status}`);

const read = (f) => (fs.existsSync(f) ? fs.readFileSync(f, 'utf8').trim() : '');
if (read(`${dir}/cloudflared.args`) !== 'tunnel --url http://127.0.0.1:9091') {
  fail(`cloudflared got wrong args: ${read(`${dir}/cloudflared.args`)}`);
}
const serve = read(`${dir}/serve.args`);
if (serve !== `serve --metrics-addr 127.0.0.1:9091 ${db}`) {
  fail(`aikoql-mcp got wrong args: ${serve}`);
}
if (read(`${dir}/public-url`) !== url) {
  fail(`AIKOQL_PUBLIC_URL not the tunnel URL: ${read(`${dir}/public-url`)}`);
}
if (!out.includes(`connector URL: ${url}/mcp`)) {
  fail('the connector URL was not printed');
}
if (read(`${dir}/admin-pw`).length < 16) {
  fail('no admin password was generated when none was set');
}
// The tunnel process must not survive the script (it kills it on exit).
const linger = spawnSync('pgrep', ['-f', `cloudflared.*${dir}`], { encoding: 'utf8' });
if (linger.status === 0 && linger.stdout.trim()) {
  fail(`cloudflared shim outlived the script: ${linger.stdout.trim()}`);
}
fs.rmSync(dir, { recursive: true, force: true });
console.log('e2e-public-trial: ok');
