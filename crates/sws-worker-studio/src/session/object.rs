//! `SiteSession`: the per-site hibernating Durable Object (wasm only).
//!
//! All decisions come from the pure [`Engine`]; this file only moves bytes:
//! WebSocket Hibernation API for sockets, DO SQLite for state, D1 for the
//! membership recheck and the checkpoint revision row, and one alarm for
//! retries, compaction, presence expiry and the membership sweep.
//!
//! Concurrency model: awaits (D1) can interleave other events, so every
//! state-changing message does its awaits FIRST (membership recheck, refreshing
//! stale peers) and then commits and broadcasts in one synchronous block. That
//! keeps log order equal to delivery order and makes the commit atomic.

use std::cell::{Cell, RefCell};

use serde_json::Value;
use worker::wasm_bindgen;
use worker::{
    Date, DurableObject, Env, Request, Response, Result, State, WebSocket,
    WebSocketIncomingMessage, WebSocketPair, console_error, console_log, durable_object,
};

use crate::model::Role;
use crate::repo;
use crate::respond::json_error;
use crate::session::attach::{Attachment, admit};
use crate::session::authz::{Action, allows, delivery_allowed, needs_recheck, role_from_row};
use crate::session::do_store::DoStore;
use crate::session::engine::{
    Actor, CheckpointOutcome, Committed, Engine, EngineError, Rejection, SyncPlan,
};
use crate::session::http::to_response;
use crate::session::protocol::{self, CheckpointReason, ClientMessage};
use crate::session::{
    CLOSE_PROTOCOL, CLOSE_REVOKED, CLOSE_TOO_LARGE, CLOSE_UNAUTHENTICATED, CLOSE_UNSUPPORTED_DATA,
    H_CREDENTIAL, H_ROLE, H_SITE, H_USER, H_WORKSPACE, PATH_REVOKE, PATH_SESSION, SessionConfig,
    is_valid_id,
};

type Eng = Engine<DoStore>;

fn now_ms() -> i64 {
    Date::now().as_millis() as i64
}

fn random_hex(bytes: usize) -> String {
    (0..bytes)
        .map(|_| {
            let b = (worker::js_sys::Math::random() * 256.0) as u8;
            format!("{b:02x}")
        })
        .collect()
}

fn user_tag(user_id: i64) -> String {
    format!("u:{user_id}")
}

#[durable_object]
pub struct SiteSession {
    state: State,
    env: Env,
    engine: RefCell<Option<Eng>>,
    /// Changes every time the runtime constructs this object (cold start or
    /// wake from hibernation); reported in `welcome`/`pong` so tests can prove
    /// the socket outlived the in-memory object.
    instance_id: String,
    config: SessionConfig,
    flushing: Cell<bool>,
}

impl DurableObject for SiteSession {
    fn new(state: State, env: Env) -> Self {
        let var = |n: &str| env.var(n).ok().map(|v| v.to_string());
        let config = SessionConfig::from_vars(
            var("SESSION_PRESENCE_TTL_MS").as_deref(),
            var("SESSION_AUTHZ_RECHECK_MS").as_deref(),
        );
        Self {
            state,
            env,
            engine: RefCell::new(None),
            instance_id: random_hex(8),
            config,
            flushing: Cell::new(false),
        }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let path = req.path();
        let method = req.method().to_string();
        match (method.as_str(), path.as_str()) {
            ("GET", PATH_SESSION) => self.accept(&req).await,
            ("POST", PATH_REVOKE) => {
                #[derive(serde::Deserialize)]
                struct Body {
                    user_id: i64,
                }
                let body: Body = match req.json().await {
                    Ok(b) => b,
                    Err(_) => return to_response(json_error(400, "bad_request", &[])),
                };
                let n = self.revoke_user(body.user_id).await;
                Response::from_json(&serde_json::json!({ "closed": n }))
            }
            _ => to_response(json_error(404, "not_found", &[])),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        let now = now_ms();
        // 1. Expired presence disappears (and peers are told).
        if let Ok(n) = self.with(None, |e| e.presence_prune(now)).await
            && n.unwrap_or(0) > 0
        {
            self.broadcast_presence(now);
        }
        // 2. Membership sweep: every connection, so a removed member is closed
        //    even if nobody is sending.
        self.recheck_peers(now, true).await;
        // 3. Retry the D1 checkpoint row if one is outstanding (idempotent).
        self.flush_pending().await;
        // 4. Compaction + operation-id pruning (idempotent, forward-only).
        match self.with(None, |e| e.compact(now_ms())).await {
            Ok(Ok(Some(h))) => console_log!("session compacted log through seq {h}"),
            Ok(Ok(None)) => {}
            Ok(Err(e)) => console_error!("compaction failed (will retry): {e}"),
            Err(e) => console_error!("compaction could not open the session: {e}"),
        }
        // 5. Re-arm for whatever remains.
        self.ensure_alarm().await;
        Response::ok("")
    }

    async fn websocket_message(
        &self,
        ws: WebSocket,
        message: WebSocketIncomingMessage,
    ) -> Result<()> {
        let text = match message {
            WebSocketIncomingMessage::String(s) => s,
            WebSocketIncomingMessage::Binary(_) => {
                let _ = ws.close(Some(CLOSE_UNSUPPORTED_DATA), Some("text frames only"));
                return Ok(());
            }
        };
        let att = match ws.deserialize_attachment::<Attachment>() {
            Ok(Some(a)) if !a.revoked => a,
            Ok(Some(_)) => return Ok(()),
            _ => {
                let _ = ws.close(Some(CLOSE_UNAUTHENTICATED), Some("unidentified connection"));
                return Ok(());
            }
        };
        match protocol::parse_client(&text) {
            Err(pe) => {
                if pe.code == "message_too_large" {
                    send(&ws, &protocol::error(pe.code, &pe.message, None, None));
                    let _ = ws.close(Some(CLOSE_TOO_LARGE), Some("message too large"));
                } else if pe.code == "unsupported_version" {
                    send(&ws, &protocol::error(pe.code, &pe.message, None, None));
                    let _ = ws.close(Some(CLOSE_PROTOCOL), Some("unsupported protocol version"));
                } else {
                    send(
                        &ws,
                        &protocol::error(
                            pe.code,
                            &pe.message,
                            pe.op_id.as_deref(),
                            pe.command_error.as_ref(),
                        ),
                    );
                }
                Ok(())
            }
            Ok(msg) => {
                if let Err(e) = self.dispatch(&ws, att, msg).await {
                    console_error!("session message failed: {e}");
                    send(
                        &ws,
                        &protocol::error("internal_error", "the session failed; retry", None, None),
                    );
                }
                Ok(())
            }
        }
    }

    async fn websocket_close(
        &self,
        ws: WebSocket,
        code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        let _ = ws.close(Some(crate::session::echo_close_code(code)), Some("closed"));
        self.drop_client(&ws).await;
        Ok(())
    }

    async fn websocket_error(&self, ws: WebSocket, _error: worker::Error) -> Result<()> {
        self.drop_client(&ws).await;
        Ok(())
    }
}

fn send(ws: &WebSocket, v: &Value) {
    // The peer may already be gone; delivery is best effort per socket.
    let _ = ws.send_with_str(v.to_string());
}

fn conflict_or_error(op_id: Option<&str>, r: &Rejection) -> Value {
    match r {
        Rejection::Conflict(c) => protocol::conflict(op_id, c),
        Rejection::Command(ce) | Rejection::InvalidDocument(ce) => {
            protocol::error(r.code(), &r.message(), op_id, Some(ce))
        }
        other => protocol::error(other.code(), &other.message(), op_id, None),
    }
}

impl SiteSession {
    // --- engine access ------------------------------------------------------

    /// Open the engine on first use (rebuilding from DO SQL after eviction), then
    /// run `f` synchronously. `identity` is `Some` on a fresh connection.
    async fn with<T>(
        &self,
        identity: Option<(&str, &str)>,
        f: impl FnOnce(&mut Eng) -> T,
    ) -> Result<T> {
        if self.engine.borrow().is_none() {
            let store = DoStore::new(self.state.storage().sql())
                .map_err(|e| worker::Error::RustError(e.to_string()))?;
            use crate::session::store::Store;
            let (w, s) = match identity {
                Some((w, s)) => (w.to_string(), s.to_string()),
                None => {
                    let m = store
                        .load_meta()
                        .map_err(|e| worker::Error::RustError(e.to_string()))?;
                    (m.workspace_id, m.site_id)
                }
            };
            if w.is_empty() {
                return Err(worker::Error::RustError(
                    "session is not initialised".into(),
                ));
            }
            let engine =
                Engine::open(store, &w, &s).map_err(|e| worker::Error::RustError(e.to_string()))?;
            *self.engine.borrow_mut() = Some(engine);
        }
        let mut guard = self.engine.borrow_mut();
        let engine = guard
            .as_mut()
            .ok_or_else(|| worker::Error::RustError("engine unavailable".into()))?;
        Ok(f(engine))
    }

    fn identity(&self) -> Option<(String, String)> {
        self.engine
            .borrow()
            .as_ref()
            .map(|e| (e.meta().workspace_id.clone(), e.meta().site_id.clone()))
    }

    // --- connect --------------------------------------------------------------

    async fn accept(&self, req: &Request) -> Result<Response> {
        let h = |name: &str| req.headers().get(name).ok().flatten();
        let upgrade = h("Upgrade").unwrap_or_default();
        if !upgrade.eq_ignore_ascii_case("websocket") {
            return to_response(json_error(
                426,
                "upgrade_required",
                &[("Upgrade", "websocket")],
            ));
        }
        let (Some(workspace_id), Some(site_id), Some(user), Some(role)) = (
            h(H_WORKSPACE),
            h(H_SITE),
            h(H_USER).and_then(|u| u.parse::<i64>().ok()),
            h(H_ROLE).and_then(|r| Role::parse(&r)),
        ) else {
            return to_response(json_error(400, "bad_request", &[]));
        };
        if !is_valid_id(&workspace_id) || !is_valid_id(&site_id) || user <= 0 {
            return to_response(json_error(400, "bad_request", &[]));
        }
        let now = now_ms();
        let opened = self
            .with(Some((&workspace_id, &site_id)), |e| {
                (e.head_seq(), e.seeded())
            })
            .await;
        let (head, seeded) = match opened {
            Ok(v) => v,
            Err(e) => {
                console_error!("session open failed: {e}");
                return to_response(json_error(409, "session_unavailable", &[]));
            }
        };
        let existing: Vec<i64> = self
            .state
            .get_websockets()
            .iter()
            .filter_map(|w| w.deserialize_attachment::<Attachment>().ok().flatten())
            .filter(|a| !a.revoked)
            .map(|a| a.user_id)
            .collect();
        if let Err(code) = admit(&existing, user) {
            return to_response(json_error(429, code, &[("Retry-After", "5")]));
        }

        let pair = WebSocketPair::new()?;
        // Hibernatable accept: the runtime, not this object, holds the socket.
        self.state
            .accept_websocket_with_tags(&pair.server, &[&user_tag(user)]);
        let client_id = format!("c-{}", random_hex(8));
        let mut att = Attachment::new(client_id.clone(), user, role, now);
        att.credential = h(H_CREDENTIAL);
        pair.server.serialize_attachment(&att)?;
        send(
            &pair.server,
            &protocol::welcome(
                &client_id,
                user,
                role.as_str(),
                head,
                seeded,
                &self.instance_id,
            ),
        );
        self.ensure_alarm().await;
        Response::from_websocket(pair.client)
    }

    // --- authorization --------------------------------------------------------

    /// Fresh D1 membership read for one user. `Err` = D1 unavailable (callers
    /// fail closed for edits and keep sockets open).
    async fn fetch_role(&self, user_id: i64) -> Result<Option<Role>> {
        let (workspace_id, _) = self
            .identity()
            .ok_or_else(|| worker::Error::RustError("session is not initialised".into()))?;
        // #65: also validate the session/token behind `att.credential` here.
        let db = crate::d1::database(&self.env)?;
        let row = crate::d1::first(&db, &repo::membership_get(&workspace_id, user_id)).await?;
        Ok(row.as_ref().and_then(role_from_row))
    }

    fn revoke_socket(&self, ws: &WebSocket, att: Attachment, reason: &str) {
        let mut a = att;
        a.revoked = true;
        let _ = ws.serialize_attachment(&a);
        send(ws, &protocol::revoked(reason));
        let _ = ws.close(Some(CLOSE_REVOKED), Some(reason));
    }

    /// Re-read membership for `ws`'s user and update/close the socket.
    /// `Ok(Some(role))` = still allowed.
    async fn recheck(&self, ws: &WebSocket, att: &Attachment, now: i64) -> Result<Option<Role>> {
        match self.fetch_role(att.user_id).await? {
            Some(role) => {
                let _ = ws.serialize_attachment(att.clone().refreshed(role, now));
                Ok(Some(role))
            }
            None => {
                self.revoke_socket(ws, att.clone(), "membership_revoked");
                self.drop_presence(&att.client_id).await;
                Ok(None)
            }
        }
    }

    /// Refresh every other connection whose last check is older than the
    /// interval (`force`: all of them). One D1 read per distinct user.
    async fn recheck_peers(&self, now: i64, force: bool) {
        let mut seen: Vec<(i64, Option<Role>)> = Vec::new();
        for ws in self.state.get_websockets() {
            let Ok(Some(att)) = ws.deserialize_attachment::<Attachment>() else {
                continue;
            };
            if att.revoked
                || !(force
                    || needs_recheck(now, att.authz_checked_ms, self.config.authz_recheck_ms))
            {
                continue;
            }
            let role = match seen.iter().find(|(u, _)| *u == att.user_id) {
                Some((_, r)) => *r,
                None => match self.fetch_role(att.user_id).await {
                    Ok(r) => {
                        seen.push((att.user_id, r));
                        r
                    }
                    Err(e) => {
                        console_error!("membership recheck failed (keeping socket): {e}");
                        continue;
                    }
                },
            };
            match role {
                Some(r) => {
                    let _ = ws.serialize_attachment(att.refreshed(r, now));
                }
                None => {
                    let client = att.client_id.clone();
                    self.revoke_socket(&ws, att, "membership_revoked");
                    self.drop_presence(&client).await;
                }
            }
        }
    }

    /// Worker -> DO internal request (#65 wires the callers).
    async fn revoke_user(&self, user_id: i64) -> usize {
        let mut n = 0;
        for ws in self.state.get_websockets_with_tag(&user_tag(user_id)) {
            if let Ok(Some(att)) = ws.deserialize_attachment::<Attachment>() {
                let client = att.client_id.clone();
                self.revoke_socket(&ws, att, "credential_revoked");
                self.drop_presence(&client).await;
                n += 1;
            }
        }
        n
    }

    // --- delivery -------------------------------------------------------------

    /// Send to every live, recently verified socket except `skip`. Synchronous
    /// on purpose: ordering. Sockets whose authorization is more than two
    /// intervals old (a failed refresh) are skipped until the sweep handles them.
    fn broadcast(&self, v: &Value, skip: Option<&WebSocket>, now: i64) {
        let text = v.to_string();
        for ws in self.state.get_websockets() {
            if skip.is_some_and(|s| *s == ws) {
                continue;
            }
            let Ok(Some(att)) = ws.deserialize_attachment::<Attachment>() else {
                continue;
            };
            if att.revoked
                || !delivery_allowed(now, att.authz_checked_ms, self.config.authz_recheck_ms)
            {
                continue;
            }
            let _ = ws.send_with_str(&text);
        }
    }

    fn broadcast_presence(&self, now: i64) {
        if let Ok(Some(rows)) = self
            .engine
            .borrow()
            .as_ref()
            .map(|e| e.presence_list(now))
            .transpose()
        {
            self.broadcast(&protocol::presence(now, &rows), None, now);
        }
    }

    async fn drop_presence(&self, client_id: &str) {
        let removed = self
            .with(None, |e| e.presence_remove(client_id))
            .await
            .is_ok();
        if removed {
            self.broadcast_presence(now_ms());
        }
    }

    async fn drop_client(&self, ws: &WebSocket) {
        if let Ok(Some(att)) = ws.deserialize_attachment::<Attachment>() {
            self.drop_presence(&att.client_id).await;
        }
        self.ensure_alarm().await;
    }

    // --- alarm scheduling -------------------------------------------------------

    async fn ensure_alarm(&self) {
        let now = now_ms();
        let connections = self
            .state
            .get_websockets()
            .iter()
            .filter(|w| {
                w.deserialize_attachment::<Attachment>()
                    .ok()
                    .flatten()
                    .is_some_and(|a| !a.revoked)
            })
            .count();
        let recheck = self.config.authz_recheck_ms;
        let at = match self
            .with(None, |e| e.next_alarm(now, connections, recheck))
            .await
        {
            Ok(Ok(at)) => at,
            _ => return,
        };
        let Some(at) = at else { return };
        let storage = self.state.storage();
        let current = storage.get_alarm().await.ok().flatten();
        // Only ever pull the alarm earlier; the handler re-arms for what remains.
        if current.is_none_or(|c| c > at || c <= now)
            && let Err(e) = storage.set_alarm((at - now).max(50)).await
        {
            console_error!("set_alarm failed: {e}");
        }
    }

    // --- checkpoint (D1) ----------------------------------------------------------

    /// Write the pending revision row to D1, once. Safe to call from anywhere,
    /// repeatedly: the id is deterministic and the insert ignores an `id` conflict.
    /// Returns true when nothing is pending afterwards.
    async fn flush_pending(&self) -> bool {
        if self.flushing.replace(true) {
            return false;
        }
        let result = self.flush_inner().await;
        self.flushing.set(false);
        result
    }

    async fn flush_inner(&self) -> bool {
        let snapshot = {
            let g = self.engine.borrow();
            g.as_ref().and_then(|e| {
                e.pending().map(|p| {
                    (
                        e.meta().workspace_id.clone(),
                        e.meta().site_id.clone(),
                        p.clone(),
                    )
                })
            })
        };
        let Some((workspace_id, site_id, p)) = snapshot else {
            return true;
        };
        let stmt = repo::revision_checkpoint_idempotent(
            &workspace_id,
            &site_id,
            &p.revision_id,
            &p.ir_sha256,
            p.byte_size,
            p.created_by,
        );
        let written = match crate::d1::database(&self.env) {
            Ok(db) => crate::d1::run(&db, &stmt).await.map(|_| ()),
            Err(e) => Err(e),
        };
        match written {
            Ok(()) => {
                if let Ok(Ok(true)) = self.with(None, |e| e.mark_revision_written(p.seq)).await {
                    self.broadcast(
                        &protocol::checkpoint(p.seq, &p.revision_id, "written", &p.reason),
                        None,
                        now_ms(),
                    );
                }
                true
            }
            Err(e) => {
                console_error!("checkpoint D1 write failed (will retry): {e}");
                let _ = self.with(None, |e| e.note_revision_failure()).await;
                false
            }
        }
    }

    // --- message handling ---------------------------------------------------------

    async fn dispatch(&self, ws: &WebSocket, att: Attachment, msg: ClientMessage) -> Result<()> {
        let now = now_ms();
        // Reads and presence: the cached verification is enough while it is
        // fresh; otherwise re-read now. Writes always re-read (below).
        let action = match &msg {
            ClientMessage::Ping => return self.on_ping(ws).await,
            ClientMessage::Sync { .. }
            | ClientMessage::PresenceGet
            | ClientMessage::Presence { .. } => Action::Read,
            ClientMessage::Batch { .. } | ClientMessage::Undo { .. } => Action::Edit,
            ClientMessage::Checkpoint { reason } => match reason {
                CheckpointReason::Publish => Action::Publish,
                _ => Action::Save,
            },
            ClientMessage::Seed { .. } => Action::Publish,
        };
        let is_write = !matches!(action, Action::Read);
        let role =
            if is_write || needs_recheck(now, att.authz_checked_ms, self.config.authz_recheck_ms) {
                match self.recheck(ws, &att, now).await {
                    Ok(Some(r)) => r,
                    Ok(None) => return Ok(()), // revoked and closed
                    Err(e) => {
                        console_error!("membership check failed: {e}");
                        send(
                            ws,
                            &protocol::error(
                                "unavailable",
                                "membership could not be verified; retry shortly",
                                None,
                                None,
                            ),
                        );
                        return Ok(());
                    }
                }
            } else {
                att.role()
                    .ok_or_else(|| worker::Error::RustError("unknown role in attachment".into()))?
            };
        if !allows(role, action) {
            let msg = match action {
                Action::Publish => "publishing requires the publisher or owner role",
                _ => "your role does not allow this",
            };
            send(ws, &protocol::error("forbidden", msg, None, None));
            return Ok(());
        }
        let att = Attachment {
            role: role.as_str().to_string(),
            ..att
        };
        let actor = Actor {
            user_id: att.user_id,
            client_id: att.client_id.clone(),
        };
        match msg {
            ClientMessage::Ping => Ok(()),
            ClientMessage::Sync { since_seq } => self.on_sync(ws, since_seq).await,
            ClientMessage::PresenceGet => {
                let now = now_ms();
                let rows = self.with(None, |e| e.presence_list(now)).await?;
                send(ws, &protocol::presence(now, &rows.map_err(engine_err)?));
                Ok(())
            }
            ClientMessage::Presence { state } => self.on_presence(ws, &actor, state).await,
            ClientMessage::Seed { document } => self.on_seed(ws, &actor, &document).await,
            ClientMessage::Batch {
                op_id,
                base_seq,
                batch,
            } => {
                // Awaits first: keep delivery targets verified before the commit.
                self.recheck_peers(now_ms(), false).await;
                let now = now_ms();
                let res = self
                    .with(None, |e| {
                        e.submit_batch(&actor, &op_id, base_seq, &batch, now)
                    })
                    .await?;
                self.finish_submit(ws, &op_id, res, now, att.user_id).await
            }
            ClientMessage::Undo { op_id, target_seq } => {
                self.recheck_peers(now_ms(), false).await;
                let now = now_ms();
                let res = self
                    .with(None, |e| e.submit_undo(&actor, &op_id, target_seq, now))
                    .await?;
                self.finish_submit(ws, &op_id, res, now, att.user_id).await
            }
            ClientMessage::Checkpoint { reason } => self.on_checkpoint(ws, &att, reason).await,
        }
    }

    async fn on_ping(&self, ws: &WebSocket) -> Result<()> {
        let head = self.with(None, |e| e.head_seq()).await?;
        send(ws, &protocol::pong(head, &self.instance_id));
        Ok(())
    }

    async fn on_sync(&self, ws: &WebSocket, since_seq: Option<i64>) -> Result<()> {
        let out = self
            .with(None, |e| {
                if !e.seeded() {
                    return Ok(protocol::error(
                        "not_seeded",
                        &Rejection::NotSeeded.message(),
                        None,
                        None,
                    ));
                }
                Ok::<Value, EngineError>(match e.sync_plan(since_seq)? {
                    SyncPlan::Replay(entries) => protocol::replay(e.head_seq(), &entries),
                    SyncPlan::Full => protocol::state(
                        e.head_seq(),
                        &e.document_text().unwrap_or_default(),
                        e.checkpoint_seq(),
                    ),
                })
            })
            .await?
            .map_err(engine_err)?;
        send(ws, &out);
        Ok(())
    }

    async fn on_presence(&self, ws: &WebSocket, actor: &Actor, state: String) -> Result<()> {
        let now = now_ms();
        let ttl = self.config.presence_ttl_ms;
        let res = self
            .with(None, |e| {
                e.presence_set(&actor.client_id, actor.user_id, state, now, ttl)
            })
            .await?;
        match res {
            Ok(()) => {
                self.broadcast_presence(now);
                self.ensure_alarm().await;
            }
            Err(r) => send(ws, &conflict_or_error(None, &r)),
        }
        Ok(())
    }

    async fn on_seed(&self, ws: &WebSocket, _actor: &Actor, document: &str) -> Result<()> {
        let now = now_ms();
        let res = self
            .with(None, |e| {
                e.seed(document, now)
                    .map(|()| (e.head_seq(), e.document_text().unwrap_or_default()))
            })
            .await?;
        match res {
            Ok((head, text)) => {
                // The sender is part of the broadcast.
                self.broadcast(&protocol::state(head, &text, None), None, now);
            }
            Err(r) => send(ws, &conflict_or_error(None, &r)),
        }
        Ok(())
    }

    async fn finish_submit(
        &self,
        ws: &WebSocket,
        op_id: &str,
        res: std::result::Result<Committed, Rejection>,
        now: i64,
        user_id: i64,
    ) -> Result<()> {
        match res {
            Ok(c) => {
                let head = c.seq.max(self.with(None, |e| e.head_seq()).await?);
                send(
                    ws,
                    &protocol::ack(op_id, c.seq, c.duplicate, c.rebased, head),
                );
                if let Some(entry) = &c.entry {
                    self.broadcast(&protocol::entry(entry), Some(ws), now);
                    self.maybe_auto_checkpoint(user_id, now).await;
                }
            }
            Err(r) => {
                if let Rejection::Unavailable(m) = &r {
                    console_error!("{m}");
                }
                send(ws, &conflict_or_error(Some(op_id), &r));
            }
        }
        Ok(())
    }

    /// Every `CHECKPOINT_EVERY` commands: snapshot locally and write exactly one
    /// D1 revision row. Never per command.
    async fn maybe_auto_checkpoint(&self, user_id: i64, now: i64) {
        let taken = self
            .with(None, |e| {
                if e.checkpoint_due() {
                    e.take_checkpoint(user_id, CheckpointReason::Auto, now).ok()
                } else {
                    None
                }
            })
            .await
            .ok()
            .flatten();
        if matches!(taken, Some(CheckpointOutcome::Taken(_))) {
            self.flush_pending().await;
            self.ensure_alarm().await;
        }
    }

    async fn on_checkpoint(
        &self,
        ws: &WebSocket,
        att: &Attachment,
        reason: CheckpointReason,
    ) -> Result<()> {
        // Resolve any outstanding row first so this request sees a clean slate.
        self.flush_pending().await;
        let now = now_ms();
        let out = self
            .with(None, |e| e.take_checkpoint(att.user_id, reason, now))
            .await?;
        match out {
            Ok(CheckpointOutcome::Taken(p)) => {
                let done = self.flush_pending().await;
                let status = if done { "written" } else { "pending" };
                let m = protocol::checkpoint(p.seq, &p.revision_id, status, reason.as_str());
                if done {
                    // `flush_pending` already broadcast "written" to everyone.
                    send(ws, &m);
                } else {
                    self.broadcast(&m, None, now);
                }
                self.ensure_alarm().await;
            }
            Ok(CheckpointOutcome::Pending(p)) => {
                send(
                    ws,
                    &protocol::checkpoint(p.seq, &p.revision_id, "pending", p.reason.as_str()),
                );
            }
            Ok(CheckpointOutcome::Unchanged { seq }) => {
                send(
                    ws,
                    &protocol::checkpoint(seq, "", "unchanged", reason.as_str()),
                );
            }
            Err(r) => send(ws, &conflict_or_error(None, &r)),
        }
        Ok(())
    }
}

fn engine_err(e: EngineError) -> worker::Error {
    worker::Error::RustError(e.to_string())
}
