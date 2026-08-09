import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  timeout: 120000,
  retries: 0,
  use: {
    baseURL: process.env.MNEMOSYNE_URL || 'http://127.0.0.1:9181',
    headless: true,
    viewport: { width: 1280, height: 900 },
  },
  // Start the MCP server before tests via globalSetup.
  globalSetup: undefined, // server is managed by the test itself
});
