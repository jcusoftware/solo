// Dual security-header mandate: static assets (_headers) and Worker /api/* responses
// (crates/sws-security/src/headers.rs) must emit the identical header set.
import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { parseHeadersFile } from "../core/tools/check-headers.mjs";

function contractHeaders() {
  const text = readFileSync(new URL("../core/public/_headers", import.meta.url), "utf8");
  return Object.fromEntries(
    parseHeadersFile(text, "/*").map(([name, value]) => [name.toLowerCase(), value]),
  );
}

const EXPECTED = contractHeaders();
const API_ERROR_PATHS = ["/api/does-not-exist"];

function expectSecurityHeaders(resp, label) {
  const got = resp.headers();
  for (const [name, value] of Object.entries(EXPECTED)) {
    expect(got[name], `${label} ${name}`).toBe(value);
  }
}

test("contract parsed from _headers", () => {
  for (const h of [
    "strict-transport-security",
    "x-content-type-options",
    "x-frame-options",
    "referrer-policy",
    "permissions-policy",
    "cross-origin-opener-policy",
    "content-security-policy",
  ]) {
    expect(EXPECTED[h], h).toBeTruthy();
  }
  expect(EXPECTED["content-security-policy"]).toContain("frame-ancestors 'none'");
  expect(EXPECTED["content-security-policy"]).toContain("https://challenges.cloudflare.com");
});

for (const path of ["/", "/takeout/"]) {
  test(`static asset ${path} has security headers and does not force no-store`, async ({ request }) => {
    const resp = await request.get(path, { maxRedirects: 0 });
    expect(resp.status()).toBe(200);
    expectSecurityHeaders(resp, path);
    const cc = (resp.headers()["cache-control"] ?? "").toLowerCase();
    expect(cc).not.toContain("no-store");
  });
}

test("static /does-not-exist serves 404.html with 404 status and security headers", async ({ request }) => {
  const resp = await request.get("/does-not-exist", { maxRedirects: 0 });
  expect(resp.status()).toBe(404);
  expect(resp.headers()["content-type"]).toContain("text/html");
  expectSecurityHeaders(resp, "/does-not-exist");
  const html = await resp.text();
  expect(html).toContain("Page not found");
  expect(html).toContain('<meta name="robots" content="noindex">');
});

for (const path of API_ERROR_PATHS) {
  test(`security headers and no-store on ${path}`, async ({ request }) => {
    const resp = await request.get(path, { maxRedirects: 0 });
    expect(resp.status()).toBe(404);
    expect(resp.headers()["cache-control"]).toContain("no-store");
    expectSecurityHeaders(resp, path);
  });
}

test("GET /api/health returns ok JSON with no-store and security headers", async ({ request }) => {
  const resp = await request.get("/api/health", { maxRedirects: 0 });
  expect(resp.status()).toBe(200);
  expect(resp.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(resp, "/api/health");
  const body = await resp.json();
  expect(body.status).toBe("ok");
  expect(body.version).toBeTruthy();
});

test("HEAD /api/health returns 200 with empty body, no-store, and security headers", async ({ request }) => {
  const resp = await request.head("/api/health", { maxRedirects: 0 });
  expect(resp.status()).toBe(200);
  expect(resp.headers()["content-type"]).toContain("application/json");
  expect(resp.headers()["cache-control"]).toContain("no-store");
  expect((await resp.body()).length).toBe(0);
  expectSecurityHeaders(resp, "HEAD /api/health");
});

for (const { method, path, allow } of [
  { method: "POST", path: "/api/health", allow: "GET, HEAD" },
  { method: "GET", path: "/api/forms/contact", allow: "POST" },
  { method: "GET", path: "/api/inquire", allow: "POST" },
  { method: "DELETE", path: "/api/newsletter", allow: "POST" },
  { method: "GET", path: "/api/checkout", allow: "POST" },
]) {
  test(`${method} ${path} returns 405 with Allow and security headers`, async ({ request }) => {
    const resp = await request.fetch(path, { method, maxRedirects: 0 });
    expect(resp.status()).toBe(405);
    expect(resp.headers()["allow"]).toBe(allow);
    expect(resp.headers()["cache-control"]).toContain("no-store");
    expectSecurityHeaders(resp, `${method} ${path}`);
    expect(await resp.json()).toEqual({ ok: false, error: "method_not_allowed" });
  });
}

test("POST /api/forms/contact accepts valid submission with 201 and no-store", async ({ request }) => {
  const resp = await request.post("/api/forms/contact", {
    data: {
      name: "Alex Example",
      email: "alex@example.test",
      phone: "+1 (703) 555-0199",
      message: "General contact form E2E test.",
      source_route: "/contact",
      "cf-turnstile-response": "XXXX.DUMMY.TOKEN.XXXX",
    },
  });
  expect(resp.status()).toBe(201);
  expect(resp.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(resp, "POST /api/forms/contact 201");
  const body = await resp.json();
  expect(body.ok).toBe(true);
  expect(body.form_id).toBe("contact");
  expect(typeof body.id).toBe("number");
  expect(body.id).toBeGreaterThan(0);
});

test("POST /api/forms/does-not-exist returns 404 unknown_form with no-store and security headers", async ({ request }) => {
  const resp = await request.post("/api/forms/does-not-exist", {
    data: {
      name: "Alex Example",
      email: "alex@example.test",
      message: "Unknown form test.",
      source_route: "/",
    },
  });
  expect(resp.status()).toBe(404);
  expect(resp.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(resp, "POST /api/forms/does-not-exist 404");
  expect(await resp.json()).toEqual({ ok: false, error: "unknown_form" });
});

test("POST /api/inquire accepts valid submission with 201 and no-store", async ({ request }) => {
  const resp = await request.post("/api/inquire", {
    data: {
      kind: "private_event",
      name: "Alex Example",
      email: "alex@example.test",
      phone: "+1 (703) 555-0199",
      event_date: "2027-06-15",
      guest_count: 25,
      message: "Private dinner inquiry for E2E test.",
      source_route: "/book-an-event",
      "cf-turnstile-response": "XXXX.DUMMY.TOKEN.XXXX",
    },
  });
  expect(resp.status()).toBe(201);
  expect(resp.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(resp, "POST /api/inquire 201");
  const body = await resp.json();
  expect(body.ok).toBe(true);
  expect(typeof body.id).toBe("number");
  expect(body.id).toBeGreaterThan(0);
});

test("POST /api/inquire returns 400 validation_failed with no-store and security headers", async ({ request }) => {
  const resp = await request.post("/api/inquire", {
    data: {
      kind: "invalid_kind",
      name: "",
      email: "not-an-email",
      message: "Hello",
      source_route: "/elsewhere",
    },
  });
  expect(resp.status()).toBe(400);
  expect(resp.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(resp, "POST /api/inquire 400");
  const body = await resp.json();
  expect(body.ok).toBe(false);
  expect(body.error).toBe("validation_failed");
  expect(body.fields).toMatchObject({
    kind: "invalid",
    name: "required",
    email: "invalid",
    source_route: "invalid",
  });
});

test("POST /api/inquire rejects honeypot without leaking field name", async ({ request }) => {
  const resp = await request.post("/api/inquire", {
    data: {
      kind: "contact",
      name: "Bot Example",
      email: "bot@example.test",
      message: "Spam message",
      source_route: "/contact",
      website: "https://spam.example",
      "cf-turnstile-response": "XXXX.DUMMY.TOKEN.XXXX",
    },
  });
  expect(resp.status()).toBe(400);
  expect(resp.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(resp, "POST /api/inquire honeypot");
  const raw = await resp.text();
  expect(raw).not.toContain("website");
  expect(JSON.parse(raw)).toEqual({ ok: false, error: "submission_rejected" });
});

test("POST /api/inquire rejects >16 KB payload with 413", async ({ request }) => {
  const oversize = "x".repeat(16 * 1024 + 128);
  const resp = await request.post("/api/inquire", {
    headers: { "Content-Type": "application/json" },
    data: JSON.stringify({
      kind: "contact",
      name: "Alex Example",
      email: "alex@example.test",
      message: oversize,
      source_route: "/contact",
    }),
  });
  expect(resp.status()).toBe(413);
  expect(resp.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(resp, "POST /api/inquire 413");
  expect(await resp.json()).toEqual({ ok: false, error: "payload_too_large" });
});

test("POST /api/inquire returns 415 for unsupported Content-Type including binary non-UTF-8 body", async ({ request }) => {
  const textResp = await request.post("/api/inquire", {
    headers: { "Content-Type": "text/plain" },
    data: "kind=contact&name=Alex",
  });
  expect(textResp.status()).toBe(415);
  expect(textResp.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(textResp, "POST /api/inquire 415 text/plain");
  expect(await textResp.json()).toEqual({ ok: false, error: "unsupported_media_type" });

  const binResp = await request.post("/api/inquire", {
    headers: { "Content-Type": "application/octet-stream" },
    data: Buffer.from([0xff, 0xfe, 0x00, 0x80]),
  });
  expect(binResp.status()).toBe(415);
  expect(binResp.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(binResp, "POST /api/inquire 415 binary");
  expect(await binResp.json()).toEqual({ ok: false, error: "unsupported_media_type" });
});

test("POST /api/newsletter rejects missing consent with 400 and no-store", async ({ request }) => {
  const resp = await request.post("/api/newsletter", {
    data: { email: "guest@example.com", source_route: "/" },
  });
  expect(resp.status()).toBe(400);
  expect(resp.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(resp, "POST /api/newsletter missing consent");
  const body = await resp.json();
  expect(body.ok).toBe(false);
  expect(body.error).toBe("validation_failed");
  expect(body.fields?.consent).toBe("required");
});

test("POST /api/newsletter returns 201 and handles duplicate email generically without overwriting consent", async ({ request }) => {
  const email = `e2e-nl-${Date.now()}-${Math.random().toString(36).slice(2, 8)}@example.test`;
  const first = await request.post("/api/newsletter", {
    data: {
      email,
      consent: "yes",
      consent_text: "Initial consent wording.",
      source_route: "/",
      "cf-turnstile-response": "XXXX.DUMMY.TOKEN.XXXX",
    },
  });
  expect(first.status()).toBe(201);
  expect(first.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(first, "POST /api/newsletter first");
  const firstBody = await first.json();
  expect(firstBody.ok).toBe(true);
  expect(firstBody.status).toBe("received");
  expect(typeof firstBody.consent_timestamp).toBe("string");

  const second = await request.post("/api/newsletter", {
    data: {
      email: email.toUpperCase(),
      consent: "yes",
      consent_text: "Attempted overwrite consent wording.",
      source_route: "/menu",
      "cf-turnstile-response": "XXXX.DUMMY.TOKEN.XXXX",
    },
  });
  expect(second.status()).toBe(201);
  expect(second.headers()["cache-control"]).toContain("no-store");
  expectSecurityHeaders(second, "POST /api/newsletter duplicate");
  const secondBody = await second.json();
  expect(secondBody.ok).toBe(true);
  expect(secondBody.status).toBe("received");
  expect(typeof secondBody.consent_timestamp).toBe("string");
});

test("forms.js browser validation error flow preserves nl-email-hint and manages focus", async ({ page }) => {
  await page.route("https://challenges.cloudflare.com/**", (r) => r.abort());
  await page.goto("/");

  const form = page.locator('form.js-api-form[action="/api/newsletter"]');
  const emailInput = form.locator("#nl-email");
  const consentBox = form.locator("#nl-consent");
  const submitBtn = form.locator('button[type="submit"]');
  const status = form.locator('[role="status"]');

  // Passes HTML5 type="email" validation in Chromium so submit fires, but fails server TLD >= 2 validation.
  await emailInput.fill("guest@invalid.c");
  await consentBox.check();
  await submitBtn.click();

  await expect(emailInput).toHaveAttribute("aria-invalid", "true");
  const describedBy = await emailInput.getAttribute("aria-describedby");
  expect(describedBy?.split(/\s+/)).toEqual(expect.arrayContaining(["nl-email-hint", "nl-email-error"]));
  await expect(form.locator("#nl-email-error")).toBeVisible();
  await expect(form.locator("#nl-email-error")).not.toBeEmpty();
  await expect(emailInput).toBeFocused();
  await expect(status).toContainText("Please check the highlighted fields");
});

test("forms.js browser valid submission flow succeeds and clears invalid state", async ({ page }) => {
  await page.goto("/");

  const form = page.locator('form.js-api-form[action="/api/newsletter"]');
  const emailInput = form.locator("#nl-email");
  const consentBox = form.locator("#nl-consent");
  const submitBtn = form.locator('button[type="submit"]');
  const status = form.locator('[role="status"]');

  // Wait briefly for the always-pass Turnstile test widget to populate cf-turnstile-response if loaded.
  const tokenInput = form.locator('input[name="cf-turnstile-response"]');
  await tokenInput.waitFor({ state: "attached", timeout: 10_000 }).catch(() => {});
  if ((await tokenInput.count()) > 0) {
    await expect(tokenInput).not.toHaveValue("", { timeout: 10_000 });
  }

  // Trigger validation error first so we also verify clearInvalid() on subsequent valid submit.
  await emailInput.fill("guest@invalid.c");
  await consentBox.check();
  await submitBtn.click();
  await expect(emailInput).toHaveAttribute("aria-invalid", "true");

  const validEmail = `e2e-browser-${Date.now()}@example.test`;
  await emailInput.fill(validEmail);
  await submitBtn.click();

  await expect(status).toContainText("Thanks! You are subscribed.", { timeout: 10_000 });
  await expect(emailInput).not.toHaveAttribute("aria-invalid", "true");
  await expect(emailInput).toHaveAttribute("aria-describedby", "nl-email-hint");
  await expect(form.locator(".field-error")).toHaveCount(0);
  await expect(emailInput).toHaveValue("");
  await expect(consentBox).not.toBeChecked();
});

// Isolated block for issue #6 (Turnstile fail-closed): invalid-token failure path.
// Kept separate to minimize merge conflicts with other spec work.
test.describe("Turnstile failure paths (issue #6)", () => {
  // Observed against `wrangler dev` with the repo's only secret, the always-pass
  // Cloudflare TEST dummy key:
  //   - DEV_MODE active (CI passes --var DEV_MODE:true; .dev.vars.example ships
  //     DEV_MODE=true): the TEST secret accepts every token, so the request
  //     succeeds (201). The strict 403 invalid-token arm is therefore unreachable
  //     in local dev and is covered by unit tests in
  //     crates/sws-security/src/turnstile.rs.
  //   - DEV_MODE unset with the TEST secret: the new fail-closed guard rejects
  //     with 503 {"error":"turnstile_unconfigured"} BEFORE siteverify is called
  //     (verified: the same request answered 503 on port 8788 with --var
  //     DEV_MODE:false).
  // This test pins that an invalid token shape can never produce an unexplained
  // 4xx/5xx other than the two documented outcomes above.
  test("POST with invalid Turnstile token shape stays within documented outcomes", async ({ request }) => {
    const resp = await request.post("/api/newsletter", {
      data: {
        email: `e2e-turnstile-invalid-${Date.now()}@example.test`,
        consent: "yes",
        consent_text: "Issue #6 invalid Turnstile token probe.",
        source_route: "/menu",
        "cf-turnstile-response": "XXXX.invalid-token-shape.XXXX",
      },
    });
    expect([201, 503]).toContain(resp.status());
    if (resp.status() === 503) {
      const body = await resp.json();
      expect(body.ok).toBe(false);
      expect(body.error).toBe("turnstile_unconfigured");
    }
    expectSecurityHeaders(resp, "POST /api/newsletter invalid turnstile token");
  });
});

// Isolated block for issue #22 (CAN-SPAM re-subscribe prevention): seeds an
// `unsubscribed` row directly into local D1, re-subscribes for that email, and
// verifies both the generic 201 (no enumeration) and that the stored status and
// original consent timestamp survive untouched.
//
// Seeding mechanics: shells out to the repo's local `node_modules/.bin/wrangler`
// (`d1 execute <DB_NAME> --local --json --command ...`, DB name from
// wrangler.jsonc, schema from crates/sws-worker-site/migrations/0001_init.sql).
// Local state lives in `.wrangler/state`, which `wrangler dev` — started by the
// Playwright webServer (playwright.config.mjs, after e2e-setup.mjs has applied
// migrations) — reads from the same directory, so the seeded row is visible to
// the running server and no network or Cloudflare account is involved.
test.describe("Newsletter unsubscribed protection (issue #22)", () => {
  const seedOrQuery = (sql) => {
    const out = execFileSync(
      "node_modules/.bin/wrangler",
      ["d1", "execute", "sws-demo", "--local", "--json", "--command", sql],
      { encoding: "utf8", cwd: new URL("..", import.meta.url).pathname },
    );
    const start = out.indexOf("[");
    const end = out.lastIndexOf("]");
    if (start < 0 || end < 0) {
      throw new Error(`wrangler d1 execute did not emit JSON: ${out.slice(0, 200)}`);
    }
    const statements = JSON.parse(out.slice(start, end + 1));
    return statements[0]?.results ?? [];
  };
  const seedUnsubscribed = (email) => {
    seedOrQuery(
      `INSERT OR REPLACE INTO newsletter_subscribers (email, status, consent_timestamp, source_route, consent_text) VALUES ('${email}', 'unsubscribed', strftime('%Y-%m-%dT%H:%M:%SZ','now'), '/issue-22-seed', 'Issue #22 seeded consent record.')`,
    );
    const [seeded] = seedOrQuery(
      `SELECT status, consent_timestamp, source_route, consent_text FROM newsletter_subscribers WHERE email = '${email}'`,
    );
    expect(seeded, "seeded unsubscribed row must exist").toBeTruthy();
    expect(seeded.status).toBe("unsubscribed");
    return seeded;
  };

  test("re-subscribing cannot overwrite an unsubscribed row's status or original consent timestamp", async ({ request }) => {
    const email = `e2e-issue22-${Date.now()}-${Math.random().toString(36).slice(2, 8)}@example.test`;
    const seeded = seedUnsubscribed(email);

    const resp = await request.post("/api/newsletter", {
      data: {
        email,
        consent: "yes",
        consent_text: "Attempted re-subscribe consent wording.",
        source_route: "/menu",
        "cf-turnstile-response": "XXXX.DUMMY.TOKEN.XXXX",
      },
    });
    // Same generic 201 as any other success: no enumeration of prior status.
    expect(resp.status()).toBe(201);
    expect(resp.headers()["cache-control"]).toContain("no-store");
    expectSecurityHeaders(resp, "POST /api/newsletter re-subscribe");
    const body = await resp.json();
    expect(body.ok).toBe(true);
    expect(body.status).toBe("received");
    expect(typeof body.consent_timestamp).toBe("string");
    expect(JSON.stringify(body).toLowerCase()).not.toContain("unsubscribed");

    // The seeded row survives the re-subscribe attempt byte-for-byte.
    const [after] = seedOrQuery(
      `SELECT status, consent_timestamp, source_route, consent_text FROM newsletter_subscribers WHERE email = '${email}'`,
    );
    expect(after).toBeTruthy();
    expect(after.status).toBe("unsubscribed");
    expect(after.consent_timestamp).toBe(seeded.consent_timestamp);
    expect(after.source_route).toBe("/issue-22-seed");
    expect(after.consent_text).toBe("Issue #22 seeded consent record.");
  });
});

test("checkout rejects non-form posts and oversized bodies with security headers", async ({ request }) => {
  for (const [options, status] of [
    [{ data: { mode: "subscription" } }, 415],
    [{ headers: { "Content-Type": "application/x-www-form-urlencoded" }, data: "x=" + "a".repeat(4096) }, 413],
  ]) {
    const resp = await request.post("/api/checkout", options);
    expect(resp.status()).toBe(status);
    expect(resp.headers()["cache-control"]).toBe("no-store");
    expect(resp.headers().location).toBeUndefined();
    for (const [name, value] of Object.entries(EXPECTED)) expect(resp.headers()[name]).toBe(value);
  }
});
