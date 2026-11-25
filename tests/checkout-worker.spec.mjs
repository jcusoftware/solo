import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";
import { Miniflare, convertV4MiniflareOptions } from "miniflare"; // Wrangler's lockfile-pinned local runtime.

const catalog = JSON.parse(readFileSync(new URL("../core/public/demo-checkout-catalog.json", import.meta.url)));
const origin = "http://127.0.0.1:8787";
const stripeUrl = "https://checkout.stripe.com/c/pay/cs_test_Mock#fidMock";
const products = { ...catalog.items, [catalog.subscription.id]: catalog.subscription.cents };
const ids = Object.fromEntries(Object.keys(products).map((id) => [id, `price_${id}`]));

function mockPrice(sku) {
  const monthly = sku === catalog.subscription.id;
  return { object: "price", id: ids[sku], livemode: false, active: true,
    currency: "usd", unit_amount: products[sku], billing_scheme: "per_unit",
    custom_unit_amount: null, transform_quantity: null,
    type: monthly ? "recurring" : "one_time",
    recurring: monthly ? { interval: "month", interval_count: 1, usage_type: "licensed" } : null };
}

test("compiled Worker rejects public-host dummy secrets and redirects both loopback modes with mocked upstreams", async () => {
  const calls = [];
  const mf = new Miniflare(convertV4MiniflareOptions({
    modules: [
      { type: "ESModule", path: fileURLToPath(new URL("../crates/sws-worker-site/build/index.js", import.meta.url)) },
      { type: "CompiledWasm", path: fileURLToPath(new URL("../crates/sws-worker-site/build/index_bg.wasm", import.meta.url)) },
    ],
    compatibilityDate: "2026-11-01",
    bindings: {
      DEMO_CHECKOUT_ORIGIN: origin,
      TURNSTILE_SECRET_KEY: "1x0000000000000000000000000000000AA",
      STRIPE_DEMO_SECRET_KEY: "sk_test_Mock",
      STRIPE_DEMO_PRICE_IDS: JSON.stringify(ids),
    },
    serviceBindings: { ASSETS: (req) => {
      expect(new URL(req.url).pathname).toBe("/demo-checkout-catalog.json");
      return Response.json(catalog);
    } },
    outboundService: async (req) => {
      const url = new URL(req.url);
      calls.push(url.origin + url.pathname);
      if (url.href === "https://challenges.cloudflare.com/turnstile/v0/siteverify") {
        expect(req.method).toBe("POST");
        const token = new URLSearchParams(await req.text()).get("response");
        return Response.json({ success: token === "mock-ok" });
      }
      expect(url.origin).toBe("https://api.stripe.com");
      expect(req.headers.get("authorization")).toBe("Bearer sk_test_Mock");
      if (url.pathname.startsWith("/v1/prices/")) {
        expect(req.method).toBe("GET");
        return Response.json(mockPrice(url.pathname.split("/price_")[1]));
      }
      expect(url.pathname).toBe("/v1/checkout/sessions");
      expect(req.method).toBe("POST");
      const form = new URLSearchParams(await req.text());
      const mode = form.get("mode");
      const expected = mode === "subscription"
        ? [[catalog.subscription.id, 1]] : [["flatbread", 2], ["soup", 1]];
      const lines = expected.map(([sku, qty], i) => {
        expect(form.get(`line_items[${i}][price]`)).toBe(ids[sku]);
        expect(form.get(`line_items[${i}][quantity]`)).toBe(String(qty));
        return { price: mockPrice(sku), quantity: qty, currency: "usd", amount_total: qty * products[sku] };
      });
      const total = lines.reduce((sum, line) => sum + line.amount_total, 0);
      return Response.json({ object: "checkout.session", id: "cs_test_Mock", livemode: false,
        mode, status: "open", ui_mode: "hosted", currency: "usd", url: stripeUrl,
        success_url: `${origin}/takeout#returned`,
        cancel_url: `${origin}/takeout#${mode === "subscription" ? "subscription" : "pickup"}-heading`,
        amount_subtotal: total, amount_total: total, line_items: { has_more: false, data: lines } });
    },
  }));

  const worker = await mf.getWorker();
  const post = (base, fields) => worker.fetch(`${base}/api/checkout`, {
    method: "POST", redirect: "manual", headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ "cf-turnstile-response": "mock-ok", ...fields }).toString(),
  });
  try {
    for (const wrongOrigin of ["https://venue.example", "http://127.0.0.1:8788"]) {
      const blocked = await post(wrongOrigin, { mode: "subscription" });
      expect(blocked.status).toBe(503);
      expect(blocked.headers.has("location")).toBe(false);
    }
    expect(calls).toEqual([]);
    const rejected = await post(origin, { mode: "subscription", "cf-turnstile-response": "mock-failed" });
    expect(rejected.status, JSON.stringify(calls)).toBe(403);
    expect(calls).toEqual(["https://challenges.cloudflare.com/turnstile/v0/siteverify"]);
    for (const fields of [{ mode: "payment", "qty-flatbread": "2", "qty-soup": "1" }, { mode: "subscription" }]) {
      const response = await post(origin, fields);
      expect(response.status).toBe(303);
      expect(response.headers.get("location")).toBe(stripeUrl);
      expect(response.headers.get("cache-control")).toBe("no-store");
      expect(response.headers.get("x-frame-options")).toBe("DENY");
      expect(response.headers.get("content-security-policy")).toContain("frame-ancestors 'none'");
    }
  } finally {
    await mf.dispose();
  }
});
