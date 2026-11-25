// Separate Playwright config for the compiled-Worker checkout mock: runs
// against Miniflare with a fully mocked Stripe/Turnstile upstream (no external
// traffic, no running dev server). Needs `npm run build` first.
// Mainline gates stay in playwright.config.mjs; this spec is excluded there.
import { defineConfig } from "@playwright/test";
import base from "./playwright.config.mjs";

export default defineConfig({
  ...base,
  testMatch: "checkout-worker.spec.mjs",
  testIgnore: [],
  webServer: undefined,
});
