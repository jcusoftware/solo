// WCAG 2.2 AA gate: axe-core + native-semantics checks that axe does not cover.
import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const ROUTES = (process.env.A11Y_ROUTES ?? "/").split(",").map((r) => r.trim()).filter(Boolean);
const WCAG_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
// Third-party "accessibility overlay" widgets are banned (they do not fix native semantics).
const OVERLAY_PATTERNS = [/accessibe/i, /acsbapp/i, /userway/i, /audioeye/i, /equalweb/i, /maxaccess/i, /purpose-?ui/i, /accessibilityassistant/i];

for (const route of ROUTES) {
  test.describe(`a11y ${route}`, () => {
    test.beforeEach(async ({ page }) => {
      // Turnstile is third-party; block it so tests are deterministic and offline-safe.
      await page.route("https://challenges.cloudflare.com/**", (r) => r.abort());
      const resp = await page.goto(route);
      expect(resp?.status(), "page should load").toBeLessThan(400);
    });

    test("axe: zero WCAG 2.2 AA violations", async ({ page }) => {
      const results = await new AxeBuilder({ page }).withTags(WCAG_TAGS).analyze();
      const summary = results.violations.map((v) => `${v.id} (${v.impact}): ${v.nodes.length} node(s) — ${v.help}`);
      expect(summary, summary.join("\n")).toEqual([]);
    });

    test("exactly one <h1> and no skipped heading levels", async ({ page }) => {
      const levels = await page.$$eval("h1,h2,h3,h4,h5,h6", (els) => els.map((e) => Number(e.tagName[1])));
      expect(levels.filter((l) => l === 1)).toHaveLength(1);
      expect(levels[0]).toBe(1);
      for (let i = 1; i < levels.length; i++) {
        expect(levels[i] - levels[i - 1], `heading jump h${levels[i - 1]} -> h${levels[i]}`).toBeLessThanOrEqual(1);
      }
    });

    test("landmarks, lang and skip link keyboard flow", async ({ page }) => {
      await expect(page.locator("html")).toHaveAttribute("lang", /^[a-z]{2}(-[A-Z]{2})?$/);
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
    });

    test("no accessibility overlay widgets", async ({ page }) => {
      const srcs = await page.$$eval("script[src], iframe[src], link[href]", (els) =>
        els.map((e) => e.getAttribute("src") ?? e.getAttribute("href") ?? ""),
      );
      const offenders = srcs.filter((s) => OVERLAY_PATTERNS.some((p) => p.test(s)));
      expect(offenders).toEqual([]);
    });

    test("every form control has an accessible label", async ({ page }) => {
      const unlabeled = await page.$$eval(
        "input:not([type=hidden]):not([type=submit]):not([type=button]), select, textarea",
        (els) =>
          els
            .filter((el) => {
              if (el.closest('[aria-hidden="true"]')) return false;
              const id = el.getAttribute("id");
              const hasFor = id && document.querySelector(`label[for="${CSS.escape(id)}"]`);
              return !(hasFor || el.closest("label") || el.getAttribute("aria-label") || el.getAttribute("aria-labelledby"));
            })
            .map((el) => el.outerHTML.slice(0, 120)),
      );
      expect(unlabeled).toEqual([]);
    });

    test("no PDF-only menus (menu content is HTML)", async ({ page }) => {
      const pdfLinks = await page.$$eval("a[href]", (els) =>
        els
          .map((e) => e.getAttribute("href") ?? "")
          .filter((href) => /\.pdf(\?|#|$)/i.test(href)),
      );
      const jsonld = await page.$$eval('script[type="application/ld+json"]', (els) => els.map((e) => e.textContent ?? ""));
      if (pdfLinks.length > 0) {
        expect(jsonld.join(" "), "PDF menus require an HTML/JSON-LD Menu equivalent").toMatch(/"Menu"/);
      }
    });

    test("newsletter consent checkbox is unchecked by default", async ({ page }) => {
      const checkboxes = page.locator('form[action="/api/newsletter"] input[type="checkbox"][name="consent"]');
      const count = await checkboxes.count();
      if (route === "/") {
        expect(count, "newsletter consent checkbox should exist on /").toBeGreaterThan(0);
      }
      for (let i = 0; i < count; i++) {
        await expect(checkboxes.nth(i)).not.toBeChecked();
      }
    });
  });
}
