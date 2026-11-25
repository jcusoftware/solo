# Security Policy

## Supported Versions

| Version | Supported |
| --- | --- |
| `0.1.x` | Yes |
| `< 0.1.0` | No |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues, discussions, or pull requests.**

Instead, report vulnerabilities privately using **GitHub Security Advisories** ("Report a vulnerability" under the repository's **Security** tab).

Please include as much of the following as possible:

- Affected crate, package, app, or route
- Steps to reproduce or a minimal proof of concept
- Potential impact (e.g., header bypass, XSS, SQL injection, DoS/memory exhaustion, secret leakage)
- Suggested remediation, if any

### Response Timeline

- **Acknowledgment:** Within 3 business days
- **Initial triage & severity assessment:** Within 7 business days
- **Coordinated disclosure:** We will coordinate a fix and advisory release with the reporter before public disclosure.

## Security Architecture Overview

Solo Website System (SWS) is engineered around defense-in-depth defaults for edge and static deployments:

1. **Dual Security-Header Parity**
   - Static assets rely on `_headers`, while dynamic `/api/*` Worker responses apply the byte-identical `SECURITY_HEADERS` constant via Rust (`with_security_headers`), covering 2xx, 4xx, and 5xx responses.
   - Enforced headers include HSTS (`max-age=63072000; includeSubDomains; preload`), `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: strict-origin-when-cross-origin`, restrictive `Permissions-Policy`, `Cross-Origin-Opener-Policy: same-origin`, and a strict Content Security Policy (`default-src 'self'`, `frame-ancestors 'none'`, `object-src 'none'`, `upgrade-insecure-requests`, with explicit allowlisting for `https://challenges.cloudflare.com` when Turnstile is enabled).
   - Header parity is continuously verified in unit tests (`cargo test`), static checks, and end-to-end Playwright checks against a live Worker.

2. **Bot Protection (Cloudflare Turnstile)**
   - Public form submission endpoints require server-side Cloudflare Turnstile `siteverify` validation before processing or storing input.
   - Development environments use official Cloudflare test keys; production deployments use per-site secrets injected at runtime.

3. **Request Bounds & Memory Safety**
   - Public API endpoints enforce a **16 KB streaming request body cap** to prevent memory exhaustion inside the 128 MB Cloudflare Workers isolate limit.
   - Worker/Wasm Rust code forbids `unwrap()` on untrusted input and avoids native/C-FFI dependencies, OS threads, or blocking I/O.

4. **Parameterized Persistence (D1 / SQLite)**
   - All D1 SQL queries strictly use **bound parameters** (`?` placeholders) rather than string interpolation.

5. **Secrets & Zone Isolation**
   - Secrets and credentials are never committed to Git. Local secrets live exclusively in gitignored `.dev.vars` or `private/*.env` files, and production secrets are provisioned via `wrangler secret put`.
   - Public Zone A artifacts (`crates/`, `apps/`, `packages/`, demo fixtures, tests, docs) must never contain real client credentials, exports, or proprietary customer data.
