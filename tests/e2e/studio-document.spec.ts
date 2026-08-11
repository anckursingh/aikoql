import { test, expect, type Page } from '@playwright/test';
import { spawn, type ChildProcess } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';

const SERVER_PORT = 9199;
const BASE_URL = `http://127.0.0.1:${SERVER_PORT}`;

const INVOICE_DIR = 'C:/Users/ancku/CascadeProjects/ai-crm-platform/services/billing-processor/output';
const INVOICE_PDF = path.join(INVOICE_DIR, 'invoice_9655.pdf');

function findBinary(): string {
  const candidates = [
    path.resolve(__dirname, '../../target/release/aikoql-mcp.exe'),
    path.resolve(__dirname, '../../target/debug/aikoql-mcp.exe'),
  ];
  for (const c of candidates) {
    if (fs.existsSync(c)) return c;
  }
  throw new Error('aikoql-mcp binary not found. Run cargo build --release first.');
}

let server: ChildProcess | null = null;
const DB_PATH = path.resolve(__dirname, '../../target/test-e2e.redb');

test.beforeAll(async () => {
  try { fs.unlinkSync(DB_PATH); } catch (_) {}
  try { fs.rmSync(DB_PATH + '.artifacts', { recursive: true, force: true }); } catch (_) {}

  const bin = findBinary();
  server = spawn(bin, [DB_PATH, '--metrics-addr', `127.0.0.1:${SERVER_PORT}`], {
    stdio: 'pipe',
    cwd: path.resolve(__dirname, '../..'),
  });

  for (let i = 0; i < 30; i++) {
    try {
      const res = await fetch(`${BASE_URL}/health`);
      if (res.ok) return;
    } catch (_) {}
    await new Promise(r => setTimeout(r, 1000));
  }
  throw new Error('Server did not start within 30s');
});

test.afterAll(() => {
  if (server) { server.kill(); server = null; }
  try { fs.unlinkSync(DB_PATH); } catch (_) {}
  try { fs.rmSync(DB_PATH + '.artifacts', { recursive: true, force: true }); } catch (_) {}
});

async function login(page: Page) {
  await page.goto(`${BASE_URL}/studio`);
  await page.fill('#login-user', 'admin');
  await page.fill('#login-pass', 'admin');
  await page.click('button:has-text("Sign In")');
  await page.waitForSelector('#sidebar', { timeout: 10000 });
}

test('document ingestion and compilation end-to-end', async ({ page }) => {
  // Collect console errors for debugging.
  const errors: string[] = [];
  page.on('console', msg => { if (msg.type() === 'error') errors.push(msg.text()); });
  page.on('pageerror', err => errors.push(err.message));

  await login(page);

  // Navigate to Document Explorer.
  await page.click('button[data-panel="documents"]');
  await page.waitForSelector('#panel-documents.active', { timeout: 5000 });
  await expect(page.locator('#panel-documents h3')).toContainText('Document Explorer');

  // Upload an invoice PDF.
  const fileInput = page.locator('#doc-file-input');
  await fileInput.setInputFiles(INVOICE_PDF);
  await page.click('button:has-text("Ingest")');

  // Wait for ingest result (or error).
  await page.waitForSelector('#doc-ingest-result', { timeout: 15000 });
  const ingestText = await page.locator('#doc-ingest-result').textContent();
  console.log('Ingest result:', ingestText);

  // Wait for document list to reload.
  await page.waitForSelector('.data-table button:has-text("Compile")', { timeout: 10000 });

  // Click Compile on the first document.
  await page.click('.data-table button:has-text("Compile")');

  // Wait for compilation result — any content.
  const resultEl = page.locator('#doc-compile-result');
  await resultEl.waitFor({ state: 'visible', timeout: 30000 });
  await page.waitForTimeout(1000); // let rendering settle

  const resultText = await resultEl.textContent();
  console.log('Compile result:', resultText?.substring(0, 500));
  if (errors.length > 0) console.error('JS errors:', errors);

  // Verify each pipeline section.
  if (resultText?.includes('Compile failed')) {
    throw new Error('Compile failed! Check server logs. Result: ' + resultText);
  }

  await expect(resultEl).toContainText('Phase Stats');
  await expect(resultEl).toContainText('Knowledge IR');
  await expect(resultEl).toContainText('Ontology Proposals');
  await expect(resultEl).toContainText('Entity Resolution');
  await expect(resultEl).toContainText('Commit Plan');
  await expect(resultEl).toContainText('Evidence Trail');
  await expect(resultEl).toContainText('Embedded Chunks');
});
