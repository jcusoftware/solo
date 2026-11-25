// Playwright config for the SWS accessibility + header gates.
// Targets BASE_URL (default http://127.0.0.1:8787). When BASE_URL is not set,
// Playwright starts `wrangler dev` itself (or reuses one already running).
import { defineConfig, devices } from "@playwright/test";

const externalBase = process.env.BASE_URL;
const baseURL = externalBase ?? "http://127.0.0.1:8787";

export default defineConfig({
  testDir: "./tests",
  testMatch: /.*\.spec\.mjs$/,
  // The compiled-Worker checkout mock runs separately via `npm run test:checkout`.
  testIgnore: "checkout-worker.spec.mjs",
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
        // Prerequisites before wrangler dev: local D1 migrations + guarded
        // .dev.vars copy (scripts/e2e-setup.mjs), so a bare test run on a
        // fresh clone passes instead of failing forms with 503/500.
        // `--var DEV_MODE:true` mirrors the CI web job: test Turnstile keys
        // are only accepted in dev mode (the #6 fail-closed guard refuses
        // them otherwise), so local submissions exercise the same 201 path.
        command:
          "node scripts/e2e-setup.mjs && npx wrangler dev --port 8787 --ip 127.0.0.1 --var DEV_MODE:true",
        url: `${baseURL}/api/health`,
        reuseExistingServer: !process.env.CI,
        timeout: 300_000,
      },
});
