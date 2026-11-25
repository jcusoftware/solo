// Prepares the local E2E prerequisites for the Playwright webServer
// (scripts/e2e-setup.mjs, referenced by playwright.config.mjs).
//
// Order matters: the local D1 database must be migrated before `wrangler dev`
// starts serving /api/*, and a dev-only .dev.vars must exist or the form and
// newsletter routes fail with 503. The copy is guarded so an operator's real
// .dev.vars is never overwritten. This script never prints secrets.
import { execSync } from "node:child_process";
import { copyFileSync, existsSync } from "node:fs";

execSync("npm run db:migrate:local", { stdio: "inherit" });

if (existsSync(".dev.vars")) {
  console.log("e2e-setup: .dev.vars already exists; leaving it untouched");
} else {
  copyFileSync(".dev.vars.example", ".dev.vars");
  console.log("e2e-setup: copied .dev.vars.example -> .dev.vars");
}
