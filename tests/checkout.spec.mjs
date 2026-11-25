import { test, expect } from "@playwright/test";

test("static cart updates and performs no checkout or Stripe requests on load or edit", async ({ page, baseURL }) => {
  const requests = [];
  await page.route("**/*", (route) => {
    const url = new URL(route.request().url());
    requests.push(url);
    return url.origin === new URL(baseURL).origin ? route.continue() : route.abort();
  });
  const response = await page.goto("/takeout");
  expect((await response.body()).length).toBeLessThan(12 * 1024);
  const total = page.getByRole("status");
  await expect(total).toHaveText("$0.00");
  const flatbread = page.getByRole("spinbutton", { name: /garden flatbread/i });
  const soup = page.getByRole("spinbutton", { name: /tomato soup/i });
  await flatbread.fill("2");
  await soup.fill("1");
  await expect(total).toHaveText("$30.50");
  await flatbread.fill("0");
  await expect(total).toHaveText("$6.50");
  await soup.fill("6");
  await expect(total).toHaveText("Check quantities");
  await expect(page.getByText(/no real charge or fulfillment/)).toHaveCount(3);
  await expect(page.getByText(/\$16\.00 USD per month/)).toBeVisible();
  expect(requests.some((url) => url.hostname.endsWith("stripe.com") || url.pathname.startsWith("/api/"))).toBe(false);
});

for (const mode of ["payment", "subscription"]) {
  test(`native ${mode} form posts with site enhancement disabled`, async ({ page }) => {
    await page.route("**/js/forms.js", (route) => route.abort());
    // Browser-only Turnstile stand-in; the production Worker never accepts it.
    await page.route("https://challenges.cloudflare.com/**", (route) => route.fulfill({
      contentType: "application/javascript",
      body: `function mockWidgets() {
        for (const widget of document.querySelectorAll('.cf-turnstile')) {
          const input = document.createElement('input');
          input.type = 'hidden'; input.name = 'cf-turnstile-response'; input.value = 'browser-mock';
          widget.append(input);
        }
      }
      if (document.readyState === 'loading') addEventListener('DOMContentLoaded', mockWidgets);
      else mockWidgets();`,
    }));
    let submitted;
    await page.route("**/api/checkout", (route) => {
      const request = route.request();
      expect(request.method()).toBe("POST");
      expect(request.isNavigationRequest()).toBe(true);
      submitted = Object.fromEntries(new URLSearchParams(request.postData()));
      return route.fulfill({ contentType: "text/html", body: "<h1>Mock checkout handoff</h1>" });
    });
    await page.goto("/takeout");
    await expect(page.locator('input[name="cf-turnstile-response"]')).toHaveCount(2);
    const button = page.getByRole("button", { name: mode === "payment" ? /Stripe for pickup/ : /Stripe for monthly pickup/ });
    if (mode === "payment") await page.getByRole("spinbutton", { name: /garden flatbread/i }).fill("2");
    await button.click();
    await expect(page.getByRole("heading", { name: "Mock checkout handoff" })).toBeVisible();
    expect(submitted).toEqual({ mode, "cf-turnstile-response": "browser-mock",
      ...(mode === "payment" ? { "qty-flatbread": "2", "qty-soup": "0", "qty-cookie": "0" } : {}) });
  });
}
