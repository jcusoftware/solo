import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import {
  contrastRatio,
  validateVenue,
  buildJsonLd,
  renderIndex,
  render404,
  renderCss,
} from '../../core/tools/render-tenant.mjs';

const DEMO_PATH = resolve(process.cwd(), 'packs/hospitality/demo/venue.json');
const DEMO_VENUE = JSON.parse(readFileSync(DEMO_PATH, 'utf8'));

function makeVenue(overrides = {}) {
  const base = {
    schema_version: 1,
    slug: 'harbor-lantern',
    name: 'Harbor Lantern',
    fictional: false,
    business_types: ['Restaurant'],
    locale: 'en-US',
    nap: {
      name: 'Harbor Lantern',
      address: {
        street: '100 Oak Lane',
        locality: 'Alexandria',
        region: 'VA',
        postal_code: '22314',
        country: 'US',
      },
      phone: '+1-703-555-0199',
      email: 'hello@harbor-lantern.example',
      website: 'https://harbor-lantern.example',
    },
    hours: {
      monday: { opens: '11:00', closes: '22:00' },
      tuesday: { opens: '11:00', closes: '22:00' },
      wednesday: { opens: '11:00', closes: '22:00' },
      thursday: { opens: '11:00', closes: '22:00' },
      friday: { opens: '11:00', closes: '23:00' },
      saturday: { opens: '10:00', closes: '23:00' },
      sunday: { closed: true },
    },
    booking_url: null,
    brand: {
      palette: {
        background: '#FAF8F5',
        text: '#1C1917',
        surface: '#FFFFFF',
        accent: '#7C2D12',
        muted_text: '#57534E',
        on_accent: '#FFFFFF',
        footer_background: '#1C1917',
        footer_text: '#FAF8F5',
      },
      contrast: {
        pairs: [
          { foreground: 'text', background: 'background', ratio: 16.5 },
        ],
      },
      fonts: {
        body: 'system-ui, sans-serif',
        heading: 'Georgia, serif',
      },
    },
    integrations: {
      seating_policy: 'walk_in_only',
      turnstile: { site_key: '1x00000000000000000000AA' },
    },
    newsletter: {
      enabled: false,
      consent_text: 'Yes, send me updates.',
    },
    menu: {
      name: 'Dinner Menu',
      currency: 'USD',
      sections: [
        {
          id: 'plates',
          name: 'Small Plates',
          items: [
            { name: 'Roasted Carrots', price: '9.00', dietary: ['vegan'] },
          ],
        },
      ],
    },
  };
  return { ...base, ...overrides };
}

describe('validateVenue', () => {
  it('accepts the demo venue manifest and a minimal valid venue', () => {
    assert.deepEqual(validateVenue(DEMO_VENUE), []);
    assert.deepEqual(validateVenue(makeVenue()), []);
  });

  it('rejects slug CSS breakout characters', () => {
    const errs = validateVenue(makeVenue({ slug: 'foo;color:red' }));
    assert.ok(errs.some((e) => e.includes('slug')));

    const errsUpper = validateVenue(makeVenue({ slug: 'Foo-Bar' }));
    assert.ok(errsUpper.some((e) => e.includes('slug')));
  });

  it('rejects javascript: and non-https booking_url and nap.website', () => {
    const v1 = makeVenue({ booking_url: 'javascript:alert(1)' });
    assert.ok(validateVenue(v1).some((e) => e.includes('booking_url')));

    const v2 = makeVenue({ booking_url: 'http://harbor-lantern.example/book' });
    assert.ok(validateVenue(v2).some((e) => e.includes('booking_url')));

    const v3 = makeVenue();
    v3.nap.website = 'javascript:void(0)';
    assert.ok(validateVenue(v3).some((e) => e.includes('nap.website')));
  });

  it('rejects ASCII control and whitespace characters in nap.website and booking_url', () => {
    for (const badUrl of [
      'https://harbor-lantern.example/\nDisallow:/',
      'https://harbor-lantern.example/\rDisallow: /',
      'https://harbor-lantern.example/\tfoo',
      'https://harbor-lantern.example/ path',
      'https://harbor-lantern.example/\u0000foo',
      'https://harbor-lantern.example/\u007ffoo',
    ]) {
      const badSite = makeVenue();
      badSite.nap.website = badUrl;
      assert.ok(
        validateVenue(badSite).some((e) => e.includes('nap.website')),
        `expected nap.website rejection for ${JSON.stringify(badUrl)}`,
      );

      const badBooking = makeVenue({ booking_url: badUrl });
      assert.ok(
        validateVenue(badBooking).some((e) => e.includes('booking_url')),
        `expected booking_url rejection for ${JSON.stringify(badUrl)}`,
      );
    }
  });

  it('rejects missing brand.fonts, CSS injection tokens, backslashes, and unbalanced quotes in brand.fonts', () => {
    const noFonts = makeVenue();
    delete noFonts.brand.fonts;
    assert.ok(validateVenue(noFonts).some((e) => e.includes('brand.fonts')));

    for (const badFont of [
      'Arial; color: red',
      'Arial { color: red }',
      'Arial, url(https://evil.example/a.css)',
      'Arial /* comment */',
      "Arial\nbody{display:none}",
      'Arial\\',
      'Arial\\; color: red',
      '"Arial',
      'Arial"',
      "'DM Sans",
      "DM Sans'",
      '"DM Sans\'',
      '"Source Serif 4""',
    ]) {
      const badBody = makeVenue();
      badBody.brand.fonts.body = badFont;
      assert.ok(
        validateVenue(badBody).some((e) => e.includes('brand.fonts.body')),
        `expected rejection for body font: ${badFont}`,
      );

      const badHeading = makeVenue();
      badHeading.brand.fonts.heading = badFont;
      assert.ok(
        validateVenue(badHeading).some((e) => e.includes('brand.fonts.heading')),
        `expected rejection for heading font: ${badFont}`,
      );
    }

    for (const goodFont of [
      '"Source Serif 4", Georgia, serif',
      "'DM Sans', sans-serif",
      '"O\'Malley Sans", Georgia, serif',
    ]) {
      const vGood = makeVenue();
      vGood.brand.fonts.body = goodFont;
      vGood.brand.fonts.heading = goodFont;
      assert.deepEqual(validateVenue(vGood), [], `expected valid font stack: ${goodFont}`);
    }
  });

  it('rejects empty brand.contrast.pairs and low-contrast required rendered pairs', () => {
    const emptyPairs = makeVenue();
    emptyPairs.brand.contrast.pairs = [];
    assert.ok(validateVenue(emptyPairs).some((e) => e.includes('brand.contrast.pairs')));

    const lowAccent = makeVenue();
    lowAccent.brand.palette.accent = '#D4B59E'; // low contrast on #FAF8F5 and #FFFFFF
    const errs = validateVenue(lowAccent);
    assert.ok(errs.some((e) => e.includes('required contrast accent/background')));
    assert.ok(errs.some((e) => e.includes('required contrast accent/surface')));

    const lowMuted = makeVenue();
    lowMuted.brand.palette.muted_text = '#E8E1D1'; // ~1.2:1 on #FAF8F5 and #FFFFFF, omitted from pairs
    const mutedErrs = validateVenue(lowMuted);
    assert.ok(mutedErrs.some((e) => e.includes('required contrast muted_text/background')));
    assert.ok(mutedErrs.some((e) => e.includes('required contrast muted_text/surface')));

    const lowAccentSecondary = makeVenue();
    lowAccentSecondary.brand.palette.accent_secondary = '#EAE3D3'; // omitted from pairs
    const secErrs = validateVenue(lowAccentSecondary);
    assert.ok(secErrs.some((e) => e.includes('required contrast accent_secondary/background')));
    assert.ok(secErrs.some((e) => e.includes('required contrast accent_secondary/surface')));

    const lowFooter = makeVenue();
    lowFooter.brand.palette.footer_text = '#333333';
    lowFooter.brand.palette.footer_background = '#1C1917';
    assert.ok(validateVenue(lowFooter).some((e) => e.includes('required contrast footer_text/footer_background')));

    const lowOnAccent = makeVenue();
    lowOnAccent.brand.palette.on_accent = '#8B4513';
    assert.ok(validateVenue(lowOnAccent).some((e) => e.includes('required contrast on_accent/accent')));

    const missingAccent = makeVenue();
    delete missingAccent.brand.palette.accent;
    assert.ok(validateVenue(missingAccent).some((e) => e.includes('brand.palette.accent required')));
  });

  it('supports menu-normalizer nullable price, price_note, contains_alcohol, and section description', () => {
    const v = makeVenue({
      menu: {
        name: 'Tasting Menu',
        currency: 'USD',
        sections: [
          {
            id: 'raw-bar',
            name: 'Raw Bar',
            description: 'Chilled local shellfish served daily.',
            items: [
              {
                name: 'Market Oysters',
                price: null,
                price_note: 'Market price',
                contains_alcohol: false,
              },
              {
                name: 'Sparkling Flight',
                price: '18.00',
                price_note: 'three 2oz pours',
                contains_alcohol: true,
              },
            ],
          },
        ],
      },
    });
    assert.deepEqual(validateVenue(v), []);

    const badAlcohol = structuredClone(v);
    badAlcohol.menu.sections[0].items[0].contains_alcohol = 'yes';
    assert.ok(validateVenue(badAlcohol).some((e) => e.includes('contains_alcohol')));

    const badNote = structuredClone(v);
    badNote.menu.sections[0].items[0].price_note = 42;
    assert.ok(validateVenue(badNote).some((e) => e.includes('price_note')));
  });

  it('enforces holiday_overrides mutual exclusivity between closed and opens/closes', () => {
    const both = makeVenue({
      holiday_overrides: [
        { date: '2026-12-25', label: 'Winter Holiday', closed: true, opens: '10:00', closes: '16:00' },
      ],
    });
    assert.ok(validateVenue(both).some((e) => e.includes('holiday_overrides[0]')));

    const neither = makeVenue({
      holiday_overrides: [{ date: '2026-12-25', label: 'Winter Holiday', closed: false }],
    });
    assert.ok(validateVenue(neither).some((e) => e.includes('holiday_overrides[0]')));

    const valid = makeVenue({
      holiday_overrides: [
        { date: '2026-12-25', label: 'Winter Holiday', closed: true },
        { date: '2026-12-31', label: 'New Year Eve', opens: '16:00', closes: '23:00' },
      ],
    });
    assert.deepEqual(validateVenue(valid), []);
  });
});

describe('renderIndex & render404', () => {
  it('omits meta description and description <p> when description is absent', () => {
    const v = makeVenue();
    delete v.description;
    delete v.seo;
    const html = renderIndex(v);
    assert.equal(html.includes('<p>undefined</p>'), false);
    assert.equal(html.includes('<meta name="description"'), false);
    assert.equal(html.includes('undefined'), false);
  });

  it('omits leading "in " when category_label is absent', () => {
    const v = makeVenue();
    delete v.category_label;
    const html = renderIndex(v);
    assert.ok(html.includes('<p class="small">Alexandria, VA.</p>'));
    assert.equal(html.includes(' in Alexandria, VA.'), false);
  });

  it('gates demo disclaimer on fictional === true and renders legal_disclaimer alone otherwise', () => {
    const realWithDisclaimer = makeVenue({
      fictional: false,
      legal_disclaimer: '21+ for bar seating after 9 PM',
    });
    const html = renderIndex(realWithDisclaimer);
    assert.ok(html.includes('<p class="notice">21+ for bar seating after 9 PM</p>'));
    assert.ok(html.includes('<p class="small">21+ for bar seating after 9 PM</p>'));
    assert.equal(html.includes('design demonstration'), false);
    assert.equal(html.includes('Not a real business'), false);

    const realNoDisclaimer = makeVenue({ fictional: false });
    const htmlPlain = renderIndex(realNoDisclaimer);
    assert.equal(htmlPlain.includes('class="notice"'), false);
    assert.equal(htmlPlain.includes('design demonstration'), false);
    assert.equal(htmlPlain.includes('Not a real business'), false);

    const demoHtml = renderIndex(DEMO_VENUE);
    assert.ok(demoHtml.includes('This site is a design demonstration'));
    assert.ok(demoHtml.includes('Not a real business.'));
  });

  it('omits newsletter blurb when absent and renders custom blurb when provided', () => {
    const vNoBlurb = makeVenue({
      newsletter: { enabled: true, consent_text: 'Yes, email me updates.' },
    });
    const htmlNoBlurb = renderIndex(vNoBlurb);
    assert.equal(htmlNoBlurb.includes('Seasonal menus'), false);
    assert.equal(htmlNoBlurb.includes('wine tastings'), false);
    assert.ok(htmlNoBlurb.includes('<h2 id="newsletter-heading">Newsletter</h2>\n<form'));

    const vWithBlurb = makeVenue({
      newsletter: {
        enabled: true,
        blurb: 'Monthly supper club invitations.',
        consent_text: 'Yes, email me updates.',
      },
    });
    const htmlWithBlurb = renderIndex(vWithBlurb);
    assert.ok(htmlWithBlurb.includes('<p>Monthly supper club invitations.</p>'));
  });

  it('conditionally renders /#happy-hour and /#newsletter nav links', () => {
    const minimal = makeVenue();
    const htmlMin = renderIndex(minimal);
    assert.equal(htmlMin.includes('href="/#happy-hour"'), false);
    assert.equal(htmlMin.includes('href="/#newsletter"'), false);

    const full = makeVenue({
      happy_hour: {
        days: ['monday', 'friday'],
        start: '16:00',
        end: '18:00',
        offers: ['$1 off oysters'],
      },
      newsletter: { enabled: true, consent_text: 'Yes, email me.' },
    });
    const htmlFull = renderIndex(full);
    assert.ok(htmlFull.includes('href="/#happy-hour"'));
    assert.ok(htmlFull.includes('href="/#newsletter"'));
  });

  it('emits booking_url in JSON-LD acceptsReservations and descriptive link text when enabled', () => {
    const bookable = makeVenue({
      booking_url: 'https://reserve.harbor-lantern.example/table',
      integrations: {
        seating_policy: 'resy',
        turnstile: { site_key: '1x00000000000000000000AA' },
      },
    });
    const ld = buildJsonLd(bookable);
    assert.equal(ld.acceptsReservations, 'https://reserve.harbor-lantern.example/table');
    const html = renderIndex(bookable);
    assert.ok(html.includes('<a href="https://reserve.harbor-lantern.example/table">Reserve a table online</a>.'));

    const walkIn = makeVenue({
      booking_url: 'https://reserve.harbor-lantern.example/table',
      integrations: {
        seating_policy: 'walk_in_only',
        turnstile: { site_key: '1x00000000000000000000AA' },
      },
    });
    assert.equal(buildJsonLd(walkIn).acceptsReservations, false);
  });

  it('omits opens and closes on closed specialOpeningHoursSpecification entries', () => {
    const v = makeVenue({
      holiday_overrides: [
        { date: '2026-11-26', label: 'Thanksgiving', closed: true },
        { date: '2026-12-24', label: 'Christmas Eve', opens: '11:00', closes: '15:00' },
      ],
    });
    const ld = buildJsonLd(v);
    assert.equal(ld.specialOpeningHoursSpecification.length, 2);
    const closedSpec = ld.specialOpeningHoursSpecification[0];
    assert.equal(closedSpec.validFrom, '2026-11-26');
    assert.equal('opens' in closedSpec, false);
    assert.equal('closes' in closedSpec, false);
    assert.equal(JSON.stringify(closedSpec).includes('00:00'), false);

    const openSpec = ld.specialOpeningHoursSpecification[1];
    assert.equal(openSpec.opens, '11:00');
    assert.equal(openSpec.closes, '15:00');
  });

  it('renders nullable price and price_note properly in HTML and JSON-LD', () => {
    const v = makeVenue({
      menu: {
        name: 'Harbor Menu',
        currency: 'USD',
        sections: [
          {
            id: 'specials',
            name: 'Daily Specials',
            description: 'Prepared fresh each afternoon.',
            items: [
              { name: 'Catch of the Day', price: null, price_note: 'Market price' },
              { name: 'Oysters', price: '18.00', price_note: 'half dozen' },
              { name: 'Bread Service', price: null },
            ],
          },
        ],
      },
    });
    const html = renderIndex(v);
    assert.ok(html.includes('<p class="section-desc">Prepared fresh each afternoon.</p>'));
    assert.ok(html.includes('<span class="item-name">Catch of the Day</span> <span class="price">Market price</span>'));
    assert.ok(html.includes('<span class="item-name">Oysters</span> <span class="price">$18.00 (half dozen)</span>'));
    assert.ok(html.includes('<p class="item-head"><span class="item-name">Bread Service</span></p>'));

    const ld = buildJsonLd(v);
    const items = ld.hasMenu.hasMenuSection[0].hasMenuItem;
    assert.deepEqual(items[0].offers, {
      '@type': 'Offer',
      priceCurrency: 'USD',
      description: 'Market price',
    });
    assert.equal('price' in items[0].offers, false);
    assert.deepEqual(items[1].offers, {
      '@type': 'Offer',
      price: '18.00',
      priceCurrency: 'USD',
    });
    assert.equal('offers' in items[2], false);
  });

  it('uses #menu h2/h3 hierarchy, (required) newsletter labels, and noindex on 404', () => {
    const v = makeVenue({
      newsletter: { enabled: true, consent_text: 'Yes, email me.' },
    });
    const html = renderIndex(v);
    assert.ok(html.includes('<section class="menu" id="menu" aria-labelledby="menu-heading">'));
    assert.ok(html.includes('<h2 id="menu-heading">Dinner Menu</h2>'));
    assert.ok(html.includes('<h3 id="menu-plates">Small Plates</h3>'));
    assert.ok(html.includes('<label for="nl-email">Email address (required)</label>'));
    assert.ok(html.includes('<label for="nl-consent">Yes, email me. (required)</label>'));

    const html404 = render404(v);
    assert.ok(html404.includes('<meta name="robots" content="noindex">'));
  });
});

describe('renderCss & contrastRatio', () => {
  it('enforces >= 4.5:1 palette contrast and footer checkbox >= 3:1 contrast', () => {
    const pal = DEMO_VENUE.brand.palette;
    const pairs = [
      ['text', 'background'],
      ['text', 'surface'],
      ['accent', 'background'],
      ['accent', 'surface'],
      ['on_accent', 'accent'],
      ['footer_text', 'footer_background'],
    ];
    for (const [fg, bg] of pairs) {
      assert.ok(
        contrastRatio(pal[fg], pal[bg]) >= 4.5,
        `expected ${fg}/${bg} >= 4.5:1`,
      );
    }
    // Footer checkbox uses --footer-text on --footer-bg
    assert.ok(contrastRatio(pal.footer_text, pal.footer_background) >= 3.0);

    const css = renderCss(DEMO_VENUE);
    assert.ok(css.includes('.site-footer .checkbox input { accent-color: var(--footer-text); }'));
  });

  it('uses non-CLS aria-invalid box-shadow, high-contrast .skip-link:focus, and 24px address link target', () => {
    const css = renderCss(makeVenue());
    assert.ok(
      css.includes('input[aria-invalid="true"], textarea[aria-invalid="true"], select[aria-invalid="true"] { box-shadow: inset 0 0 0 2px currentColor; }'),
    );
    assert.equal(css.includes('border-width: 4px'), false);
    assert.ok(
      css.includes('.skip-link:focus, .skip-link:focus-visible { top: 1rem; outline: 3px solid var(--bg); outline-offset: 2px; box-shadow: 0 0 0 7px var(--text); }'),
    );
    assert.ok(css.includes('address a { display: inline-block; min-height: 24px; padding: .25rem 0; }'));
  });
});
