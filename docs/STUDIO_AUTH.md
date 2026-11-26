# Studio Authentication Model (v0.2): cardless GitHub OAuth and invitations

Status: **design record for issue #60; the registration validation has NOT YET PERFORMED** (see §2). This document informs #61 (D1 registry), #62 (Durable Object sessions), #65 (auth implementation) and #68 (CLI/MCP credentials).

Source material: the issue bodies for #60, #61, #62, #65 and #68, plus `AGENTS.md` and `docs/ARCHITECTURE.md`. The "Studio Control Plane and Visual Builder design" document cited by those issues was not available when this was written, so every value the issues do not determine is marked **Proposed (needs maintainer confirmation)**. Nothing marked Proposed is approved. Cloudflare and GitHub quotas, limits and OAuth behaviours are **assumptions to re-verify against current vendor documentation**, not facts.

All examples use fictional `.example` domains. No credential, client ID or secret appears in this repository.

---

## 1. Decision and non-goals

**Decision (from #60).** Studio authentication for v0.2 is:

1. A **self-hosted GitHub OAuth app** owned by the deploying operator (not a shared SWS-operated app).
2. **One bootstrapped owner** per workspace.
3. **Explicit collaborator invitations** issued by the owner (or a role the owner delegates, see §4).
4. **Scoped, rotating, revocable CLI/MCP credentials** for non-browser clients (§7).

**Not launch dependencies (explicit non-goals for v0.2):**

- Cloudflare Access / Zero Trust. It may be added later as an optional layer, never as a default.
- Magic links (would need an email provider and deliverability work).
- Passkeys / WebAuthn.
- Any auth SaaS (Auth0, Clerk, etc.) or any paid plan, card, or payment method as a prerequisite.
- Social providers other than GitHub, SAML/SSO, SCIM, self-service sign-up.

**Hard constraint.** A fresh Cloudflare account and a fresh GitHub account, each with no payment method on file, must be able to complete bootstrap, invite a collaborator, sign in, and issue a CLI credential. If any step requires a card, the model fails #60 and must be revised.

Turnstile (AGENTS.md §8) stays mandatory on every **public** form. It is separate from Studio authentication: Studio sign-in is protected by OAuth `state` and rate limiting (§5), not by Turnstile on the OAuth redirect.

---

## 2. Prerequisites and OAuth app registration checklist

**Validation status: NOT YET PERFORMED.** This is a human step for the maintainer. It must be done from a genuinely fresh account pair with no payment method, and recorded below. No agent or CI job performs it, and no credential produced by it is ever committed.

### 2.1 Prerequisites

- [ ] A GitHub account with no payment method on file and no paid plan (note whether it is a personal account or an organization-owned app).
- [ ] A Cloudflare account with no payment method on file (Workers Free plan).
- [ ] Ability to run `wrangler` locally (`npm ci`, `~/.cargo/bin` on `PATH`).
- [ ] A callback host: `http://127.0.0.1:8787` for local dev, and the `*.workers.dev` hostname of the Studio Worker (or a custom domain, which is optional) for production.

### 2.2 Registration steps (re-verify each against current GitHub docs)

1. In GitHub: Settings, Developer settings, OAuth Apps, "New OAuth App". (Confirm the path still exists and that creating an OAuth app does not prompt for billing.)
2. Fill in: application name (fictional-safe, e.g. `Solo Studio (example workspace)`), homepage URL, and **Authorization callback URL**.
3. Callback URL: propose `https://studio.example.workers.dev/auth/github/callback` for production and a **separate** OAuth app for local dev with `http://127.0.0.1:8787/auth/github/callback`. Using one app per environment avoids multiple-callback ambiguity. Re-verify GitHub's current rules for callback matching (host, port, sub-path) and whether `127.0.0.1` over `http` is accepted.
4. Generate a client secret. Record it **only** in `.dev.vars` (local) or via `wrangler secret put` (production); never in the repository, an issue, a PR, or chat (AGENTS.md §8).
5. Note the client ID (not secret, but still kept in config outside the repo for tenant-specific values) and bind it per §8.
6. Confirm the minimum scope (§5.1) works: sign in and fetch the numeric user ID.
7. Confirm whether the app can be used by accounts other than its owner without verification, review, or payment.

### 2.3 Items to verify (record the answer, do not assume)

| Question | Assumption to verify |
| --- | --- |
| Does OAuth app creation require a payment method? | Assumed no. |
| Does authorizing with no scope return the public profile including a stable numeric `id`? | Assumed yes (`GET /user`). |
| Is PKCE (`code_challenge`, `S256`) supported for OAuth apps? | Believed supported in recent GitHub releases; confirm. If not, rely on `state` plus the client secret. |
| Does the token response need `Accept: application/json` to return JSON? | Believed yes; confirm. |
| Can multiple client secrets coexist for zero-downtime rotation? | Believed yes; confirm. |
| Do OAuth app user-to-server tokens expire by default? | Believed not by default; confirm. The design does not depend on it because the GitHub token is discarded after identity lookup (§5.1). |
| Is there any rate limit on token exchange that affects free-tier sign-in volume? | Unknown; record. |
| Cloudflare: do Workers Free, D1 and SQLite Durable Objects (free plan) work without a card for this flow? | Assumed yes; confirm against current Cloudflare pricing and limits docs. |

### 2.4 Evidence (to be filled in by the maintainer)

| Field | Value |
| --- | --- |
| Validation status | NOT YET PERFORMED |
| Date performed | |
| GitHub account type (personal / org) and plan | |
| Cloudflare plan and payment method on file (should be none) | |
| OAuth app registered without a payment prompt (yes/no) | |
| Sign-in completed end to end (yes/no) | |
| Numeric `id` returned with no scope (yes/no) | |
| PKCE supported (yes/no) | |
| Second account (non-owner) could authorize the app (yes/no) | |
| Deviations from this document | |
| Result (pass / fail / partial) | |
| Performed by | |

Do not paste client IDs, secrets, tokens, cookies or screenshots containing them into this table.

---

## 3. Owner bootstrap

Goal: exactly one owner per workspace, established without any credential in the repository, and no second bootstrap possible.

### 3.1 Mechanism (Proposed, needs maintainer confirmation)

1. The operator sets a Worker secret `STUDIO_BOOTSTRAP_TOKEN` (a high-entropy random value, generated locally, never committed) with `wrangler secret put`. It is single-use by design.
2. The operator opens `https://studio.example.workers.dev/bootstrap` and supplies the bootstrap token plus signs in with GitHub. The Worker verifies the token in constant time, completes the OAuth flow, reads the numeric GitHub user ID, and in **one D1 transaction** (a single `batch`) inserts the workspace, the user, and the `owner` membership, and writes a `bootstrap_state` row marking bootstrap complete.
3. After success the bootstrap route returns `404` permanently, and the operator should delete the secret (`wrangler secret delete STUDIO_BOOTSTRAP_TOKEN`). The Worker does not rely on the secret being deleted.

Alternative considered: first-GitHub-user-to-sign-in-wins. Rejected because anyone who finds the URL first becomes owner. Another alternative: a pre-declared GitHub numeric ID in a binding (`STUDIO_OWNER_GITHUB_ID`) with no token. This is simpler and has no race, but requires the operator to know their numeric ID first. Both are acceptable; the maintainer should pick one (open question 1).

### 3.2 Preventing a second bootstrap (fail closed)

- A `bootstrap_state` table (or a unique constraint, see §9) allows at most one completed bootstrap per workspace. The insert is conditional (`INSERT ... WHERE NOT EXISTS`) and its failure aborts the whole transaction.
- If the registry is unreachable, unreadable, or in an unexpected state, bootstrap **refuses** (fail closed). Absence of a readable `bootstrap_state` row is never treated as "not bootstrapped" without a successful, explicit read.
- Bootstrap is rate-limited and logs failed attempts without logging the supplied token.
- The bootstrap token is compared in constant time and is never stored in D1; only completion is recorded.
- Ownership changes after bootstrap go through the owner-transfer path in §4, not the bootstrap route.

---

## 4. Roles and invitations

### 4.1 Roles (Proposed, needs maintainer confirmation)

The issues imply an owner, collaborators, and edit-only versus publish-capable credentials. Proposed role set, scoped to a workspace (and, where noted, to a site within it):

| Capability | owner | admin | publisher | editor | viewer |
| --- | --- | --- | --- | --- | --- |
| View sites and live presence | yes | yes | yes | yes | yes |
| Edit site content (commands over WebSocket/HTTP) | yes | yes | yes | yes | no |
| Publish (upload/compile and mark a revision publishable) | yes | yes | yes | no | no |
| Activate production | yes | yes | yes | no | no |
| Invite collaborators | yes | yes (not above own role) | no | no | no |
| Remove collaborators | yes | yes (not owners or admins) | no | no | no |
| Change roles | yes | limited (below admin) | no | no | no |
| Create/rotate/revoke own CLI/MCP credentials | yes | yes | yes | yes (edit scope only) | no |
| Revoke other users' credentials | yes | yes | no | no | no |
| Rotate OAuth secret / bootstrap / transfer ownership / delete workspace | yes | no | no | no | no |

Rules:

- Exactly one `owner` per workspace at a time. The owner cannot be removed or demoted except by an explicit transfer to an existing member, which is a single transaction that also demotes the previous owner.
- A user can never grant a role higher than their own.
- A credential's effective capability is the **intersection** of its scopes (§7) and the creator's *current* role; demoting a user immediately shrinks their credentials.
- The maintainer may collapse `admin` or `viewer` if they are not wanted at launch (open question 2).

### 4.2 Invitations

**Issuance.** An authorized inviter creates an invitation for a role and **a GitHub login to be claimed**, optionally an email as a label only. Inviting by login string is only a hint (logins can be renamed); see binding below.

**Token.** The invitation is represented by a random single-use token (at least 128 bits, Proposed: 256) delivered out of band by the inviter as a URL (`https://studio.example.workers.dev/invite/<token>`). Only a hash of the token is stored (§7.3). The Studio never sends email in v0.2.

**Binding to a GitHub identity.**

1. The invitation stores the expected GitHub login for display and, if known, the numeric GitHub ID.
2. On acceptance the invitee signs in with GitHub. The Worker reads the numeric `id` and `login` from the authenticated response, never from the URL.
3. If the invitation was created with a numeric ID it must match exactly. If it was created with a login only, the first successful acceptance whose login case-insensitively matches **binds** the invitation to that numeric ID, and the membership is keyed on the numeric ID from then on. The login is stored only as a display field.
4. A mismatching identity is rejected with a generic error that does not reveal who the invitation was for.
5. Acceptance is a single D1 transaction: consume the invitation (conditional update on unused, unexpired, unrevoked), create the user if absent, and create the membership. Replays fail.

**Expiry (Proposed):** 7 days from issuance. **Max outstanding invitations per workspace (Proposed):** 20. **Max pending per login (Proposed):** 1.

**Revocation.** An inviter with the right to invite, or the owner, may revoke a pending invitation at any time; revocation is effective immediately for acceptance. Accepted invitations are not revoked; the member is removed instead (§6.3). Expired and revoked invitation rows are retained for audit for a bounded period (Proposed: 90 days), then purged.

**Removal of a collaborator.** Deleting the membership is the revocation event: it invalidates sessions, credentials and live sockets as described in §5.5 and §7.

---

## 5. Session lifecycle

### 5.1 OAuth flow and creation

Authorization-code flow against GitHub (endpoints `https://github.com/login/oauth/authorize` and `https://github.com/login/oauth/access_token`; re-verify against current docs).

1. `GET /auth/github/login`: the Worker generates a random `state` (at least 128 bits), and, if PKCE is confirmed supported (§2.3), a code verifier. It stores `state` (and verifier) server-side with a short TTL (Proposed: 10 minutes, single use) and also binds it to the browser with a short-lived cookie. It redirects to GitHub with `client_id`, `redirect_uri`, `state`, and the minimal scope.
2. `GET /auth/github/callback`: the Worker checks that `state` matches the stored and cookie values (constant time), is unexpired and unused, then consumes it. A mismatch or replay returns `400` and creates no session.
3. The Worker exchanges the code for a token (server-side, with the client secret), calls `GET https://api.github.com/user`, takes the numeric `id` and `login`, then **discards the GitHub access token** (not persisted, not logged). Studio does not need to call GitHub on the user's behalf.
4. **Scope (Proposed):** request no scope (public profile only). Do not request `user:email` or `repo`. If email is ever needed it is a separate decision.
5. The Worker requires an existing membership for the numeric ID (or a valid invitation being accepted, or the bootstrap path). A GitHub user with no membership gets a generic denial and no session.
6. A new session row is created with a fresh random identifier. Any pre-existing session cookie is discarded (session fixation defence).

### 5.2 Cookie attributes (Proposed)

- Name: `__Host-solo_studio_session` (the `__Host-` prefix requires `Secure`, `Path=/`, and no `Domain`).
- `HttpOnly`, `Secure`, `SameSite=Lax` (Lax is needed so the top-level OAuth redirect back carries the `state` cookie; the session cookie itself can be set after the callback as `Strict` if the maintainer prefers, see open question 5).
- The value is an opaque random identifier. Only its hash is stored server-side (§9). No JWT, no user data in the cookie.
- Responses on auth routes carry the same security headers as all Worker responses (AGENTS.md §4) and `Cache-Control: no-store`.

### 5.3 CSRF

- Any state-changing HTTP route requires the session cookie **and** either a per-session CSRF token header or a strict `Origin` check against the Studio origin (Proposed: both). `GET` routes do not mutate state.
- The WebSocket upgrade validates `Origin` against the Studio origin and the session before upgrading; cross-site WebSocket hijacking is rejected.
- CLI/MCP credentials are bearer tokens sent in an `Authorization` header and are never accepted from cookies or query strings.

### 5.4 Expiry, renewal, logout

All values below are **Proposed (needs maintainer confirmation)**:

| Setting | Proposed value |
| --- | --- |
| Idle timeout | 24 hours |
| Absolute lifetime | 14 days, then full re-authentication |
| Renewal | Sliding: `last_seen_at` extended at most once per 15 minutes to bound D1 writes (#61 forbids per-edit checkpoint writes; apply the same discipline to session touches) |
| Logout | `POST /auth/logout` deletes the session row, clears the cookie, and closes that session's sockets |
| Re-auth for sensitive actions | Owner-only actions (secret rotation, ownership transfer, workspace delete) require a session younger than 15 minutes (Proposed) |

Expired sessions are rejected on every request by comparing stored timestamps to the server clock; purging is housekeeping only and is never required for correctness.

### 5.5 Revocation and WebSocket / Durable Object behaviour

Requirements for #62 and #65:

1. **Authority.** Authorization is decided from D1 (membership role, session validity, credential validity), not from anything cached in the cookie or in a hibernated attachment.
2. **Attachment contents.** A WebSocket attachment may hold only identity (session or credential ID, user ID, role-at-connect, `authz_checked_at`). It is a hint, never an authorization decision. Restoring it after hibernation restores identity only.
3. **Recheck before edits.** Before applying any command that mutates site state, the Durable Object must have a recent authorization check. Proposed policy: recheck against D1 if `authz_checked_at` is older than 30 seconds, and **always** recheck before publish, activation, invitation, and membership or credential changes (no cache). If D1 is unavailable the check fails closed and the edit is rejected, not buffered.
4. **Revocation signal.** On removal of a collaborator, session deletion, role change, or credential revocation, the control plane notifies the affected site Durable Objects (Proposed: an internal RPC to close matching sockets by user ID or credential ID). On receipt the DO closes those sockets with a defined close code and stops sending updates.
5. **Bounded delay.** Because the push signal can be lost (DO evicted, network failure), delivery must also stop through the pull path: every socket is re-authorized on its next inbound message, on wake from hibernation, and by a periodic alarm. **Proposed maximum delay for update delivery to stop after revocation: 60 seconds in the worst case (alarm-driven), typically under 5 seconds (push).** This is a design target; #65 and #62 must document the final number and test it, including a case that hibernates the DO before revocation.
6. **After hibernation.** On wake, before sending any outbound broadcast to a restored socket, the DO re-validates that socket's identity (so a revoked user does not receive the first post-wake update). This is a stricter requirement than rechecking on edits and is called out for #62.
7. **Soft presence** entries for a removed user expire immediately on revocation rather than waiting for their TTL.

---

## 6. Recovery

### 6.1 Lost owner access

Cases: owner loses their GitHub account or 2FA, or the GitHub account is suspended.

- Recovery path (Proposed): an operator with Cloudflare account access can re-run a **break-glass** procedure: set a new secret `STUDIO_RECOVERY_TOKEN` with `wrangler secret put`, then call a recovery route that, with the token and a fresh GitHub sign-in, reassigns the owner membership to the new numeric ID. The route logs an audit event, revokes all existing sessions and credentials, and is rate-limited.
- This trades security for availability: anyone with Cloudflare account access can take over the workspace. That is inherent in self-hosting and should be stated plainly to operators. Protect the Cloudflare account with 2FA.
- Without Cloudflare access and without GitHub access there is no recovery by design.

### 6.2 Rotated or compromised OAuth client secret

1. Generate a second client secret in GitHub (assumed possible, verify), `wrangler secret put` the new value, deploy, confirm sign-in, then delete the old secret in GitHub.
2. If the secret may have been used by an attacker: delete it in GitHub first (users cannot sign in briefly), then deploy the replacement. Existing Studio sessions are independent of the OAuth secret, so also consider forcing a global session revocation (Proposed: owner action "revoke all sessions").
3. Never log or echo the secret during rotation.

### 6.3 Removed collaborator

Removal deletes the membership and, in the same transaction, deletes the user's sessions and revokes the credentials they created. Sockets are closed per §5.5. The user's authored history and audit rows are retained with the numeric ID.

### 6.4 GitHub account rename or deletion

- **Rename:** memberships are keyed on the numeric ID, so rename changes nothing except the displayed login, refreshed at next sign-in. This is why login-only keying is rejected.
- **Deletion or suspension:** the user can no longer sign in. Their sessions end by expiry; the owner should remove the member to revoke credentials immediately. A deleted GitHub login might later be re-registered by someone else with the same login but a **different numeric ID**, which does not match any membership, so name squatting does not grant access.
- Re-verify that GitHub never reuses numeric user IDs.

---

## 7. Scoped CLI / MCP credentials (for #65 and #68)

### 7.1 Scopes (Proposed names)

| Scope | Allows | Notes |
| --- | --- | --- |
| `site:read` | Pull site state and snapshots | |
| `site:edit` | Push edit commands through the shared reducer | Cannot publish or activate production, ever |
| `site:publish` | Submit a revision-pinned artifact manifest and upload checked artifacts for staging/publish | Required by #68 for CLI-built artifacts; does not imply activation |
| `site:activate` | Activate a published revision to production | Separate from publish; recommended to require a short-lived credential |

Hard rules:

- **`site:edit` alone can never publish or activate production**, enforced server-side on the route that performs the action, not only in the CLI.
- Scopes are not hierarchical by implication; a credential carries an explicit list. `site:publish` does not imply `site:edit`.
- A credential is bound to one workspace and an explicit list of site IDs (or an explicit "all sites in this workspace"). It is never valid across workspaces (tenant scoping).
- Effective permission is the intersection of scopes and the creator's current role (§4.1). Capabilities that role cannot hold cannot be minted (an `editor` cannot create a `site:publish` credential).

### 7.2 Issuance

- Created in the authenticated Studio UI (or by an authenticated session via API), never by an unauthenticated route. Re-authentication recency applies for `site:publish` and `site:activate` (Proposed: session younger than 15 minutes).
- The plaintext token is shown **once** at creation. Format (Proposed): a recognisable prefix plus a random body, e.g. `solo_pat_` followed by at least 256 bits of base64url entropy, so secret scanners can detect leaks.
- The CLI stores it outside the repository (OS keychain where available, otherwise a mode-600 file in the user config directory) and reads it from an environment variable in CI. The CLI refuses to write it into a project directory.

### 7.3 Storage as hashes only

- Only a hash is stored (Proposed: SHA-256 of the full token, which is acceptable for high-entropy random tokens, unlike passwords) plus a short non-secret prefix identifier for lookup and display.
- Plaintext is never persisted in D1, a Durable Object, logs, or error messages. Lookup is by prefix identifier followed by constant-time comparison of the hash.

### 7.4 Expiry, rotation, revocation

All numbers **Proposed (needs maintainer confirmation)**:

| Setting | Proposed |
| --- | --- |
| Default TTL `site:read`/`site:edit` | 30 days |
| Default TTL `site:publish` | 7 days |
| Default TTL `site:activate` | 24 hours |
| Maximum TTL (any) | 90 days |
| Credentials per user | 10 active |

- **Rotation:** creating a replacement and revoking the old one are separate steps; an optional overlap window (Proposed: up to 24 hours) lets a CI pipeline cut over without downtime. A rotation endpoint issues a new token with the same scopes and marks the old one with an earlier expiry.
- **Revocation:** immediate. Sets `revoked_at`; the next request fails and live sockets using that credential are closed per §5.5.
- Revocation of a credential is also triggered by removing its creator, demoting them below the credential's required role, or the owner's "revoke all" action.
- Every use updates `last_used_at` at a bounded rate (not per request), to give owners visibility without per-request D1 writes.

### 7.5 Redaction and tenant scoping

- Tokens, session identifiers, OAuth `code`, `state`, client secret and `Authorization`/`Cookie` headers are redacted from logs, error bodies, panic messages and telemetry. Use a single redaction helper and test it (#65: "safe credential redaction").
- Error responses for a bad or foreign credential are uniform (`401`/`403`) and do not reveal whether a credential or site exists in another tenant.
- Every D1 query that touches tenant data binds the workspace ID derived from the authenticated principal, never from a client-supplied value (#61 tenant scoping). All queries use bound parameters (AGENTS.md §3).
- MCP mutations use the same server-side authorization as the CLI; the MCP server is not a trusted bypass.

---

## 8. Secrets handling

Consistent with AGENTS.md §8: secrets live in `.dev.vars` (gitignored), `private/*.env` (gitignored, mode 600) or `wrangler secret put`. They are never committed and never printed. `wrangler.jsonc` and `.dev.vars.example` carry placeholders only.

Names below are **Proposed** for #61 and #65:

| Name | Kind | Purpose |
| --- | --- | --- |
| `GITHUB_OAUTH_CLIENT_ID` | binding var (non-secret, per environment, kept out of tenant-agnostic code) | OAuth app client ID |
| `GITHUB_OAUTH_CLIENT_SECRET` | secret | OAuth code exchange |
| `STUDIO_BOOTSTRAP_TOKEN` | secret (temporary) | One-shot owner bootstrap (§3) |
| `STUDIO_RECOVERY_TOKEN` | secret (temporary) | Break-glass owner recovery (§6.1); unset when unused |
| `STUDIO_SESSION_HMAC_KEY` | secret | Keyed hashing of session identifiers and CSRF tokens (so D1 contents alone cannot be replayed) |
| `STUDIO_PUBLIC_ORIGIN` | binding var | Exact Studio origin for redirect URI and `Origin` checks |
| `DB` | D1 binding | Studio registry (separate from the visitor Worker's DB, per #61) |

Rules:

- Local development uses the separate dev OAuth app and a dev-only `.dev.vars`; the always-pass Turnstile test keys are for public forms only and have no role in Studio auth.
- The Studio Worker's configuration is separate from the visitor Worker (#61). The visitor Worker holds no OAuth secret.
- A missing secret makes the dependent route fail closed with a generic `503`, never a default value.
- CI uses no real OAuth secret; tests run against a fake GitHub endpoint.

---

## 9. Hand-off to #61 / #62 / #65 / #68

### 9.1 For #61 (Studio Worker and D1 registry)

1. Tables (names Proposed): `workspaces`, `sites`, `users`, `memberships`, `invitations`, `sessions`, `credentials`, `bootstrap_state`, `audit_events`, `oauth_states` (or store `state` in a DO/KV-free short-lived row).
2. `users`: internal ID, `github_id` (INTEGER, **UNIQUE**, immutable key), `github_login` (display only, updated at sign-in), `created_at`. No GitHub access token column. No email column unless the maintainer decides otherwise.
3. `memberships`: `workspace_id`, `user_id`, `role` (CHECK against the role set), `created_at`; unique on (`workspace_id`, `user_id`). A partial unique index guarantees at most one `owner` per workspace.
4. `bootstrap_state`: `workspace_id` primary key, `completed_at`, `owner_user_id`; insert-once semantics.
5. `invitations`: `id`, `workspace_id`, `role`, `token_hash`, `expected_github_login`, `bound_github_id` (nullable until accepted), `created_by`, `created_at`, `expires_at`, `accepted_at`, `revoked_at`.
6. `sessions`: `id_hash`, `user_id`, `workspace_id`, `created_at`, `last_seen_at`, `expires_at`, `revoked_at`. No plaintext identifier.
7. `credentials`: `id`, `prefix`, `token_hash`, `workspace_id`, `created_by`, `scopes`, `site_ids` (or a join table), `created_at`, `expires_at`, `last_used_at`, `revoked_at`, `rotated_from`.
8. Every tenant table carries `workspace_id`; migrations include tests that a query for workspace A never returns workspace B rows.
9. All queries bound-parameter; session/credential touch writes are rate-limited and not per-edit; D1 checkpoint writes do not occur per edit (as #61 already requires).
10. Strict request bounds and the dual security headers apply to every Studio route (AGENTS.md §4, §3).

### 9.2 For #62 (Durable Object sessions)

1. Attachments hold identity only (§5.5.2); authorization state is re-derived from D1.
2. Recheck authorization before applying edits using the policy in §5.5.3; always recheck for publish and activation paths.
3. Expose a revocation entry point (close sockets by user ID or credential ID) callable only by the Studio Worker.
4. Re-validate restored sockets before the first outbound broadcast after hibernation (§5.5.6).
5. Re-authorize on a periodic, retry-safe alarm so delivery stops within the documented bound even if the push signal is lost.
6. Presence entries for a removed identity are dropped on revocation.
7. Integration tests: removal while connected, removal while hibernated, credential revocation, and measurement of the maximum delivery-stop delay.

### 9.3 For #65 (auth implementation)

1. Implement the flow in §5.1 with `state` (and PKCE if confirmed), minimal scope, and no persisted GitHub token.
2. Implement bootstrap (§3) with fail-closed second-bootstrap prevention and a test that a second attempt fails, including under concurrent attempts.
3. Implement roles (§4.1), invitation binding to the numeric ID (§4.2), expiry and revocation, with tests for replay, wrong identity, expired, and revoked invitations.
4. Implement session cookie, CSRF and `Origin` checks, expiry and logout (§5.2 to §5.4).
5. Implement scoped credentials (§7) with the `edit` cannot `publish` or `activate` invariant enforced on the server, tested for every publish and activation route.
6. Protect HTTP and WebSocket paths; test tenant denial (cross-workspace access returns uniform errors).
7. Document and test the maximum revocation-propagation delay (§5.5.5), including after hibernation.
8. Test redaction of secrets in logs and errors (§7.5).
9. Apply Turnstile to public forms and document bootstrap without credentials in the repository (the second is satisfied by §3 and §8).
10. Do not introduce Cloudflare Access, a card requirement, or an auth SaaS as a default dependency.

### 9.4 For #68 (CLI and MCP)

1. `solo studio` commands read the credential from the OS keychain, a mode-600 user config file, or an environment variable, never from the project directory; the CLI never prints it.
2. Pull and push use `site:edit` (and `site:read`); `publish` uses a `site:publish` credential and submits a revision-pinned artifact manifest; production activation, if exposed in the CLI, needs `site:activate` and is a distinct, separately reported step.
3. Surface "forbidden: credential lacks scope" distinctly from "unauthenticated" and from revision conflicts.
4. MCP mutations go through the same server-side authorization as the CLI.
5. CLI output and logs redact tokens; test with a canary token.

---

## 10. Open questions

1. Bootstrap method: single-use `STUDIO_BOOTSTRAP_TOKEN` (proposed) versus a pre-declared numeric owner ID binding?
2. Is the five-role set (`owner`, `admin`, `publisher`, `editor`, `viewer`) wanted, or should v0.2 ship with fewer (for example `owner`, `editor`, `viewer`)?
3. Are the proposed session lifetimes, invitation expiry (7 days), credential TTLs and the 60-second worst-case revocation delay acceptable?
4. Is the break-glass recovery route (§6.1) acceptable, given anyone with Cloudflare account access can use it?
5. `SameSite=Lax` versus re-issuing a `Strict` cookie after the OAuth callback?
6. Does PKCE work for OAuth apps (to be confirmed in §2.3), or should a GitHub App be reconsidered? #60 specifies an OAuth app, so this is only relevant if verification fails.
7. Is one workspace per Worker deployment the launch model, or must a single deployment host several workspaces (affects `workspace_id` scoping and bootstrap)?
8. Should invitations be deliverable by email later, and if so through which provider (not a launch dependency)?
9. Free-tier limits for Durable Object requests, D1 rows read/written and Worker requests under sign-in plus presence load are unverified assumptions; who validates them and when?
10. Is an audit log (`audit_events`) required for v0.2 or deferred?
