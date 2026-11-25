#!/usr/bin/env node
// Tenant-agnostic static renderer (zero dependencies).
//
// Usage: node core/tools/render-tenant.mjs <venue.json> <outdir> [--check]
//
// Writes <outdir>/{index.html,404.html,styles.css,robots.txt,sitemap.xml}
// deterministically (no timestamps). With --check, nothing is written and the
// process exits 1 if any generated file is missing or differs.
// Also verifies the manifest's recorded brand contrast ratios (WCAG 2.x).

import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const DAYS = ['monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday', 'sunday'];
const DIET_SCHEMA = {
  vegan: 'VeganDiet',
  vegetarian: 'VegetarianDiet',
  'gluten-free': 'GlutenFreeDiet',
  'dairy-free': 'LowLactoseDiet',
  halal: 'HalalDiet',
  kosher: 'KosherDiet',
  'low-salt': 'LowSaltDiet',
};
const DIET_LABEL = {
  vegan: 'Vegan',
  vegetarian: 'Vegetarian',
  'gluten-free': 'Gluten-free',
  'dairy-free': 'Dairy-free',
  halal: 'Halal',
  kosher: 'Kosher',
  'low-salt': 'Low-salt',
};
const HHMM = /^([01][0-9]|2[0-3]):[0-5][0-9]$/;
const HEX = /^#[0-9A-Fa-f]{6}$/;
const SLUG = /^[a-z0-9]+(-[a-z0-9]+)*$/;
const CSS_INJECTION = /[;{}\n\r\\]|\/\*|\*\/|url\(/i;
const URL_CONTROL_OR_WS = /[\u0000-\u0020\u007f]/;
const TURNSTILE_SRC = 'https://challenges.cloudflare.com/turnstile/v0/api.js';

// ---------- helpers ----------
export function esc(value) {
  return String(value)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function jsonLd(obj) {
  // Safe inside <script>: neutralise "<", ">", "&" and JS line separators.
  return JSON.stringify(obj)
    .replace(/</g, '\\u003c')
    .replace(/>/g, '\\u003e')
    .replace(/&/g, '\\u0026')
    .replace(/\u2028/g, '\\u2028')
    .replace(/\u2029/g, '\\u2029');
}

const cap = (s) => s.charAt(0).toUpperCase() + s.slice(1);

function time12(hhmm) {
  const [h, m] = hhmm.split(':').map(Number);
  const suffix = h < 12 ? 'AM' : 'PM';
  const h12 = h % 12 === 0 ? 12 : h % 12;
  return `${h12}:${String(m).padStart(2, '0')} ${suffix}`;
}

const timeEl = (hhmm) => `<time datetime="${esc(hhmm)}">${esc(time12(hhmm))}</time>`;

function dayRanges(days) {
  const idx = [...new Set(days.map((d) => DAYS.indexOf(d)))].sort((a, b) => a - b);
  const out = [];
  for (let i = 0; i < idx.length; i++) {
    let j = i;
    while (j + 1 < idx.length && idx[j + 1] === idx[j] + 1) j++;
    out.push(i === j ? cap(DAYS[idx[i]]) : `${cap(DAYS[idx[i]])}–${cap(DAYS[idx[j]])}`);
    i = j;
  }
  return out.join(', ');
}

function formatPrice(price, currency) {
  return currency === 'USD' ? `$${price}` : `${price} ${currency}`;
}

function phoneParts(phone) {
  const digits = phone.replace(/[^0-9+]/g, '');
  const m = /^\+1-(\d{3})-(\d{3})-(\d{4})$/.exec(phone);
  return { href: `tel:${digits}`, display: m ? `(${m[1]}) ${m[2]}-${m[3]}` : phone };
}

export function contrastRatio(a, b) {
  const lum = (hex) => {
    const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
    const l = c.map((x) => (x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4));
    return 0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2];
  };
  const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

function formatItemPrice(it, currency) {
  if (it.price !== null && it.price !== undefined) {
    const base = formatPrice(it.price, currency);
    return it.price_note ? `${base} (${it.price_note})` : base;
  }
  return it.price_note ?? '';
}

function isHttpsUrl(val) {
  if (typeof val !== 'string' || !val.trim() || URL_CONTROL_OR_WS.test(val)) return false;
  try {
    return new URL(val).protocol === 'https:';
  } catch {
    return false;
  }
}

function hasBalancedQuotes(val) {
  let quote = null;
  for (const ch of val) {
    if (quote) {
      if (ch === quote) quote = null;
    } else if (ch === '"' || ch === "'") {
      quote = ch;
    }
  }
  return quote === null;
}

function isSafeFontStack(val) {
  return typeof val === 'string' && val.trim().length > 0 && !CSS_INJECTION.test(val) && hasBalancedQuotes(val);
}

function resolvePalette(pal) {
  const c = (k, fb) => pal[k] ?? pal[fb];
  return {
    background: pal.background,
    text: pal.text,
    surface: c('surface', 'background'),
    accent: pal.accent,
    accent_secondary: c('accent_secondary', 'accent'),
    muted_text: c('muted_text', 'text'),
    on_accent: c('on_accent', 'background'),
    footer_background: c('footer_background', 'text'),
    footer_text: c('footer_text', 'background'),
  };
}
export function validateVenue(v) {
  const errs = [];
  const need = (cond, msg) => {
    if (!cond) errs.push(msg);
  };
  need(v && typeof v === 'object', 'manifest must be an object');
  for (const k of ['slug', 'name', 'nap', 'hours', 'brand', 'menu', 'integrations', 'business_types']) {
    need(v?.[k] !== undefined, `missing required key: ${k}`);
  }
  if (errs.length) return errs;
  need(typeof v.slug === 'string' && SLUG.test(v.slug), 'slug must match ^[a-z0-9]+(-[a-z0-9]+)*$');
  need(typeof v.name === 'string' && v.name.trim().length > 0, 'name must be a non-empty string');
  need(Array.isArray(v.business_types) && v.business_types.length > 0, 'business_types must be a non-empty array');

  const nap = v.nap ?? {};
  need(typeof nap.name === 'string' && nap.name.trim().length > 0, 'nap.name required');
  need(typeof nap.phone === 'string' && nap.phone.trim().length > 0, 'nap.phone required');
  need(typeof nap.email === 'string' && nap.email.trim().length > 0, 'nap.email required');
  need(isHttpsUrl(nap.website), 'nap.website must be a valid https:// URL');
  const addr = nap.address;
  need(addr && typeof addr === 'object', 'nap.address required');
  if (addr && typeof addr === 'object') {
    for (const ak of ['street', 'locality', 'region', 'postal_code', 'country']) {
      need(typeof addr[ak] === 'string' && addr[ak].trim().length > 0, `nap.address.${ak} required`);
    }
  }
  if (v.booking_url !== undefined && v.booking_url !== null) {
    need(isHttpsUrl(v.booking_url), 'booking_url must be a valid https:// URL');
  }
  if (v.seo?.routes !== undefined) {
    need(Array.isArray(v.seo.routes), 'seo.routes must be an array');
    if (Array.isArray(v.seo.routes)) {
      for (const [i, r] of v.seo.routes.entries()) {
        need(typeof r === 'string' && r.startsWith('/'), `seo.routes[${i}] must start with /`);
      }
    }
  }

  for (const d of DAYS) {
    const h = v.hours[d];
    need(h && (h.closed === true || (HHMM.test(h.opens) && HHMM.test(h.closes))), `hours.${d} invalid`);
  }
  for (const [i, o] of (v.holiday_overrides ?? []).entries()) {
    need(/^\d{4}-\d{2}-\d{2}$/.test(o.date), `holiday_overrides[${i}].date invalid`);
    const isClosed = o.closed === true && o.opens === undefined && o.closes === undefined;
    const isOpen = (o.closed === undefined || o.closed === false) && HHMM.test(o.opens) && HHMM.test(o.closes);
    need(isClosed || isOpen, `holiday_overrides[${i}] needs closed: true or opens/closes`);
  }

  const fonts = v.brand.fonts;
  need(fonts && typeof fonts === 'object', 'brand.fonts required');
  if (fonts && typeof fonts === 'object') {
    need(
      isSafeFontStack(fonts.body),
      'brand.fonts.body required and must not contain CSS injection tokens',
    );
    if (fonts.heading !== undefined) {
      need(
        isSafeFontStack(fonts.heading),
        'brand.fonts.heading must be a non-empty string without CSS injection tokens',
      );
    }
  }

  const pal = v.brand.palette ?? {};
  let paletteValid = true;
  for (const [k, hex] of Object.entries(pal)) {
    const ok = typeof hex === 'string' && HEX.test(hex);
    if (!ok) paletteValid = false;
    need(ok, `brand.palette.${k} must be #RRGGBB`);
  }
  for (const k of ['background', 'text', 'accent']) {
    if (!pal[k]) paletteValid = false;
    need(pal[k], `brand.palette.${k} required`);
  }
  const pairs = v.brand.contrast?.pairs;
  need(Array.isArray(pairs) && pairs.length > 0, 'brand.contrast.pairs must be a non-empty array');
  for (const [i, p] of (Array.isArray(pairs) ? pairs : []).entries()) {
    const fg = pal[p.foreground];
    const bg = pal[p.background];
    if (!fg || !bg || !HEX.test(fg) || !HEX.test(bg)) {
      errs.push(`brand.contrast.pairs[${i}] references unknown or invalid palette key`);
      continue;
    }
    const actual = Math.round(contrastRatio(fg, bg) * 100) / 100;
    need(actual >= 4.5, `contrast ${p.foreground}/${p.background} = ${actual} < 4.5`);
    need(Math.abs(actual - p.ratio) <= 0.01, `contrast ${p.foreground}/${p.background} recorded ${p.ratio}, computed ${actual}`);
  }
  if (paletteValid) {
    const rp = resolvePalette(pal);
    const requiredPairs = [
      ['text', 'background'],
      ['text', 'surface'],
      ['accent', 'background'],
      ['accent', 'surface'],
      ['accent_secondary', 'background'],
      ['accent_secondary', 'surface'],
      ['muted_text', 'background'],
      ['muted_text', 'surface'],
      ['on_accent', 'accent'],
      ['footer_text', 'footer_background'],
    ];
    for (const [fgKey, bgKey] of requiredPairs) {
      const ratio = contrastRatio(rp[fgKey], rp[bgKey]);
      const actual = Math.round(ratio * 100) / 100;
      need(ratio >= 4.5, `required contrast ${fgKey}/${bgKey} = ${actual} < 4.5`);
    }
  }

  if (v.happy_hour) {
    need(HHMM.test(v.happy_hour.start) && HHMM.test(v.happy_hour.end), 'happy_hour start/end must be HH:MM');
    need((v.happy_hour.days ?? []).every((d) => DAYS.includes(d)), 'happy_hour.days invalid');
  }
  need(Array.isArray(v.menu?.sections) && v.menu.sections.length > 0, 'menu.sections must be a non-empty array');
  for (const s of v.menu?.sections ?? []) {
    need(SLUG.test(s.id ?? ''), `menu section id invalid: ${s.id}`);
    need(typeof s.name === 'string' && s.name.trim().length > 0, `menu section name required: ${s.id}`);
    need(Array.isArray(s.items) && s.items.length > 0, `menu section items must be a non-empty array: ${s.id}`);
    for (const it of s.items ?? []) {
      need(typeof it.name === 'string' && it.name.trim().length > 0, 'menu item name required');
      need(
        it.price === null || (typeof it.price === 'string' && /^\d+\.\d{2}$/.test(it.price)),
        `menu item price must be decimal string or null: ${it.name}`,
      );
      if (it.contains_alcohol !== undefined) {
        need(typeof it.contains_alcohol === 'boolean', `menu item contains_alcohol must be boolean: ${it.name}`);
      }
      if (it.price_note !== undefined) {
        need(typeof it.price_note === 'string', `menu item price_note must be string: ${it.name}`);
      }
      for (const t of it.dietary ?? []) need(DIET_SCHEMA[t], `unknown dietary tag ${t} on ${it.name}`);
    }
  }
  if (v.newsletter?.enabled) need(v.integrations?.turnstile?.site_key, 'integrations.turnstile.site_key required for newsletter');
  return errs;
}

// ---------- JSON-LD ----------
export function buildJsonLd(v) {
  const base = v.nap.website.replace(/\/$/, '');
  const a = v.nap.address;
  const ld = {
    '@context': 'https://schema.org',
    '@type': v.business_types.length === 1 ? v.business_types[0] : v.business_types,
    '@id': `${base}/#venue`,
    name: v.nap.name,
  };
  if (v.description) ld.description = v.description;
  ld.url = `${base}/`;
  ld.telephone = v.nap.phone;
  ld.email = v.nap.email;
  ld.address = {
    '@type': 'PostalAddress',
    streetAddress: a.street,
    addressLocality: a.locality,
    addressRegion: a.region,
    postalCode: a.postal_code,
    addressCountry: a.country,
  };
  if (v.geo) ld.geo = { '@type': 'GeoCoordinates', latitude: v.geo.latitude, longitude: v.geo.longitude };
  if (v.price_range) ld.priceRange = v.price_range;
  if (v.cuisine?.length) ld.servesCuisine = v.cuisine;
  ld.openingHoursSpecification = DAYS.filter((d) => !v.hours[d].closed).map((d) => ({
    '@type': 'OpeningHoursSpecification',
    dayOfWeek: `https://schema.org/${cap(d)}`,
    opens: v.hours[d].opens,
    closes: v.hours[d].closes,
  }));
  if (v.holiday_overrides?.length) {
    ld.specialOpeningHoursSpecification = v.holiday_overrides.map((o) => {
      const spec = {
        '@type': 'OpeningHoursSpecification',
        validFrom: o.date,
        validThrough: o.date,
      };
      if (!o.closed) {
        spec.opens = o.opens;
        spec.closes = o.closes;
      }
      return spec;
    });
  }
  ld.acceptsReservations =
    v.integrations.seating_policy !== 'walk_in_only' && v.booking_url ? v.booking_url : false;
  ld.hasMenu = {
    '@type': 'Menu',
    name: v.menu.name,
    hasMenuSection: v.menu.sections.map((s) => {
      const section = { '@type': 'MenuSection', name: s.name };
      if (s.description) section.description = s.description;
      section.hasMenuItem = s.items.map((it) => {
        const item = { '@type': 'MenuItem', name: it.name };
        if (it.description) item.description = it.description;
        if (it.price !== null && it.price !== undefined) {
          item.offers = { '@type': 'Offer', price: it.price, priceCurrency: v.menu.currency };
        } else if (it.price_note) {
          item.offers = { '@type': 'Offer', priceCurrency: v.menu.currency, description: it.price_note };
        }
        if (it.dietary?.length) item.suitableForDiet = it.dietary.map((t) => `https://schema.org/${DIET_SCHEMA[t]}`);
        return item;
      });
      return section;
    }),
  };
  return ld;
}

// ---------- HTML ----------
function addressBlock(v) {
  const a = v.nap.address;
  const ph = phoneParts(v.nap.phone);
  return `<address>
<span class="nap-name">${esc(v.nap.name)}</span><br>
${esc(a.street)}<br>
${esc(a.locality)}, ${esc(a.region)} ${esc(a.postal_code)}<br>
<a href="${esc(ph.href)}">${esc(ph.display)}</a><br>
<a href="mailto:${esc(v.nap.email)}">${esc(v.nap.email)}</a>
</address>`;
}

function head(v, { title, description, canonical, jsonld, forms, noindex }) {
  const lines = [
    '<!doctype html>',
    `<html lang="${esc((v.locale ?? 'en').split('-')[0])}">`,
    '<head>',
    '<meta charset="utf-8">',
    '<meta name="viewport" content="width=device-width, initial-scale=1">',
    `<title>${esc(title)}</title>`,
  ];
  if (typeof description === 'string' && description.trim()) {
    lines.push(`<meta name="description" content="${esc(description)}">`);
  }
  if (noindex) lines.push('<meta name="robots" content="noindex">');
  lines.push(`<meta name="theme-color" content="${esc(v.brand.palette.background)}">`);
  if (canonical) lines.push(`<link rel="canonical" href="${esc(canonical)}">`);
  lines.push('<link rel="icon" href="data:,">', '<link rel="stylesheet" href="/styles.css">');
  if (jsonld) lines.push(`<script type="application/ld+json">${jsonLd(jsonld)}</script>`);
  if (forms) {
    lines.push('<script src="/js/forms.js" defer></script>');
    lines.push(`<script src="${TURNSTILE_SRC}" async defer></script>`);
  }
  lines.push('</head>');
  return lines.join('\n');
}

function siteHeader(v) {
  let notice = '';
  if (v.fictional === true) {
    notice = v.legal_disclaimer
      ? `<p class="notice"><strong>${esc(v.legal_disclaimer)}.</strong> This site is a design demonstration; the business, address, phone number and menu are invented.</p>\n`
      : '<p class="notice">This site is a design demonstration; the business, address, phone number and menu are invented.</p>\n';
  } else if (v.legal_disclaimer) {
    notice = `<p class="notice">${esc(v.legal_disclaimer)}</p>\n`;
  }
  const navItems = [
    '<li><a href="/#menu">Menu</a></li>',
    v.happy_hour ? '<li><a href="/#happy-hour">Happy hour</a></li>' : '',
    '<li><a href="/#visit">Hours &amp; location</a></li>',
    v.newsletter?.enabled ? '<li><a href="/#newsletter">Newsletter</a></li>' : '',
  ]
    .filter(Boolean)
    .join('\n');
  return `<a class="skip-link" href="#main">Skip to main content</a>
<header class="site-header">
${notice}<div class="wrap header-row">
<p class="brand"><a href="/">${esc(v.name)}</a></p>
<nav aria-label="Primary">
<ul>
${navItems}
</ul>
</nav>
</div>
</header>`;
}

function newsletterForm(v) {
  const n = v.newsletter;
  const key = v.integrations.turnstile.site_key;
  const blurb = typeof n.blurb === 'string' && n.blurb.trim() ? `<p>${esc(n.blurb)}</p>\n` : '';
  return `<section class="newsletter" id="newsletter" aria-labelledby="newsletter-heading">
<h2 id="newsletter-heading">Newsletter</h2>
${blurb}<form class="js-api-form" action="/api/newsletter" method="post" data-success="Thanks! You are subscribed. Check your inbox for future updates.">
<input type="hidden" name="source_route" value="/">
<input type="hidden" name="consent_text" value="${esc(n.consent_text)}">
<div class="field">
<label for="nl-email">Email address (required)</label>
<input id="nl-email" name="email" type="email" autocomplete="email" inputmode="email" maxlength="254" required aria-describedby="nl-email-hint">
<p class="hint" id="nl-email-hint">Example: name@example.com</p>
</div>
<div class="field checkbox">
<input id="nl-consent" name="consent" type="checkbox" value="yes" required>
<label for="nl-consent">${esc(n.consent_text)} (required)</label>
</div>
${n.privacy_note ? `<p class="hint">${esc(n.privacy_note)}</p>\n` : ''}<div class="hp" aria-hidden="true">
<label for="nl-website">Leave this field empty</label>
<input id="nl-website" name="website" type="text" tabindex="-1" autocomplete="off">
</div>
<div class="cf-turnstile" data-sitekey="${esc(key)}" data-action="newsletter"></div>
<button type="submit">Subscribe</button>
<p class="form-status" role="status" aria-live="polite"></p>
</form>
</section>`;
}

function siteFooter(v, { withForm }) {
  let disclaimer = '';
  if (v.fictional === true) {
    disclaimer = v.legal_disclaimer
      ? `<p class="small">${esc(v.legal_disclaimer)}. Not a real business.</p>`
      : '<p class="small">Not a real business.</p>';
  } else if (v.legal_disclaimer) {
    disclaimer = `<p class="small">${esc(v.legal_disclaimer)}</p>`;
  }
  return `<footer class="site-footer">
<div class="wrap footer-grid">
${withForm && v.newsletter?.enabled ? newsletterForm(v) + '\n' : ''}<div class="footer-nap">
<p class="footer-title">Find us</p>
${addressBlock(v)}
${disclaimer}
</div>
</div>
</footer>`;
}

function hoursTable(v) {
  const rows = DAYS.map((d) => {
    const h = v.hours[d];
    const cell = h.closed ? 'Closed' : `${timeEl(h.opens)} – ${timeEl(h.closes)}`;
    return `<tr><th scope="row">${cap(d)}</th><td>${cell}</td></tr>`;
  }).join('\n');
  return `<table class="hours">
<caption>Regular opening hours</caption>
<thead><tr><th scope="col">Day</th><th scope="col">Hours</th></tr></thead>
<tbody>
${rows}
</tbody>
</table>`;
}

function holidayList(v) {
  if (!v.holiday_overrides?.length) return '';
  const items = v.holiday_overrides
    .map((o) => {
      const when = o.closed ? 'Closed' : `${timeEl(o.opens)} – ${timeEl(o.closes)}`;
      return `<li><time datetime="${esc(o.date)}">${esc(o.label)}</time>: ${when}</li>`;
    })
    .join('\n');
  return `<h3>Holiday hours</h3>\n<ul>\n${items}\n</ul>`;
}

function seatingText(v) {
  switch (v.integrations.seating_policy) {
    case 'walk_in_only':
      return 'We are walk-in only. No reservations are needed; just come by.';
    default:
      return v.booking_url
        ? `<a href="${esc(v.booking_url)}">Reserve a table online</a>.`
        : 'Please call us to reserve a table.';
  }
}

function menuSections(v) {
  return v.menu.sections
    .map((s) => {
      const items = s.items
        .map((it) => {
          const tags = (it.dietary ?? []).map((t) => DIET_LABEL[t]).join(', ');
          const priceText = formatItemPrice(it, v.menu.currency);
          const priceHtml = priceText ? ` <span class="price">${esc(priceText)}</span>` : '';
          return `<li class="menu-item">
<p class="item-head"><span class="item-name">${esc(it.name)}</span>${priceHtml}</p>
${it.description ? `<p class="item-desc">${esc(it.description)}</p>\n` : ''}${tags ? `<p class="tags">${esc(tags)}</p>\n` : ''}</li>`;
        })
        .join('\n');
      return `<section class="menu-section card" aria-labelledby="menu-${esc(s.id)}">
<h3 id="menu-${esc(s.id)}">${esc(s.name)}</h3>
${s.description ? `<p class="section-desc">${esc(s.description)}</p>\n` : ''}<ul class="menu-items">
${items}
</ul>
</section>`;
    })
    .join('\n');
}

function happyHour(v) {
  const hh = v.happy_hour;
  if (!hh) return '';
  const offers = hh.offers.map((o) => `<li>${esc(o)}</li>`).join('\n');
  return `<section class="happy-hour card" id="happy-hour" aria-labelledby="happy-hour-heading">
<h2 id="happy-hour-heading">Happy hour</h2>
<p class="hh-when">${esc(dayRanges(hh.days))}, ${timeEl(hh.start)} – ${timeEl(hh.end)}</p>
<ul>
${offers}
</ul>
${hh.notes ? `<p class="small">${esc(hh.notes)}</p>` : ''}
</section>`;
}

export function renderIndex(v) {
  const base = v.nap.website.replace(/\/$/, '');
  const a = v.nap.address;
  const access = (v.venue_accessibility ?? []).map((x) => `<li>${esc(x)}</li>`).join('\n');
  const locationText =
    typeof v.category_label === 'string' && v.category_label.trim()
      ? `${esc(v.category_label)} in ${esc(a.locality)}, ${esc(a.region)}.`
      : `${esc(a.locality)}, ${esc(a.region)}.`;
  return `${head(v, {
    title: v.seo?.title ?? v.name,
    description: v.seo?.description ?? v.description,
    canonical: `${base}/`,
    jsonld: buildJsonLd(v),
    forms: Boolean(v.newsletter?.enabled),
  })}
<body>
${siteHeader(v)}
<main id="main" tabindex="-1">
<div class="wrap">
<div class="intro">
<h1>${esc(v.name)}</h1>
${v.tagline ? `<p class="tagline">${esc(v.tagline)}</p>\n` : ''}${v.description ? `<p>${esc(v.description)}</p>\n` : ''}<p class="small">${locationText}</p>
</div>
<section class="visit card" id="visit" aria-labelledby="visit-heading">
<h2 id="visit-heading">Hours &amp; location</h2>
${addressBlock(v)}
${hoursTable(v)}
${holidayList(v)}
<h3>Reservations</h3>
<p>${seatingText(v)}</p>
${access ? `<h3>Accessibility</h3>\n<ul>\n${access}\n</ul>` : ''}
</section>
${happyHour(v)}
<section class="menu" id="menu" aria-labelledby="menu-heading">
<h2 id="menu-heading">${esc(v.menu.name)}</h2>
${menuSections(v)}
</section>
</div>
</main>
${siteFooter(v, { withForm: true })}
</body>
</html>
`;
}

export function render404(v) {
  return `${head(v, {
    title: `Page not found — ${v.name}`,
    description: `Page not found. ${v.seo?.description ?? ''}`.trim(),
    canonical: null,
    jsonld: null,
    forms: false,
    noindex: true,
  })}
<body>
${siteHeader(v)}
<main id="main" tabindex="-1">
<div class="wrap intro">
<h1>Page not found</h1>
<p>Sorry, we could not find that page. <a href="/">Return to the home page</a>.</p>
</div>
</main>
${siteFooter(v, { withForm: false })}
</body>
</html>
`;
}

export function renderCss(v) {
  const rp = resolvePalette(v.brand.palette);
  const f = v.brand.fonts;
  return `/* Generated by core/tools/render-tenant.mjs from ${v.slug}/venue.json. Do not edit. */
:root {
  --bg: ${rp.background};
  --text: ${rp.text};
  --surface: ${rp.surface};
  --accent: ${rp.accent};
  --accent-2: ${rp.accent_secondary};
  --muted: ${rp.muted_text};
  --on-accent: ${rp.on_accent};
  --footer-bg: ${rp.footer_background};
  --footer-text: ${rp.footer_text};
  --font-body: ${f.body};
  --font-heading: ${f.heading ?? f.body};
}
*, *::before, *::after { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; text-size-adjust: 100%; }
@media (prefers-reduced-motion: no-preference) { html { scroll-behavior: smooth; } }
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { animation: none !important; transition: none !important; scroll-behavior: auto !important; }
}
body { margin: 0; background: var(--bg); color: var(--text); font-family: var(--font-body); font-size: 1.0625rem; line-height: 1.6; }
h1, h2, h3, .brand, .menu-title, .footer-title { font-family: var(--font-heading); line-height: 1.2; }
h1 { font-size: clamp(2rem, 5vw, 3rem); margin: 0 0 .5rem; }
h2 { font-size: 1.6rem; margin: 0 0 1rem; }
h3 { font-size: 1.2rem; margin: 1.5rem 0 .5rem; }
a { color: var(--accent); text-decoration-thickness: .08em; text-underline-offset: .18em; }
a:hover { text-decoration-thickness: .16em; }
:focus-visible { outline: 3px solid var(--text); outline-offset: 3px; }
.site-footer :focus-visible { outline-color: var(--footer-text); }
main:focus { outline: none; }
.wrap { max-width: 60rem; margin: 0 auto; padding: 0 1.25rem; }
.skip-link { position: absolute; left: 1rem; top: -100rem; z-index: 10; padding: .75rem 1rem; background: var(--bg); color: var(--text); border: 2px solid var(--text); font-weight: 700; }
.skip-link:focus, .skip-link:focus-visible { top: 1rem; outline: 3px solid var(--bg); outline-offset: 2px; box-shadow: 0 0 0 7px var(--text); }
.notice { margin: 0; padding: .5rem 1.25rem; background: var(--text); color: var(--bg); text-align: center; font-size: .95rem; }
.site-header { border-bottom: 1px solid var(--muted); }
.header-row { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: .5rem 1.5rem; padding-top: .75rem; padding-bottom: .75rem; }
.brand { margin: 0; font-size: 1.5rem; font-weight: 700; }
.brand a { color: var(--text); text-decoration: none; }
nav ul { list-style: none; margin: 0; padding: 0; display: flex; flex-wrap: wrap; gap: .25rem 1rem; }
nav a { display: inline-block; min-height: 44px; padding: .6rem .25rem; }
.intro { padding: 2.5rem 0 1rem; }
.tagline { font-size: 1.25rem; color: var(--accent); margin-top: 0; }
.small, .hint { font-size: .9rem; color: var(--muted); }
.card { background: var(--surface); border: 1px solid var(--muted); border-radius: .5rem; padding: 1.5rem; margin: 1.5rem 0; }
address { font-style: normal; margin-bottom: 1rem; }
address a { display: inline-block; min-height: 24px; padding: .25rem 0; }
.nap-name { font-weight: 700; }
table.hours { border-collapse: collapse; width: 100%; max-width: 28rem; }
.hours caption { text-align: left; font-weight: 700; padding-bottom: .5rem; }
.hours th, .hours td { text-align: left; padding: .4rem .5rem; border-bottom: 1px solid var(--muted); }
.hh-when { font-weight: 700; color: var(--accent-2); }
.menu > h2 { margin: 2rem 0 1rem; }
.section-desc { color: var(--muted); margin: 0 0 1rem; }
.menu-items { list-style: none; margin: 0; padding: 0; display: grid; gap: 1rem; }
@media (min-width: 40rem) { .menu-items { grid-template-columns: 1fr 1fr; } }
.menu-item p { margin: 0; }
.item-head { display: flex; justify-content: space-between; gap: 1rem; font-weight: 700; }
.price { color: var(--accent); white-space: nowrap; }
.item-desc { color: var(--muted); }
.tags { font-size: .85rem; color: var(--accent-2); }
.site-footer { background: var(--footer-bg); color: var(--footer-text); margin-top: 3rem; padding: 2.5rem 0; }
.site-footer a, .site-footer .small, .site-footer .hint { color: var(--footer-text); }
.footer-grid { display: grid; gap: 2rem; }
@media (min-width: 48rem) { .footer-grid { grid-template-columns: 3fr 2fr; } }
.footer-title { font-size: 1.3rem; font-weight: 700; margin: 0 0 .75rem; }
.newsletter h2 { font-size: 1.3rem; }
.field { margin: 0 0 1rem; }
.field label { display: block; font-weight: 700; margin-bottom: .25rem; }
.field .hint { margin: .25rem 0 0; }
input[type="email"], input[type="text"], input[type="tel"], input[type="date"], input[type="number"], textarea, select { width: 100%; max-width: 26rem; min-height: 44px; padding: .5rem .75rem; font: inherit; color: var(--text); background: var(--surface); border: 2px solid var(--muted); border-radius: .25rem; }
input[aria-invalid="true"], textarea[aria-invalid="true"], select[aria-invalid="true"] { box-shadow: inset 0 0 0 2px currentColor; }
.site-footer input[type="email"], .site-footer input[type="text"], .site-footer input[type="tel"], .site-footer input[type="date"], .site-footer input[type="number"], .site-footer textarea, .site-footer select { border-color: var(--footer-text); }
.checkbox { display: flex; flex-wrap: wrap; align-items: flex-start; gap: .75rem; }
.checkbox input { flex: none; width: 24px; height: 24px; margin: .15rem 0 0; accent-color: var(--accent); }
.site-footer .checkbox input { accent-color: var(--footer-text); }
.checkbox label { flex: 1 1 0; min-width: 0; font-weight: 400; margin: 0; }
.field-error { margin: .25rem 0 0; font-size: .9rem; font-weight: 700; }
.checkbox .field-error { flex: 1 0 100%; margin: 0; }
button { min-height: 44px; min-width: 44px; padding: .6rem 1.25rem; font: inherit; font-weight: 700; color: var(--on-accent); background: var(--accent); border: 2px solid var(--accent); border-radius: .25rem; cursor: pointer; }
.site-footer button { color: var(--footer-bg); background: var(--footer-text); border-color: var(--footer-text); }
button[disabled] { cursor: progress; }
.cf-turnstile { margin: 0 0 1rem; min-height: 65px; }
.form-status { min-height: 1.6em; font-weight: 700; }
.hp { position: absolute; left: -10000px; width: 1px; height: 1px; overflow: hidden; }
@media print { .site-footer form, nav, .skip-link { display: none; } body { background: #fff; color: #000; } }
`;
}

export function renderRobots(v) {
  const base = v.nap.website.replace(/\/$/, '');
  return `User-agent: *\nAllow: /\nDisallow: /api/\n\nSitemap: ${base}/sitemap.xml\n`;
}

export function renderSitemap(v) {
  const base = v.nap.website.replace(/\/$/, '');
  const routes = v.seo?.routes?.length ? v.seo.routes : ['/'];
  const urls = routes.map((r) => `  <url><loc>${esc(base + r)}</loc></url>`).join('\n');
  return `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${urls}\n</urlset>\n`;
}

export function renderAll(v) {
  return {
    'index.html': renderIndex(v),
    '404.html': render404(v),
    'styles.css': renderCss(v),
    'robots.txt': renderRobots(v),
    'sitemap.xml': renderSitemap(v),
  };
}

// ---------- CLI ----------
async function main(argv) {
  const args = argv.filter((a) => !a.startsWith('--'));
  const check = argv.includes('--check');
  if (args.length !== 2) {
    console.error('usage: render-tenant.mjs <venue.json> <outdir> [--check]');
    return 2;
  }
  const [manifestPath, outdir] = args;
  let venue;
  try {
    venue = JSON.parse(await readFile(manifestPath, 'utf8'));
  } catch (e) {
    console.error(`render-tenant: cannot read ${manifestPath}: ${e.message}`);
    return 2;
  }
  const errs = validateVenue(venue);
  if (errs.length) {
    for (const e of errs) console.error(`render-tenant: ${manifestPath}: ${e}`);
    return 1;
  }
  const files = renderAll(venue);
  let stale = 0;
  if (!check) await mkdir(outdir, { recursive: true });
  for (const [name, content] of Object.entries(files)) {
    const path = join(outdir, name);
    if (check) {
      const existing = await readFile(path, 'utf8').catch(() => null);
      if (existing !== content) {
        console.error(`render-tenant: STALE ${path} (run npm run render)`);
        stale++;
      }
    } else {
      await writeFile(path, content);
    }
  }
  const html = Buffer.byteLength(files['index.html']);
  const css = Buffer.byteLength(files['styles.css']);
  const total = html + css;
  console.log(`render-tenant: index.html ${html} B + styles.css ${css} B = ${total} B${check ? ' (check)' : ''}`);
  if (total > 50 * 1024) {
    console.error('render-tenant: HTML+CSS exceeds 50 KB budget');
    return 1;
  }
  return stale ? 1 : 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = await main(process.argv.slice(2));
}
