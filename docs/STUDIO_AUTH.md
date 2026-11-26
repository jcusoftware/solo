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

Turnstile (AGENTS.md §8) stays mandatory on every **public** form. It is separate from Studio authentication: Studio sign-in is protected by OAuth `state` and rate limiting (§5.6), not by Turnstile on the OAuth redirect.

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
5. Note the client ID (not a secret, but the real value is never committed). Set it as the `GITHUB_OAUTH_CLIENT_ID` Worker `vars` entry in the operator's own gitignored config override, as defined in §8; the committed config carries only an `.example` placeholder.
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
| Does a D1 `batch()` run as one atomic transaction in which a constraint violation (for example a PRIMARY KEY or UNIQUE conflict) rolls back every statement, while an `UPDATE` or `INSERT ... SELECT` matching zero rows is a silent success? | Believed yes for both; **verify against current D1 docs**. The design (§3.2, §4.2, §6.1) depends on the constraint-violation behaviour and does not rely on a zero-row statement failing. D1 has no interactive transactions. |
| Does D1 read replication (if enabled) serve reads that can lag the primary? | Believed yes; **verify against current D1 docs**. See §5.5.8. |
| Is the Workers Rate Limiting binding available on the Free plan, and is it per-location and approximate? | Unknown; **verify against current Cloudflare docs**. See §5.6 for the D1/Durable Object fallback. |
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

1. The operator sets a Worker secret `STUDIO_BOOTSTRAP_TOKEN` (a high-entropy random value, generated locally, never committed) with `wrangler secret put`. It is single-use by design. A value shorter than the minimum (Proposed: 32 random bytes, at least 43 base64url characters) is treated as unset: the route fails closed (see §3.2).
2. The operator opens `https://studio.example.workers.dev/bootstrap` and submits the bootstrap token in a **`POST` body** (never a path or query string, so it cannot reach access logs or `Referer`). The Worker verifies the token in constant time, then creates an `oauth_states` row with `purpose = 'bootstrap'` and `secret_verified = 1`, and redirects to GitHub (§5.1). The callback dispatches to bootstrap **only** from that server-side row, never from a callback parameter.
3. On the callback the Worker reads the numeric GitHub user ID and runs one D1 `batch` (atomic; see §2.3 to verify) whose **first** statement inserts the singleton `bootstrap_state` row (`id INTEGER PRIMARY KEY CHECK (id = 1)`, `completed_at`, `owner_github_id`). A second bootstrap hits a PRIMARY KEY violation on that first statement, so the whole batch rolls back and no workspace, user or membership is written. The remaining statements (insert workspace, upsert user, insert `owner` membership) follow, with IDs generated by the Worker beforehand.
4. After success the bootstrap route returns `404` permanently, and the operator should delete the secret (`wrangler secret delete STUDIO_BOOTSTRAP_TOKEN`). The Worker does not rely on the secret being deleted.

Alternative considered: first-GitHub-user-to-sign-in-wins. Rejected because anyone who finds the URL first becomes owner. Another alternative: a pre-declared GitHub numeric ID in a binding (`STUDIO_OWNER_GITHUB_ID`) with no token. This is simpler and has no race, but requires the operator to know their numeric ID first. Both are acceptable; the maintainer should pick one (open question 1).

### 3.2 Preventing a second bootstrap (fail closed)

- **Why a singleton and not a conditional insert.** In a D1 batch a statement that matches zero rows (an `INSERT ... SELECT ... WHERE NOT EXISTS`, or an `UPDATE`) is a silent no-op, not an error, so the rest of the batch would still commit. Only a real constraint violation or raised error rolls the batch back. The guard is therefore a `bootstrap_state` singleton row (`id INTEGER PRIMARY KEY CHECK (id = 1)`) inserted **first** in the batch, so a second bootstrap fails with a PK violation and nothing else is written.
- **Deployment-wide, not per workspace.** Bootstrap completion is recorded once per deployment, not per `workspace_id`. Once that row exists the bootstrap route returns `404` regardless of workspace or of whether the secret is still set, so a leaked or retained token cannot create and own a second workspace. This holds until open question 7 (multiple workspaces per deployment) is decided; any multi-workspace model must define a new, separately authorized workspace-creation path rather than reuse bootstrap.
- If the registry is unreachable, unreadable, or in an unexpected state, bootstrap **refuses** (fail closed). Absence of a readable `bootstrap_state` row is never treated as "not bootstrapped" without a successful, explicit read from the **primary** (§5.5.8).
- The bootstrap route returns `404` (not `503`) when the secret is unset or shorter than the minimum length, so an unconfigured deployment does not advertise the route.
- Bootstrap is rate-limited (§5.6) and logs failed attempts without logging the supplied token.
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
| Invite collaborators | yes (any role except `owner`) | yes (roles below `admin` only) | no | no | no |
| Remove collaborators | yes | yes (members below `admin` only) | no | no | no |
| Change roles | yes | limited (below admin) | no | no | no |
| Create/rotate/revoke own CLI/MCP credentials | yes | yes | yes | yes (edit scope only) | no |
| Revoke other users' credentials | yes | yes | no | no | no |
| Rotate OAuth secret / bootstrap / transfer ownership / delete workspace | yes | no | no | no | no |

Rules:

- Exactly one `owner` per workspace at a time. The owner cannot be removed or demoted except by an explicit transfer to an existing member, which is a single transaction that also demotes the previous owner.
- A user can never grant a role higher than their own. Admin invite and remove rights are symmetric: an admin can invite and remove only roles below `admin`; only the owner invites or removes admins.
- `owner` is never a valid invitation role (one-owner rule); a database `CHECK` on `invitations.role` rejects it. Ownership moves only by explicit transfer (above) or the recovery path (§6.1).
- A credential's effective capability is always evaluated as the **intersection** of its scopes (§7) and the creator's *current* role. **Proposed:** in addition, demoting or removing a user **revokes** all credentials they created, in the same operation (not merely narrows them), so the revoked and narrowed behaviours cannot diverge. The at-use intersection stays as defence in depth against races.
- The maintainer may collapse `admin` or `viewer` if they are not wanted at launch (open question 2).

### 4.2 Invitations

**Issuance.** An authorized inviter creates an invitation for a role and **a GitHub login to be claimed**, optionally an email as a label only. Inviting by login string is only a hint (logins can be renamed); see binding below.

**Token.** The invitation is represented by a random single-use token (at least 128 bits, Proposed: 256) delivered out of band by the inviter as a URL (`https://studio.example.workers.dev/invite/<token>`). Only a hash of the token is stored (§7.3). The Studio never sends email in v0.2.

**Token in the URL path (Proposed).** A path token can leak through access logs, browser history and `Referer`. Mitigations: (a) on the first `GET /invite/<token>` the Worker verifies the token hash, creates an `oauth_states` row (`purpose = 'invitation'`, `invitation_id`, and the verified token hash), sets the browser-binding cookie, and immediately redirects with `303` to GitHub (§5.1), so the token is never rendered into a page or sent onward; after that point only the server-side reference is used. (b) The route responds with `Cache-Control: no-store` and `Referrer-Policy: no-referrer`. (c) The request path for `/invite/*` is redacted in all logs and telemetry (§7.5). (d) Invite hits are rate-limited (§5.6).

**Binding to a GitHub identity.**

1. The invitation stores the expected GitHub login for display and, if known, the numeric GitHub ID.
2. On acceptance the invitee signs in with GitHub. The Worker reads the numeric `id` and `login` from the authenticated response, never from the URL.
3. If the invitation was created with a numeric ID it must match exactly. If it was created with a login only, the first successful acceptance whose login case-insensitively matches **binds** the invitation to that numeric ID, and the membership is keyed on the numeric ID from then on. The login is stored only as a display field.
4. A mismatching identity is rejected with a generic error that does not reveal who the invitation was for.
5. **Acceptance must not rely on a failing `UPDATE`.** D1 has no interactive transactions, and in a `batch` an `UPDATE` that matches zero rows is not an error, so a following `INSERT` would still commit. Acceptance is therefore one `batch` in which the **membership insert is itself conditional on the invitation being valid**:

   ```sql
   INSERT INTO memberships (workspace_id, user_id, role, created_at)
   SELECT i.workspace_id, ?user_id, i.role, ?now
   FROM invitations i
   WHERE i.id = ?1 AND i.token_hash = ?2
     AND i.accepted_at IS NULL AND i.revoked_at IS NULL AND i.expires_at > ?now
     AND (i.bound_github_id = ?gid
          OR (i.bound_github_id IS NULL AND lower(i.expected_github_login) = lower(?login)))
     AND EXISTS (SELECT 1 FROM memberships m
                 WHERE m.workspace_id = i.workspace_id AND m.user_id = i.created_by
                   AND (m.role = 'owner' OR (m.role = 'admin' AND i.role IN ('publisher','editor','viewer'))));
   ```

   The role comes from the invitation row, never from the request. The statement order is: upsert `users` (a user row without a membership grants nothing), the conditional membership insert above, then `UPDATE invitations SET accepted_at = ?now, bound_github_id = ?gid` with the same predicate. An expired, revoked, replayed or wrong-identity token matches no row, so **no membership is created**. The Worker also checks that both statements reported exactly one changed row and otherwise returns the generic error. Optionally (defence in depth) a trigger using `RAISE(ABORT)` or a `UNIQUE` consumed-marker row can make a failed consume raise instead of no-op.
6. **Inviter authority is rechecked.** The final `EXISTS` clause above rechecks, at acceptance time, that the inviter still holds a role allowed to grant the invited role. In addition, removing or demoting a user revokes all of their pending invitations in the same operation (§6.3).
7. **Acceptance by an existing member is an error.** If the signed-in identity already has a membership in the workspace, the `UNIQUE (workspace_id, user_id)` violation aborts the batch (invitation stays unconsumed) and the Worker returns a generic error. Acceptance never changes an existing member's role; role changes go through §4.1.

**Expiry (Proposed):** 7 days from issuance. **Max outstanding invitations per workspace (Proposed):** 20. **Max pending per login (Proposed):** 1.

**Revocation.** An inviter with the right to invite (within the symmetric rules of §4.1), or the owner, may revoke a pending invitation at any time; revocation is effective immediately for acceptance. Accepted invitations are not revoked; the member is removed instead (§6.3). Expired and revoked invitation rows are retained for audit for a bounded period (Proposed: 90 days), then purged.

**Removal of a collaborator.** Deleting the membership is the revocation event: it invalidates sessions, credentials and live sockets as described in §5.5 and §7.

---

## 5. Session lifecycle

### 5.1 OAuth flow and creation

Authorization-code flow against GitHub (endpoints `https://github.com/login/oauth/authorize` and `https://github.com/login/oauth/access_token`; re-verify against current docs).

1. **Entry points create the `oauth_states` row.** Every flow starts at a server route that records its intent server-side: `GET /auth/github/login` (`purpose = 'login'`), `POST /bootstrap` (`'bootstrap'`, after the token check, §3.1), `POST /recovery` (`'recovery'`, after the token check, §6.1) and `GET /invite/<token>` (`'invitation'`, §4.2). Each Worker-generated row stores a random `state` (at least 128 bits), `purpose`, `invitation_id` (invitation flow only), a `secret_verified` flag ("bootstrap/recovery token verified"), the optional PKCE verifier (if confirmed supported, §2.3), the post-login `return_to` (below), and a short TTL (Proposed: 10 minutes, single use). The state is also bound to the browser with a short-lived cookie. The Worker redirects to GitHub with `client_id`, `redirect_uri`, `state` and the minimal scope. **`redirect_uri` is fixed** to `STUDIO_PUBLIC_ORIGIN + /auth/github/callback` with no added parameters, so purpose cannot be carried or altered through the redirect.
2. `GET /auth/github/callback`: the Worker checks that `state` matches the stored and cookie values (constant time), is unexpired and unused, then consumes it. A mismatch or replay returns `400` and creates no session. **The callback dispatches only from the stored row** (`purpose`, `invitation_id`, `secret_verified`); it ignores any `purpose`, `invitation`, `next` or similar callback parameter, and a bootstrap or recovery purpose without `secret_verified = 1` is rejected.
3. The Worker exchanges the code for a token (server-side, with the client secret), calls `GET https://api.github.com/user`, takes the numeric `id` and `login`, then **discards the GitHub access token** (not persisted, not logged). **Proposed (needs maintainer confirmation; not specified by #60):** discarding is chosen because Studio does not need to call GitHub on the user's behalf and a stored token would be an extra theft target.
4. **Scope (Proposed):** request no scope (public profile only). Do not request `user:email` or `repo`. If email is ever needed it is a separate decision.
5. The Worker requires an existing membership for the numeric ID (or a valid invitation being accepted, or the bootstrap path). A GitHub user with no membership gets a generic denial and no session.
6. A new session row is created with a fresh random identifier. Any pre-existing session cookie is discarded (session fixation defence).
7. **Post-login redirect.** The destination is a same-origin **relative path** (it must start with a single `/`, not `//` or `/\`, and contain no scheme or host), validated and stored as `return_to` in the `oauth_states` row at step 1, defaulting to `/`. The callback never reads a redirect target from its own query string; an invalid stored value falls back to `/`.

### 5.2 Cookie attributes (Proposed)

- Name: `__Host-solo_studio_session` (the `__Host-` prefix requires `Secure`, `Path=/`, and no `Domain`).
- `HttpOnly`, `Secure`, `SameSite=Lax` (Lax is needed so the top-level OAuth redirect back carries the `state` cookie; the session cookie itself can be set after the callback as `Strict` if the maintainer prefers, see open question 5).
- The value is an opaque random identifier. Only its keyed hash (HMAC-SHA-256 with `STUDIO_SESSION_HMAC_KEY`) is stored server-side (§9); why sessions use HMAC while credentials use plain SHA-256 is explained in §7.3. No JWT, no user data in the cookie.
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

Expired sessions are rejected on every request by comparing stored timestamps to the server clock; purging is housekeeping only and is never required for correctness. Session validity also requires a live membership row (joined at lookup), so a removed member's session is dead even if a cleanup statement did not run. Session, membership and revocation lookups read from the D1 primary (§5.5.8).

### 5.5 Revocation and WebSocket / Durable Object behaviour

Requirements for #62 and #65:

1. **Authority.** Authorization is decided from D1 (membership role, session validity, credential validity), not from anything cached in the cookie or in a hibernated attachment.
2. **Attachment contents (Proposed; #62 owns the final shape).** A WebSocket attachment may hold only identity (session or credential ID, user ID, role-at-connect, `authz_checked_at`). It is a hint, never an authorization decision. Restoring it after hibernation restores identity only.
3. **Recheck before edits.** Before applying any command that mutates site state, the Durable Object must have a recent authorization check. Proposed policy: recheck against D1 if `authz_checked_at` is older than 30 seconds. For a credential-authenticated socket the recheck evaluates **scope ∩ the creator's current role** (and that the credential is unrevoked, unexpired and bound to this site), not mere validity, so a demoted creator or a scope that no longer includes `site:edit` is refused. If D1 is unavailable the check fails closed and the edit is rejected, not buffered.
3a. **Privileged actions are never accepted over the edit channel.** Publish, activation, invitation, and membership or credential management are **not** WebSocket commands. They are accepted only on HTTP routes that enforce §7.1 server-side with a fresh D1 check (no cache) and the re-auth rules of §5.4 and §7.2. A command of that kind arriving on the socket is refused and, for credentials, counted as an anomaly. An edit-only credential therefore cannot publish or activate through the WebSocket.
4. **Revocation signal.** On removal of a collaborator, session deletion, role change, or credential revocation, the control plane notifies the affected site Durable Objects (Proposed: an internal RPC to close matching sockets by user ID or credential ID). On receipt the DO closes those sockets with a defined close code and stops sending updates.
5. **Bounded delay.** Because the push signal can be lost (DO evicted, network failure), delivery must also stop through the pull path: every socket is re-authorized on its next inbound message, on wake from hibernation, and by a periodic alarm. **Proposed maximum delay for update delivery to stop after revocation: 60 seconds in the worst case (alarm-driven), typically under 5 seconds (push).** This is a design target; #65 and #62 must document the final number and test it, including a case that hibernates the DO before revocation.
6. **After hibernation (Proposed; beyond the issue text).** On wake, before sending any outbound broadcast to a restored socket, the DO re-validates that socket's identity (so a revoked user does not receive the first post-wake update). This is a stricter requirement than rechecking on edits and is called out for #62.
7. **Soft presence (Proposed; beyond the issue text)** entries for a removed user expire immediately on revocation rather than waiting for their TTL.
8. **Read from the primary.** Authorization, revocation and bootstrap-state reads (session, membership, credential, invitation, `bootstrap_state`) must be served by the D1 primary. If D1 read replication is enabled for the registry, those queries must use the primary-only/session-consistent path, otherwise replication stays disabled for the registry database, because a lagging replica could accept a revoked session or report bootstrap as incomplete. **Verify against current D1 docs** (§2.3).

### 5.6 Rate limiting

All keys and limits are **Proposed (needs maintainer confirmation)**; #60 only requires that sign-in and bootstrap be rate limited.

| Route | Key | Limit |
| --- | --- | --- |
| `GET /auth/github/login`, `/auth/github/callback` | client IP | 10 per minute |
| `POST /bootstrap`, `POST /recovery` | client IP, and one global key per route | 5 per minute per IP; 10 failures per hour globally, then locked until the operator intervenes |
| `GET /invite/<token>` | client IP | 10 per minute; 5 failed token lookups per hour |
| Credential authentication failures | client IP plus credential prefix | 20 failures per minute |
| Credential and invitation creation | user | 20 per hour |

Mechanism: the Workers Rate Limiting binding is the first choice for the per-IP keys. **Verify against current Cloudflare docs whether it is available on the Workers Free plan**, and note it is approximate and per-location, so it must not be the only control on bootstrap or recovery. **Fallback (and the authoritative counter for the global bootstrap/recovery keys):** a fixed-window counter in D1 (single `INSERT ... ON CONFLICT DO UPDATE` per attempt, primary only) or a per-key SQLite Durable Object. Rate-limit writes are bounded to the failure paths and window rollovers so they respect the free-tier write budget. Over limit returns `429` with `Retry-After` and no detail. If the limiter is unavailable, bootstrap, recovery and invite routes fail closed. The client IP is taken from `CF-Connecting-IP` (verify). #65 implements and tests these.

---

## 6. Recovery

### 6.1 Lost owner access

Cases: owner loses their GitHub account or 2FA, or the GitHub account is suspended.

- Recovery path (Proposed): an operator with Cloudflare account access can re-run a **break-glass** procedure: set a new secret `STUDIO_RECOVERY_TOKEN` with `wrangler secret put`, then `POST` the token **in the request body only** (never a path or query string) to a recovery route. The Worker verifies it in constant time, records `purpose = 'recovery'` and `secret_verified = 1` in the `oauth_states` row (§5.1), and on the callback, with a fresh GitHub sign-in, reassigns the owner membership to the new numeric ID. The route logs an audit event, revokes all existing sessions and credentials, and is rate-limited (§5.6).
- **No reuse.** The first statement of the recovery `batch` inserts the SHA-256 hash of the supplied token into a `consumed_recovery_tokens` table (primary key on the hash). A reused token hits a PK violation and the whole batch rolls back (same reasoning as §3.2: a zero-row no-op would not abort). Rotate the secret to a new value for each recovery.
- The route returns `404` when `STUDIO_RECOVERY_TOKEN` is unset, and also when it is shorter than the minimum length (Proposed: 32 random bytes, at least 43 base64url characters); a short value is treated as a configuration error and fails closed. The operator should `wrangler secret delete STUDIO_RECOVERY_TOKEN` after use.
- This trades security for availability: anyone with Cloudflare account access can take over the workspace. That is inherent in self-hosting and should be stated plainly to operators. Protect the Cloudflare account with 2FA. **Anyone able to set Worker secrets is owner-equivalent**, and that includes any Cloudflare API token with Workers Scripts edit rights, such as a CI deploy token. Scope and guard those tokens as owner credentials.
- Without Cloudflare access and without GitHub access there is no recovery by design.

### 6.2 Rotated or compromised OAuth client secret

1. Generate a second client secret in GitHub (assumed possible, verify), `wrangler secret put` the new value, deploy, confirm sign-in, then delete the old secret in GitHub.
2. If the secret may have been used by an attacker: delete it in GitHub first (users cannot sign in briefly), then deploy the replacement. Existing Studio sessions are independent of the OAuth secret, so also consider forcing a global session revocation (Proposed: owner action "revoke all sessions").
3. Never log or echo the secret during rotation.

### 6.3 Removed collaborator

**Proposed (needs maintainer confirmation; the issue requires only that removal invalidates access).** Removal runs one `batch` containing only unconditional, idempotent statements keyed on workspace and user: delete the user's sessions, revoke the credentials they created, revoke their pending invitations, then delete the membership. No statement depends on another statement failing, so the design does not rely on batch error semantics (§3.2). Because session and credential validity also join to a live membership (§5.4), a removed member loses access even if a retry is needed. Sockets are closed per §5.5. The user's authored history and audit rows are retained with the numeric ID. Demotion applies the same credential and pending-invitation revocation (§4.1).

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
- **Why sessions use HMAC and credentials plain SHA-256 (Proposed).** Both are 256-bit random values, so an unkeyed hash already resists offline guessing. Session identifiers use `HMAC-SHA-256` with `STUDIO_SESSION_HMAC_KEY` as defence in depth, and because sessions are short-lived: rotating that key safely invalidates every session at once ("revoke all sessions", §6.2). Credentials, invitation tokens, bootstrap/recovery tokens (consumed-hash records) use plain SHA-256 so that rotating the session key never silently breaks long-lived CI credentials. If the maintainer prefers one scheme, use a separate keyed hash per class rather than sharing a key (open question 11).

### 7.4 Expiry, rotation, revocation

All numbers **Proposed (needs maintainer confirmation)**:

| Setting | Proposed |
| --- | --- |
| Default TTL `site:read`/`site:edit` | 30 days |
| Default TTL `site:publish` | 7 days |
| Default TTL `site:activate` | 24 hours |
| Maximum TTL (any) | 90 days |
| Credentials per user | 10 active |

- **Rotation:** creating a replacement and revoking the old one are separate steps; an optional overlap window (Proposed: up to 24 hours) lets a CI pipeline cut over without downtime. A rotation endpoint issues a new token with the same scopes (never more than the creator's current role allows) and marks the old one with an earlier expiry.
- **Who may rotate (Proposed).** Rotation is allowed only from a **recently authenticated browser session** (younger than 15 minutes, the §7.2 re-auth rule) of the credential's creator, or of an owner/admin acting under §4.1. A bearer credential cannot rotate itself, because a stolen token must not be able to mint successors and live forever.
- **Chain limit.** Each credential carries an absolute `chain_expires_at`, set at original issuance to issue time plus the maximum TTL, and copied unchanged through `rotated_from`. A rotated credential's `expires_at` can never exceed `chain_expires_at`, so rotation cannot exceed the maximum TTL or bypass the re-auth rule of §7.2; extending beyond it requires a fresh issuance through §7.2.
- **Revocation:** immediate. Sets `revoked_at`; the next request fails and live sockets using that credential are closed per §5.5.
- Revocation of a credential is also triggered by removing **or demoting** its creator (all of their credentials are revoked, consistent with §4.1 and §6.3), or the owner's "revoke all" action.
- Every use updates `last_used_at` at a bounded rate (not per request), to give owners visibility without per-request D1 writes.

### 7.5 Redaction and tenant scoping

- Tokens, session identifiers, **invitation tokens, bootstrap tokens, recovery tokens**, OAuth `code`, `state`, client secret, CSRF tokens and `Authorization`/`Cookie` headers are redacted from logs, error bodies, panic messages and telemetry. Use a single redaction helper and test it (#65: "safe credential redaction").
- **Request paths and queries are redacted too.** Logs record the matched route template (for example `/invite/:token`) rather than the raw path, and never record query strings. Bootstrap and recovery tokens are accepted only in a `POST` body, never in a path or query, and request bodies on those routes are never logged.
- Error responses for a bad or foreign credential are uniform (`401`/`403`) and do not reveal whether a credential or site exists in another tenant.
- Every D1 query that touches tenant data binds the workspace ID derived from the authenticated principal, never from a client-supplied value (#61 tenant scoping). All queries use bound parameters (AGENTS.md §3).
- MCP mutations use the same server-side authorization as the CLI; the MCP server is not a trusted bypass.

---

## 8. Secrets handling

Consistent with AGENTS.md §8: secrets live in `.dev.vars` (gitignored), `private/*.env` (gitignored, mode 600) or `wrangler secret put`. They are never committed and never printed. `wrangler.jsonc` and `.dev.vars.example` carry placeholders only.

Names below are **Proposed** for #61 and #65:

| Name | Kind | Purpose |
| --- | --- | --- |
| `GITHUB_OAUTH_CLIENT_ID` | `vars` entry in the Studio Worker config (non-secret, per environment) | OAuth app client ID |
| `GITHUB_OAUTH_CLIENT_SECRET` | secret | OAuth code exchange |
| `STUDIO_BOOTSTRAP_TOKEN` | secret (temporary) | One-shot owner bootstrap (§3); minimum length enforced |
| `STUDIO_RECOVERY_TOKEN` | secret (temporary) | Break-glass owner recovery (§6.1); unset when unused; minimum length enforced |
| `STUDIO_SESSION_HMAC_KEY` | secret | Keyed hashing of session identifiers and CSRF tokens (so D1 contents alone cannot be replayed) |
| `STUDIO_PUBLIC_ORIGIN` | `vars` entry in the Studio Worker config (non-secret) | Exact Studio origin for the fixed redirect URI (§5.1) and `Origin` checks |
| `DB` | D1 binding | Studio registry (separate from the visitor Worker's DB, per #61) |

Rules:

- **Where the non-secret config lives (Proposed: one mechanism).** `GITHUB_OAUTH_CLIENT_ID` and `STUDIO_PUBLIC_ORIGIN` are Worker `vars` in the Studio Worker's wrangler config. The committed file holds only `.example` placeholders; each operator's real values live in a gitignored per-environment override under `private/` passed to wrangler with `--config`. Local dev uses the same `vars` keys with the dev OAuth app's values and `http://127.0.0.1:8787`; `.dev.vars` carries only the secrets.
- **GitHub endpoint URLs are constants in production.** Any override of the GitHub authorize, token or API URLs (used to point tests at a fake GitHub) is **test-only**: it exists only in test builds or the test harness config and is **never** a production variable or secret.
- **Owner-equivalence.** Anyone who can set Worker secrets (including a Cloudflare API token with Workers Scripts edit rights, such as a CI deploy token) can set the bootstrap/recovery secrets or replace the code, and is therefore owner-equivalent (§6.1).
- Local development uses the separate dev OAuth app and a dev-only `.dev.vars`; the always-pass Turnstile test keys are for public forms only and have no role in Studio auth.
- The Studio Worker's configuration is separate from the visitor Worker (#61). The visitor Worker holds no OAuth secret.
- A missing secret makes the dependent route fail closed with a generic `503`, never a default value. The exceptions are `STUDIO_BOOTSTRAP_TOKEN` and `STUDIO_RECOVERY_TOKEN`, whose routes return `404` when the secret is unset or below the minimum length (§3.2, §6.1).
- CI uses no real OAuth secret; tests run against a fake GitHub endpoint.

---

## 9. Hand-off to #61 / #62 / #65 / #68

### 9.1 For #61 (Studio Worker and D1 registry)

1. Tables (names Proposed): `workspaces`, `sites`, `users`, `memberships`, `invitations`, `sessions`, `credentials`, `bootstrap_state`, `audit_events`, `oauth_states` (or store `state` in a DO/KV-free short-lived row) with columns `state_hash`, `purpose`, `invitation_id`, `invitation_token_hash`, `secret_verified`, `return_to`, `pkce_verifier`, `expires_at`, `consumed_at`; plus a rate-limit counter table if the D1 fallback of §5.6 is chosen.
2. `users`: internal ID, `github_id` (INTEGER, **UNIQUE**, immutable key), `github_login` (display only, updated at sign-in), `created_at`. No GitHub access token column. No email column unless the maintainer decides otherwise.
3. `memberships`: `workspace_id`, `user_id`, `role` (CHECK against the role set), `created_at`; unique on (`workspace_id`, `user_id`). A partial unique index guarantees at most one `owner` per workspace.
4. `bootstrap_state`: singleton, `id INTEGER PRIMARY KEY CHECK (id = 1)`, `completed_at`, `owner_github_id`; deployment-wide, inserted first in the bootstrap batch (§3.2). Also `consumed_recovery_tokens` (`token_hash` primary key, `consumed_at`) inserted first in the recovery batch (§6.1).
5. `invitations`: `id`, `workspace_id`, `role` (CHECK `role <> 'owner'`), `token_hash`, `expected_github_login`, `bound_github_id` (nullable until accepted), `created_by`, `created_at`, `expires_at`, `accepted_at`, `revoked_at`.
6. `sessions`: `id_hash`, `user_id`, `workspace_id`, `created_at`, `last_seen_at`, `expires_at`, `revoked_at`. No plaintext identifier.
7. `credentials`: `id`, `prefix`, `token_hash`, `workspace_id`, `created_by`, `scopes`, `site_ids` (or a join table), `created_at`, `expires_at`, `chain_expires_at`, `last_used_at`, `revoked_at`, `rotated_from`.
8. Every tenant table carries `workspace_id`; migrations include tests that a query for workspace A never returns workspace B rows.
9. Authorization, revocation and bootstrap reads use the D1 primary (§5.5.8). All queries bound-parameter; session/credential touch writes are rate-limited and not per-edit; D1 checkpoint writes do not occur per edit (as #61 already requires).
10. Strict request bounds and the dual security headers apply to every Studio route (AGENTS.md §4, §3).
11. Tests: a second bootstrap (including with a different workspace name) hits the singleton PK and writes nothing; every write that must be atomic uses constraint-violation, not zero-row, semantics (§3.2).

### 9.2 For #62 (Durable Object sessions)

1. Attachments hold identity only (§5.5.2); authorization state is re-derived from D1.
2. Recheck authorization before applying edits using the policy in §5.5.3 (scope ∩ current role for credential sockets); publish and activation are HTTP-only (item 3).
3. Never accept publish, activation, invitation, or membership/credential management over the edit WebSocket (§5.5.3a); credential-authenticated sockets recheck scope ∩ current role. Test: an edit-only credential attempts publish and activate over the WebSocket and is refused.
4. Expose a revocation entry point (close sockets by user ID or credential ID) callable only by the Studio Worker.
5. Re-validate restored sockets before the first outbound broadcast after hibernation (§5.5.6).
6. Re-authorize on a periodic, retry-safe alarm so delivery stops within the documented bound even if the push signal is lost.
7. Presence entries for a removed identity are dropped on revocation.
8. Integration tests: removal while connected, removal while hibernated, credential revocation, and measurement of the maximum delivery-stop delay.

### 9.3 For #65 (auth implementation)

1. Implement the flow in §5.1 with `state` (and PKCE if confirmed), minimal scope, and no persisted GitHub token.
2. Implement bootstrap (§3) with the singleton-first batch and deployment-wide completion; tests that a second attempt fails (including for a different workspace and under concurrent attempts) and that the route is `404` afterwards, and that a short or unset token yields `404`.
3. Implement roles (§4.1) and invitation binding to the numeric ID (§4.2) using the conditional membership insert (never a zero-row `UPDATE` as the guard). Tests: replay, wrong identity, expired and revoked invitations each leave **no membership**; acceptance by an existing member errors without changing the role; an `owner` invitation is rejected; removing or demoting the inviter blocks acceptance of their pending invitations; the invite token is absent from logs.
3a. Implement the `oauth_states` purpose binding (§5.1): tests that a callback with a forged `purpose`/`invitation`/`next` parameter is ignored, a bootstrap or recovery purpose without `secret_verified` is rejected, and an off-origin `return_to` is never used.
3b. Implement recovery (§6.1) with the consumed-hash first statement; test that a reused recovery token fails and writes nothing, and that short or unset values give `404`.
3c. Implement rate limiting (§5.6) with tests for each route and the fail-closed behaviour.
4. Implement session cookie, CSRF and `Origin` checks, expiry and logout (§5.2 to §5.4).
5. Implement scoped credentials (§7) with the `edit` cannot `publish` or `activate` invariant enforced on the server, tested for every publish and activation route, **and over the WebSocket** (an edit-only credential attempting publish/activate on the socket is refused). Tests for rotation: only from a recent browser session, and `chain_expires_at` is never exceeded.
6. Protect HTTP and WebSocket paths; test tenant denial (cross-workspace access returns uniform errors).
7. Document and test the maximum revocation-propagation delay (§5.5.5), including after hibernation.
8. Test redaction of secrets in logs and errors (§7.5), including invitation, bootstrap and recovery tokens and request paths and queries.
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
4. Is the break-glass recovery route (§6.1) acceptable, given anyone with Cloudflare account access, or an API token with Workers Scripts edit rights (such as a CI deploy token), can use it?
5. `SameSite=Lax` versus re-issuing a `Strict` cookie after the OAuth callback?
6. Does PKCE work for OAuth apps (to be confirmed in §2.3), or should a GitHub App be reconsidered? #60 specifies an OAuth app, so this is only relevant if verification fails.
7. Is one workspace per Worker deployment the launch model, or must a single deployment host several workspaces (affects `workspace_id` scoping and bootstrap)? Until decided, bootstrap completion is deployment-wide (§3.2); a multi-workspace model needs its own authorized workspace-creation path.
8. Should invitations be deliverable by email later, and if so through which provider (not a launch dependency)?
9. Free-tier limits for Durable Object requests, D1 rows read/written and Worker requests under sign-in plus presence load are unverified assumptions; who validates them and when?
10. Is an audit log (`audit_events`) required for v0.2 or deferred?
11. Should sessions (HMAC) and credentials (plain SHA-256) keep different hashing schemes (§7.3), and are the rate limits and `chain_expires_at` rotation rule (§5.6, §7.4) acceptable?
