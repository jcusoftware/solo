# Studio site command sessions

`MIT OR Apache-2.0`

Implemented by issue #62 in `crates/sws-worker-studio/src/session/`. One SQLite-backed, hibernating Durable Object (`SiteSession`, binding `SITE_SESSION`) per site owns the live document. Command reduction is always the Rust reducer in `sws_ir::edit` ([`EDITOR_COMMANDS.md`](EDITOR_COMMANDS.md)); sessions never reimplement it. Pure logic (protocol, engine, rebase rules, authorization policy, attachments, SQL) is host-tested; only the Durable Object, DO SQL adapter and Worker entry are wasm-only.

## Route and identity

`GET /api/sites/{workspace_id}/{site_id}/session` with `Upgrade: websocket`. Order: request bounds, authenticate (`401 unauthenticated`), membership in D1 (`403 forbidden`, also for an unknown workspace), site exists in that workspace (`404`), then the request is forwarded to the object named `workspace_id/site_id` (ids are `[a-z0-9_-]{1,64}`, so names cannot collide). A plain GET answers `426`. Until #65 ships, `auth::authenticate` rejects everyone; only the local test build (`--features insecure-dev-auth`, never part of `build/` or any deploy, guarded by `npm run check:studio:release`) accepts `X-Sws-Dev-User`.

## Messages

One JSON text frame each, `{"v":1,"type":...}`, parsed strictly (unknown fields, versions or types are errors; nothing panics on input). Client: `ping`, `sync{since_seq?}`, `seed{document}`, `batch{op_id,base_seq,batch}`, `undo{op_id,target_seq}`, `checkpoint{reason: save|publish}`, `presence{state}`, `presence_get`. Server: `welcome`, `pong`, `state`, `replay`, `ack`, `entry`, `presence`, `checkpoint`, `conflict`, `error`, `revoked`. `batch` carries an ordinary command batch, parsed by `sws_ir::edit::parse_batch`. Every applied command is an `entry` with the next `seq`, delivered to all other sockets in log order; the sender gets an `ack`.

## Rules

- **Idempotency.** An `op_id` (8-64 chars) is remembered for 24 h (at most 5000). A retry returns the original `seq` with `duplicate: true` and changes and broadcasts nothing, even if the document has moved on. Another user reusing the id is refused.
- **Stale batches** (`base_seq` behind the head). Rebased onto the head only when every command is an explicit independent field update (`set_site_meta`, `set_color_token`, `set_font_token`, `set_data_value`, `set_attr`, `set_text`, `set_tag`, `set_classes`, `set_style_refs`) and none of its touched fields overlaps one touched by an entry after `base_seq`. Otherwise a `conflict`: `stale_structural` (insert/remove/move/page/rule structure, token or data removal, even without overlap), `field_conflict`, `target_changed`, or `base_too_old` (older than the retained log; `sync`). Each names the head, the conflicting `seqs` and `paths`, and says what to resend.
- **Undo** is a new log entry applying the target's recorded inverse. Own entries only (`undo_not_owner`); `undo_incompatible` if any later entry touched an overlapping field, including a later undo of the same entry; `undo_too_old` after compaction. Undoing an undo is a redo. History is never rewound.
- **Presence** is a bounded JSON object (512 bytes), one per socket, expiring after a TTL (default 30 s), rate limited, removed on disconnect, pruned by the alarm.

## Persistence and checkpoints

DO SQLite holds the latest snapshot, the command log (contiguous `seq`, immutable rows, enforced by triggers), the operation index and presence. Waking from hibernation rebuilds from the snapshot plus a replay of the log tail. D1 is written **only** at a checkpoint: every 50 commands, or on `checkpoint` save (any member) / publish (publisher or owner). A checkpoint writes the snapshot locally and queues exactly one `site_revisions` row (id derived from workspace, site, seq and hash; `ON CONFLICT (id) DO NOTHING`, so retries are safe). The pending row survives eviction; failed writes retry from the alarm with backoff (2 s doubling, 5 min cap), and edits are refused (`checkpoint_backlog`) if 200 commands pile up unsaved. The alarm also compacts the log to the 50 entries below the latest snapshot (idempotent, forward-only), prunes expired operation ids and presence, and runs the membership sweep.

## Bounds

Message 1 MiB (256 KiB except `seed`), document 1 MiB canonical JSON, 200 commands per batch, 16 sockets per site (4 per user), 512-byte presence, `base_seq`/`target_seq` validated. `seed` needs publisher or owner, only on an empty site, and is gated like `solo edit` (editable, lossless).

## Authorization after hibernation

The socket attachment (`client_id`, `user_id`, last verified role, `credential` reserved for #65) is restored after hibernation, but it is identity, not authority. The role is re-read from D1 before every state-changing message (so a removed or demoted member can never apply an edit), and every open socket is re-verified at least every `SESSION_AUTHZ_RECHECK_MS` (default 15 s, hard cap 30 s) by the alarm sweep and before each batch's broadcast. A removed member receives `revoked`, a 4403 close, and no further entries.

**Hooks for #65:** `object::fetch_role` / `recheck` is where session or token validity joins the D1 membership read (the attachment already carries `credential`); the Worker calls `session::entry::revoke_user(env, workspace_id, site_id, user_id)` (internal `POST /internal/revoke`) on logout, token revocation or membership change to close that user's sockets immediately; `entry::open` sets `X-Sws-Internal-Credential`. The 60 s worst-case revocation bound and its measured test belong to #65.

## Local tests and what they prove

`npm run test:studio:sessions` builds the Worker with `insecure-dev-auth` into `crates/sws-worker-studio/build-dev-auth`, runs `wrangler dev -c wrangler.studio-sessions.jsonc` on port 8790 with its own persisted state, and drives real WebSockets. It proves multi-client ordered sync, idempotent retries, rebase/conflict rules, undo, presence expiry, exactly one D1 revision row per checkpoint, membership removal, state surviving a full process restart, and hibernation: the object is evicted while the socket stays open and a new instance (different `instance_id`) serves the next message from the restored attachment and DO SQL. It does **not** prove production eviction timing, billing or duration behaviour, multi-region placement, or behaviour across deploys (a deploy disconnects sockets; clients must reconnect and `sync`). Real latency and quotas remain assumptions until verified on a Free account (#59, #69).
