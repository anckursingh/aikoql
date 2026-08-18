#!/usr/bin/env node
// PRR-5 plugin validation (CI): plugin.json + marketplace.json parse, required
// fields present, mcpServers entries well-formed, and plugin/npm/Cargo versions
// aligned (drift caught pre-tag; the release gate then pins all three to the tag).
const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..');
const fail = (msg) => { console.error(`plugin validation FAILED: ${msg}`); process.exit(1); };
const readJson = (p) => { try { return JSON.parse(fs.readFileSync(p, 'utf8')); } catch (e) { fail(`${path.relative(root, p)}: ${e.message}`); } };

const plugin = readJson(path.join(root, '.claude-plugin', 'plugin.json'));
const market = readJson(path.join(root, '.claude-plugin', 'marketplace.json'));
const npm = readJson(path.join(root, 'npm-publish', 'package.json'));

for (const [file, obj] of [['plugin.json', plugin], ['marketplace.json', market]]) {
  if (typeof obj.name !== 'string' || obj.name.length === 0) fail(`${file} missing name`);
}
if (typeof plugin.version !== 'string' || plugin.version.length === 0) fail('plugin.json missing version');

for (const [name, server] of Object.entries(plugin.mcpServers || {})) {
  if (typeof server.command !== 'string' || server.command.length === 0) fail(`plugin.json mcpServers.${name} missing command`);
  if (!Array.isArray(server.args) || server.args.length === 0) fail(`plugin.json mcpServers.${name} missing args`);
}
if (Object.keys(plugin.mcpServers || {}).length === 0) fail('plugin.json declares no mcpServers');
if (!Array.isArray(market.plugins) || market.plugins.length === 0) fail('marketplace.json declares no plugins');
if (market.plugins[0].source !== './') fail('marketplace.json plugin source must be "./" (local dir)');

const cargoVer = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8').match(/^version = "([^"]+)"/m);
if (!cargoVer) fail('Cargo.toml version not found');
const versions = { plugin: plugin.version, npm: npm.version, cargo: cargoVer[1] };
const all = Object.entries(versions);
if (new Set(all.map(([, v]) => v)).size !== 1) {
  fail(`version drift: ${all.map(([k, v]) => `${k}=${v}`).join(' ')}`);
}

console.log(`plugin validation OK (versions aligned at ${versions.plugin}; ${Object.keys(plugin.mcpServers).length} mcpServer, ${market.plugins.length} marketplace plugin)`);
