// Scheduling (issue #44) browser flow: the rendered `/book-a-call` page against
// the dev server (CI `web` job, same scoping as checkout.spec.mjs) with mocked
// slots/booking APIs and a local Turnstile stub — no external traffic.
// Renderer-level determinism (toggles, sitemap, budgets, nav, config markers)
// is covered by the pack-crate cargo tests; the Worker slot math is covered by
// host-testable cargo tests in crates/sws-worker-site.
import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WCAG_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const testSiteKey = "1x00000000000000000000AA";
// 2026-11-01 is the fall-back day in America/New_York: 05:00Z and 06:00Z are
// both 1:00 AM local (EDT then EST) — two distinct UTC instants per label.
const slots = [
  { start_utc: "2026-11-01T05:00:00.000Z", end_utc: "2026-11-01T05:30:00.000Z" },
  { start_utc: "2026-11-01T06:00:00.000Z", end_utc: "2026-11-01T06:30:00.000Z" },
];

async function mockBookingFlow(page, submit) {
  await page.clock.setFixedTime(new Date("2026-10-30T16:00:00Z"));
  await page.route("https://challenges.cloudflare.com/**", (route) => {
    const url = new URL(route.request().url());
    if (!url.pathname.endsWith("/api.js")) return route.abort();
    // Emulate the widget's contract, without reaching the live challenge service.
    return route.fulfill({ contentType: "application/javascript", body: `
      function testTurnstile() {
        for (const widget of document.querySelectorAll('.cf-turnstile[data-sitekey="${testSiteKey}"]')) {
          if (widget.querySelector('input')) continue;
          const token = document.createElement('input');
          token.type = 'hidden'; token.name = 'cf-turnstile-response'; token.value = 'mock-turnstile-token';
          widget.append(token);
        }
      }
      window.turnstile = { reset: testTurnstile };
      if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', testTurnstile);
      else testTurnstile();` });
  });
  await page.route("**/api/bookings/slots*", (route) =>
    route.fulfill({ json: { slots: new URL(route.request().url()).searchParams.get("date") === "2026-11-01" ? slots : [] } }),
  );
  if (submit) {
    await page.route("**/api/bookings", (route) => submit(route));
  }
}

async function assertAccessible(page) {
  const { violations } = await new AxeBuilder({ page }).withTags(WCAG_TAGS).analyze();
  expect(violations.map(({ id, nodes }) => `${id}: ${nodes.map((n) => n.target.join(" ")).join(", ")}`)).toEqual([]);
  await expect(page.getByRole("heading", { level: 1 })).toHaveCount(1);
  await expect(page.getByRole("main")).toHaveCount(1);
}

async function findTimes(page) {
  await page.goto("/book-a-call");
  await page.getByLabel("Date (required)").fill("2026-11-01");
  await page.getByRole("button", { name: "Find times" }).click();
  await expect(page.getByRole("group", { name: "Available times (required)" })).toBeVisible();
}

test("booking page validates, supports keyboard slots, and confirms invitation", async ({ page }) => {
  const submissions = [];
  await mockBookingFlow(page, async (route) => {
    submissions.push(Object.fromEntries(new URLSearchParams(route.request().postData())));
    await route.fulfill({ status: 201, json: { status: "confirmed", ...slots[1], calendar_invitation: true } });
  });
  await findTimes(page);

  // Submitting without required fields fails inline with a focused summary.
  await page.getByRole("button", { name: "Book this time" }).click();
  await expect(page.locator("#booking-errors")).toBeFocused();
  await expect(page.getByLabel("Name (required)")).toHaveAttribute("aria-invalid", "true");
  await expect(page.getByText("Choose an available time.", { exact: true }).first()).toBeVisible();
  await assertAccessible(page);

  await page.getByLabel("Name (required)").fill("Sample Guest");
  await page.getByLabel("Email (required)").fill("guest@example.test");
  const early = page.getByRole("radio", { name: "1:00 AM EDT" });
  const late = page.getByRole("radio", { name: "1:00 AM EST" });
  await early.focus();
  await early.press("Space");
  await early.press("ArrowDown");
  await expect(late).toBeChecked();
  await page.getByRole("button", { name: "Book this time" }).click();
  await expect(page.getByRole("region", { name: "Your call is booked" })).toBeFocused();
  await expect(page.getByText("Check your email for the calendar invitation.")).toBeVisible();
  await expect(page.locator("#booking-confirmation time")).toHaveAttribute("datetime", slots[1].start_utc);
  expect(submissions).toEqual([{ guest_name: "Sample Guest", guest_email: "guest@example.test", guest_notes: "",
    start_utc: slots[1].start_utc, "cf-turnstile-response": "mock-turnstile-token" }]);
});

test("uncertain booking focuses guidance and stops resubmission", async ({ page }) => {
  await mockBookingFlow(page, (route) => route.fulfill({ status: 503, json: { error: "booking_pending_review", reference: "b123abc" } }));
  await findTimes(page);
  await page.getByRole("radio", { name: "1:00 AM EDT" }).check();
  await page.getByLabel("Name (required)").fill("Sample Guest");
  await page.getByLabel("Email (required)").fill("guest@example.test");
  await page.getByRole("button", { name: "Book this time" }).click();
  await expect(page.locator("#booking-errors")).toBeFocused();
  await expect(page.getByText(/Do not submit again.*b123abc/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Book this time" })).toBeHidden();
});

test("page keeps the fail-closed no-JS contract and stays on-origin", async ({ browser, baseURL }) => {
  const origin = new URL(baseURL).origin;
  const external = [];
  const context = await browser.newContext({ javaScriptEnabled: false, baseURL });
  const page = await context.newPage();
  await page.route("**/*", (route) => {
    const url = new URL(route.request().url());
    if (url.origin !== origin && !url.hostname.endsWith("cloudflare.com")) external.push(url.href);
    return url.origin === origin ? route.continue() : route.abort();
  });
  const response = await page.goto("/book-a-call");
  expect(response.ok()).toBe(true);
  // With JavaScript disabled the form stays hidden (the `hidden` attribute is
  // removed by booking-client.js, so submission fails closed by construction)
  // and the noscript notice offers the phone fallback.
  await expect(page.locator("#booking-form")).toBeHidden();
  await expect(page.locator("#booking-form noscript p")).toContainText("Online scheduling requires JavaScript");
  await expect(page.locator("#booking-form noscript a[href^='tel:']")).toBeAttached();
  expect(external).toEqual([]);
  await context.close();
});
