// The image over TLS from outside the container: with AIKOQL_TLS_CERT /
// AIKOQL_TLS_KEY set (the test certificates, SAN IP:127.0.0.1), the published
// MCP port serves an encrypted session from the host — initialize with the
// token, remember, get — and, armed with AIKOQL_ALLOW_REMOTE_HTTP and a login,
// the published HTTP port serves /health over HTTPS. A plaintext client on the
// MCP port is never answered. Without TLS (the image default) nothing here can
// connect: the server binds loopback inside the container.
// Usage: node scripts/e2e-docker-tls.js [image]
const { execSync } = require('child_process');
const fs = require('fs');
const net = require('net');
const path = require('path');
const tls = require('tls');

const image = process.argv[2] || 'aikoql:test';
const NAME = 'aikoql-tls-test';
const FX = path.resolve(__dirname, '../crates/services/api/mcp/tests/fixtures/tls');
const ca = fs.readFileSync(path.join(FX, 'ca.pem'));

setTimeout(() => { console.error('e2e-docker-tls: watchdog timeout (120s) — aborting'); cleanup(); process.exit(1); }, 120000).unref();

function sh(cmd) { try { execSync(cmd, { stdio: 'ignore' }); } catch (_) { /* best-effort cleanup */ } }
function cleanup() { sh(`docker rm -f ${NAME}`); }
function fail(msg) {
  console.error('e2e-docker-tls: FAIL ' + msg);
  try { console.error(execSync(`docker logs ${NAME} 2>&1`, { encoding: 'utf8' }).slice(-3000)); } catch (_) { /* gone */ }
  cleanup(); process.exit(1);
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const port = (p) => execSync(`docker port ${NAME} ${p}/tcp`, { encoding: 'utf8' }).trim().split('\n')[0].split(':').pop();

function open(p) {
  return new Promise((res, rej) => {
    const s = tls.connect({ host: '127.0.0.1', port: p, ca, servername: undefined }, () => res(s));
    s.once('error', rej);
  });
}

async function connectRetry(p) {
  for (let i = 0; i < 60; i++) {
    try { return await open(p); } catch (_) { await sleep(500); }
  }
  fail(`no TLS connection to published port ${p}`);
}

function rpc(s) {
  let buf = ''; let next = 0; const pending = new Map();
  s.on('data', (c) => {
    buf += c.toString('utf8');
    let nl;
    while ((nl = buf.indexOf('\n')) !== -1) {
      const line = buf.slice(0, nl).trim(); buf = buf.slice(nl + 1);
      if (!line) continue;
      let m; try { m = JSON.parse(line); } catch (_) { continue; }
      if (pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
    }
  });
  return (method, params) => new Promise((res) => {
    const id = ++next; pending.set(id, res);
    s.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
  });
}

(async () => {
  cleanup();
  execSync([
    'docker run -d --name', NAME,
    '-p 127.0.0.1::9090 -p 127.0.0.1::9091',
    `-v ${FX}:/certs:ro`,
    '-e AIKOQL_TLS_CERT=/certs/server.pem -e AIKOQL_TLS_KEY=/certs/server.key',
    '-e AIKOQL_TCP_TOKEN=tls-e2e::admin',
    '-e AIKOQL_ALLOW_REMOTE_HTTP=1 -e AIKOQL_ADMIN_PASSWORD=tls-e2e-pw',
    image,
  ].join(' '), { stdio: 'ignore' });
  const [mcp, http] = [port(9090), port(9091)];

  // MCP over TLS through the published port
  const s = await connectRetry(Number(mcp));
  const call = rpc(s);
  const init = await call('initialize', { protocolVersion: '2024-11-05', capabilities: {}, token: 'tls-e2e', clientInfo: { name: 'e2e-docker-tls', version: '0' } });
  if (!init.result) fail(`initialize over TLS: ${JSON.stringify(init)}`);
  const tool = async (name, args) => JSON.parse((await call('tools/call', { name, arguments: args })).result.content[0].text);
  const put = await tool('remember', { subject: 'admin', type_name: 'note', properties: { body: 'tls from the host' } });
  if (!put.koid) fail(`remember over TLS: ${JSON.stringify(put)}`);
  const got = await tool('get', { subject: 'admin', koid: put.koid });
  if (got.properties.body !== 'tls from the host') fail(`get over TLS: ${JSON.stringify(got)}`);
  s.end();

  // HTTPS /health through the published port
  const health = await new Promise((res) => {
    const h = tls.connect({ host: '127.0.0.1', port: Number(http), ca }, () => {
      h.write('GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n');
    });
    let out = ''; h.on('data', (c) => { out += c; }); h.on('end', () => res(out)); h.on('error', (e) => res('ERR ' + e.message));
  });
  if (!/^HTTP\/1\.\d 200 /.test(health)) fail(`HTTPS /health: ${health.slice(0, 200)}`);

  // a plaintext client on the TLS port gets no JSON-RPC answer
  const plain = await new Promise((res) => {
    const p = net.connect(Number(mcp), '127.0.0.1', () => p.write(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { token: 'tls-e2e' } }) + '\n'));
    let out = ''; p.on('data', (c) => { out += c; }); p.on('close', () => res(out)); p.on('error', () => res(out));
    setTimeout(() => { p.destroy(); res(out); }, 3000);
  });
  if (plain.includes('"result"')) fail('a plaintext client was served on the TLS port');

  cleanup();
  console.log('e2e-docker-tls: PASS — MCP over TLS and HTTPS /health through published ports');
  process.exit(0);
})().catch((e) => fail(e.message));
