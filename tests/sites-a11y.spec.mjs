// Playwright WCAG 2.2 AA + strict-CSP verification for sws-landing and sws-docs static sites.
import http from "node:http";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

import { parseHeadersFile } from "../core/tools/check-headers.mjs";
import { main as buildSites } from "../sites/build.mjs";
import { DOC_PAGES } from "../sites/src/docs.mjs";

const WCAG_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const OVERLAY_PATTERNS = [
  /accessibe/i,
  /acsbapp/i,
  /userway/i,
  /audioeye/i,
  /equalweb/i,
  /maxaccess/i,
  /purpose-?ui/i,
  /accessibilityassistant/i,
];

const MIME_TYPES = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".xml": "application/xml; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
};

async function startStaticServer(rootDir) {
  const absRoot = path.resolve(rootDir);
  const headersRaw = await readFile(path.join(absRoot, "_headers"), "utf8");
  const rootHeaders = parseHeadersFile(headersRaw, "/*");

  const server = http.createServer(async (req, res) => {
    const reqPath = new URL(req.url ?? "/", "http://127.0.0.1").pathname;
    let relPath = reqPath.endsWith("/") ? `${reqPath}index.html` : reqPath;
    if (relPath.startsWith("/")) relPath = relPath.slice(1);
    const fullPath = path.resolve(absRoot, relPath);

    for (const [name, value] of rootHeaders) {
      // Omit upgrade-insecure-requests on local http://127.0.0.1 test server so Chromium loads /styles.css over HTTP.
      const testValue =
        name.toLowerCase() === "content-security-policy"
          ? value.replace(/;\s*upgrade-insecure-requests/i, "")
          : value;
      res.setHeader(name, testValue);
    }

    if (!fullPath.startsWith(absRoot + path.sep) && fullPath !== absRoot) {
      res.writeHead(403, { "content-type": "text/plain; charset=utf-8" });
      res.end("Forbidden");
      return;
    }

    try {
      const body = await readFile(fullPath);
      const ext = path.extname(fullPath);
      res.writeHead(200, { "content-type": MIME_TYPES[ext] ?? "application/octet-stream" });
      res.end(body);
    } catch {
      const notFound = await readFile(path.join(absRoot, "404.html"));
      res.writeHead(404, { "content-type": "text/html; charset=utf-8" });
      res.end(notFound);
    }
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = server.address();
  return {
    baseUrl: `http://127.0.0.1:${addr.port}`,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}

async function assertPageAccessibility(page, url, expectedStatus = 200) {
  const consoleErrors = [];
  const onConsole = (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  };
  page.on("console", onConsole);

  try {
    const resp = await page.goto(url);
    expect(resp?.status(), `HTTP status for ${url}`).toBe(expectedStatus);

    const results = await new AxeBuilder({ page }).withTags(WCAG_TAGS).analyze();
    const summary = results.violations.map(
      (v) => `${v.id} (${v.impact}): ${v.nodes.length} node(s) — ${v.help}`,
    );
    expect(summary, summary.join("\n")).toEqual([]);

    const levels = await page.$$eval("h1,h2,h3,h4,h5,h6", (els) =>
      els.map((e) => Number(e.tagName[1])),
    );
    expect(levels.filter((l) => l === 1)).toHaveLength(1);
    expect(levels[0]).toBe(1);
    for (let i = 1; i < levels.length; i += 1) {
      expect(
        levels[i] - levels[i - 1],
        `heading jump h${levels[i - 1]} -> h${levels[i]}`,
      ).toBeLessThanOrEqual(1);
    }

    await expect(page.locator("html")).toHaveAttribute("lang", "en");
    await expect(page.locator("main#main")).toHaveCount(1);
    await expect(page.locator("header").first()).toBeAttached();
    await expect(page.locator("footer").first()).toBeAttached();

    const skip = page.locator('.skip-link[href="#main"]');
    await expect(skip).toBeAttached();
    await page.keyboard.press("Tab");
    await expect(skip).toBeFocused();
    await expect(skip).toBeVisible();
    const box = await skip.boundingBox();
    expect(box, "skip link should enter viewport on focus").not.toBeNull();
    expect(box.y).toBeGreaterThanOrEqual(0);
    expect(box.x).toBeGreaterThanOrEqual(0);
    await page.keyboard.press("Enter");
    await expect(page.locator("#main")).toBeFocused();

    // WCAG 2.1 SC 1.4.10 Reflow (no horizontal page overflow) + WCAG 2.2 SC 2.5.8 Target Size (>= 24x24px)
    const layoutMetrics = await page.evaluate(() => {
      const overflowPx = document.documentElement.scrollWidth - window.innerWidth;
      const smallTargets = [];
      for (const el of document.querySelectorAll("a, button")) {
        if (el.classList.contains("skip-link")) continue;
        const inProse =
          el.closest("p, li, td") &&
          !el.closest(".primary-nav, .footer-links, .docs-nav-list, .cta-row, .doc-pager") &&
          !el.classList.contains("download-link");
        if (inProse) continue;
        const rect = el.getBoundingClientRect();
        if (rect.width < 24 || rect.height < 24) {
          smallTargets.push(
            `${el.textContent?.trim().slice(0, 40)} (${Math.round(rect.width)}x${Math.round(rect.height)})`,
          );
        }
      }
      return { overflowPx, smallTargets };
    });
    expect(layoutMetrics.overflowPx, `Horizontal overflow on ${url}`).toBeLessThanOrEqual(0);
    expect(layoutMetrics.smallTargets, `Undersized interactive targets (<24x24) on ${url}`).toEqual([]);

    const srcs = await page.$$eval("script[src], iframe[src], link[href]", (els) =>
      els.map((e) => e.getAttribute("src") ?? e.getAttribute("href") ?? ""),
    );
    const offenders = srcs.filter((s) => OVERLAY_PATTERNS.some((p) => p.test(s)));
    expect(offenders).toEqual([]);

    const unexpectedErrors =
      expectedStatus === 404
        ? consoleErrors.filter((msg) => !/status of 404/i.test(msg))
        : consoleErrors;
    expect(unexpectedErrors, `Console/CSP errors on ${url}`).toEqual([]);
  } finally {
    page.off("console", onConsole);
  }
}

test.describe("static sites WCAG 2.2 AA + CSP", () => {
  let landingServer;
  let docsServer;

  test.beforeAll(async () => {
    landingServer = await startStaticServer("sites/landing/public");
    docsServer = await startStaticServer("sites/docs/public");
  });

  test.afterAll(async () => {
    if (landingServer) await landingServer.close();
    if (docsServer) await docsServer.close();
  });

  test("sws-landing / passes WCAG 2.2 AA and strict CSP", async ({ page }) => {
    await assertPageAccessibility(page, `${landingServer.baseUrl}/`, 200);
  });

  test("sws-landing 404 passes WCAG 2.2 AA and strict CSP", async ({ page }) => {
    await assertPageAccessibility(page, `${landingServer.baseUrl}/does-not-exist`, 404);
  });

  test("sws-landing v0.1.0 release (hasCli=true + showcaseUrl) passes WCAG 2.2 AA and strict CSP", async ({
    page,
  }) => {
    const releaseTmpDir = await mkdtemp(path.join(os.tmpdir(), "sws-sites-release-a11y-"));
    let releaseLandingServer;
    let releaseDocsServer;
    try {
      const outLanding = path.join(releaseTmpDir, "landing");
      const outDocs = path.join(releaseTmpDir, "docs");
      const rc = await buildSites(
        [
          "--version",
          "v0.1.0",
          "--has-cli",
          "true",
          "--repo",
          "solo-website-system/sws",
          "--showcase-url",
          "https://demo.jcusoftware.com",
          "--out-landing",
          outLanding,
          "--out-docs",
          outDocs,
        ],
        {},
      );
      expect(rc).toBe(0);
      releaseLandingServer = await startStaticServer(outLanding);
      releaseDocsServer = await startStaticServer(outDocs);
      await assertPageAccessibility(page, `${releaseLandingServer.baseUrl}/`, 200);
      await assertPageAccessibility(page, `${releaseDocsServer.baseUrl}/showcase/`, 200);
    } finally {
      if (releaseLandingServer) await releaseLandingServer.close();
      if (releaseDocsServer) await releaseDocsServer.close();
      await rm(releaseTmpDir, { recursive: true, force: true });
    }
  });

  for (const docPage of DOC_PAGES) {
    test(`sws-docs ${docPage.href} passes WCAG 2.2 AA and strict CSP`, async ({ page }) => {
      await assertPageAccessibility(page, `${docsServer.baseUrl}${docPage.href}`, 200);
    });
  }

  test("sws-docs 404 passes WCAG 2.2 AA and strict CSP", async ({ page }) => {
    await assertPageAccessibility(page, `${docsServer.baseUrl}/does-not-exist`, 404);
  });

  test("mobile viewports (375x812 and 320x568) pass WCAG 2.2 AA and reflow without overflow", async ({
    page,
  }) => {
    for (const vp of [
      { width: 375, height: 812 },
      { width: 320, height: 568 },
    ]) {
      await page.setViewportSize(vp);
      await assertPageAccessibility(page, `${landingServer.baseUrl}/`, 200);
      await assertPageAccessibility(page, `${docsServer.baseUrl}/`, 200);
      await assertPageAccessibility(page, `${docsServer.baseUrl}/getting-started/`, 200);
    }
  });
});
