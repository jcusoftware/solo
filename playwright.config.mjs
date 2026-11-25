// Playwright config for the SWS accessibility + header gates.
// Targets BASE_URL (default http://127.0.0.1:8787). When BASE_URL is not set,
// Playwright starts `wrangler dev` itself (or reuses one already running).
import { defineConfig, devices } from "@playwright/test";

const externalBase = process.env.BASE_URL;
const baseURL = externalBase ?? "http://127.0.0.1:8787";

export default defineConfig({
  testDir: "./tests",
  testMatch: /.*\.spec\.mjs$/,
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL,
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: externalBase
    ? undefined
    : {
        command: "npx wrangler dev --port 8787 --ip 127.0.0.1",
        url: `${baseURL}/api/health`,
        reuseExistingServer: !process.env.CI,
        timeout: 300_000,
      },
});
