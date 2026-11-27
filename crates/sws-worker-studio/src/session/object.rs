//! `SiteSession`: the per-site hibernating Durable Object (wasm only).
//!
//! All decisions come from the pure [`Engine`]; this file only moves bytes:
//! WebSocket Hibernation API for sockets, DO SQLite for state, D1 for the
//! membership recheck and the checkpoint revision row, and one alarm for
//! retries, compaction, presence expiry and the membership sweep.
//!
//! Concurrency model:
//!
//! * **Ordering.** Inbound messages are handled one at a time through a FIFO
//!   ([`Fifo`]): a handler takes its ticket synchronously on arrival and runs only
//!   when every earlier message has finished. So pipelined messages (a client that
//!   sends batch N+1 before the ack of batch N) are applied in the order they were
//!   sent, per socket and across sockets, even though handlers await D1.
//! * **Atomic commit.** Awaits (D1) can interleave other events, so every
//!   state-changing message does its awaits FIRST (membership recheck, refreshing
//!   stale peers), re-checks the socket was not revoked meanwhile, and then
//!   commits and broadcasts in one synchronous block. Each engine call runs
//!   inside `storage.transactionSync` so its DO SQL writes commit or roll back
//!   together ([`run_in_tx`]: a failed or thrown transaction drops the cached
//!   engine and returns an error, never a success).
//! * **Revocation is sticky.** After any await the socket's attachment is read
//!   again and merged ([`merge_membership`]); a copy taken before an await is
//!   never written back, so a concurrent revocation cannot be undone.

use std::cell::{Cell, RefCell};

use serde_json::Value;
use worker::wasm_bindgen;
use worker::wasm_bindgen::JsCast;
use worker::wasm_bindgen::JsValue;
use worker::wasm_bindgen::closure::ScopedClosure;
use worker::{
    Date, DurableObject, Env, Request, Response, Result, State, WebSocket,
    WebSocketIncomingMessage, WebSocketPair, console_error, console_log, durable_object,
};

use crate::auth::policy::effective_role;
use crate::model::Role;
use crate::repo;
use crate::respond::json_error;
use crate::session::attach::{Attachment, Refresh, admit, merge_membership};
use crate::session::authz::{
    Action, allowed_now, allows, delivery_allowed, needs_recheck, role_from_row,
};
use crate::session::do_store::DoStore;
use crate::session::engine::{
    Actor, CheckpointOutcome, Committed, Engine, EngineError, FailurePlan, Rejection, SyncPlan,
    plan_checkpoint_failure,
};
use crate::session::fifo::Fifo;
use crate::session::http::to_response;
use crate::session::protocol::{self, CheckpointReason, ClientMessage};
use crate::session::txn::run_in_tx;
use crate::session::{
    CLOSE_PROTOCOL, CLOSE_REVOKED, CLOSE_TOO_LARGE, CLOSE_UNAUTHENTICATED, CLOSE_UNSUPPORTED_DATA,
    H_CREDENTIAL, H_ROLE, H_SITE, H_USER, H_WORKSPACE, MAX_QUEUED_MESSAGES, PATH_REVISION,
    PATH_REVOKE, PATH_SESSION, SessionConfig, alarm_after, is_valid_id,
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
    /// Serializes message handling (see the module docs).
    fifo: Fifo,
    /// Test build only: simulate a D1 outage (see `PATH_D1_FAULT`).
    #[cfg(feature = "insecure-dev-auth")]
    dev_d1_down: Cell<bool>,
    /// Test build only: only checkpoint writes fail (membership reads work).
    #[cfg(feature = "insecure-dev-auth")]
    dev_ckpt_down: Cell<bool>,
    /// Test build only: every membership read takes 400 ms, so a test can land a
    /// revocation deterministically while a message is awaiting D1.
    #[cfg(feature = "insecure-dev-auth")]
    dev_d1_slow: Cell<bool>,
}

/// What a checkpoint flush ended with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flush {
    /// Nothing was pending, or another flush is in flight.
    Idle,
    Written,
    /// Transient failure: still pending, retried by the alarm with backoff.
    Pending,
    /// Permanently unwritable: dropped (with an alert) so edits resume.
    Abandoned,
}

impl DurableObject for SiteSession {
    fn new(state: State, env: Env) -> Self {
        let var = |n: &str| env.var(n).ok().map(|v| v.to_string());
        let config = SessionConfig::from_vars(
            var("SESSION_PRESENCE_TTL_MS").as_deref(),
            var("SESSION_AUTHZ_RECHECK_MS").as_deref(),
            var("SESSION_IDLE_SWEEP_MS").as_deref(),
        );
        Self {
            state,
            env,
            engine: RefCell::new(None),
            instance_id: random_hex(8),
            config,
            flushing: Cell::new(false),
            fifo: Fifo::new(),
            #[cfg(feature = "insecure-dev-auth")]
            dev_d1_down: Cell::new(false),
            #[cfg(feature = "insecure-dev-auth")]
            dev_ckpt_down: Cell::new(false),
            #[cfg(feature = "insecure-dev-auth")]
            dev_d1_slow: Cell::new(false),
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
                    /// Only sockets opened with this credential tag (logout, token
                    /// revocation); `None` = every socket of the user.
                    #[serde(default)]
                    credential: Option<String>,
                }
                let body: Body = match req.json().await {
                    Ok(b) => b,
                    Err(_) => return to_response(json_error(400, "bad_request", &[])),
                };
                let n = self
                    .revoke_user(body.user_id, body.credential.as_deref())
                    .await;
                Response::from_json(&serde_json::json!({ "closed": n }))
            }
            ("GET", PATH_REVISION) => {
                let id = req
                    .url()
                    .ok()
                    .and_then(|u| {
                        u.query_pairs()
                            .find(|(k, _)| k == "id")
                            .map(|(_, v)| v.to_string())
                    })
                    .filter(|id| is_valid_id(id));
                let Some(id) = id else {
                    return to_response(json_error(400, "bad_request", &[]));
                };
                // A never-used object has nothing to look up: answer 404 without
                // running the schema (a read must not create tables).
                if self.engine.borrow().is_none()
                    && !DoStore::initialised(&self.state.storage().sql())
                {
                    return to_response(json_error(404, "not_found", &[]));
                }
                let snap = self
                    .with(None, |e| e.revision_snapshot(&id))
                    .await?
                    .map_err(engine_err)?;
                match snap {
                    Some(s) => Response::from_json(&serde_json::json!({
                        "revision_id": id,
                        "seq": s.seq,
                        "sha256": s.sha256,
                        "document": s.doc,
                    })),
                    None => to_response(json_error(404, "not_found", &[])),
                }
            }
            #[cfg(feature = "insecure-dev-auth")]
            ("POST", crate::session::PATH_TX_PROBE) => self.tx_probe().await,
            #[cfg(feature = "insecure-dev-auth")]
            ("POST", crate::session::PATH_D1_FAULT) => {
                let mode = req
                    .url()
                    .ok()
                    .and_then(|u| {
                        u.query_pairs()
                            .find(|(k, _)| k == "mode")
                            .map(|(_, v)| v.to_string())
                    })
                    .unwrap_or_default();
                self.dev_d1_down.set(mode == "down");
                self.dev_d1_slow.set(mode == "slow");
                self.dev_ckpt_down
                    .set(mode == "down" || mode == "checkpoint");
                Response::from_json(&serde_json::json!({ "mode": mode }))
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
            self.broadcast_presence(now).await;
        }
        // 2. Membership sweep: every connection, so a removed member is closed
        //    even if nobody is sending. (Cadence: `idle_sweep_ms`, see `ensure_alarm`.)
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
        // Bounded queue: past the cap, refuse instead of queueing (a flooding socket
        // must not grow memory without bound). Nothing is applied for a refused message.
        if self.fifo.is_full(MAX_QUEUED_MESSAGES) {
            send(
                &ws,
                &protocol::error(
                    "rate_limited",
                    "too many messages in flight; slow down and resend",
                    None,
                    None,
                ),
            );
            return Ok(());
        }
        // The ticket is taken before the first await, i.e. in arrival order.
        let turn = self.fifo.enter();
        let _held = turn.await;
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
                        &protocol::error(
                            "unavailable",
                            "the session storage failed and was rolled back; retry shortly",
                            None,
                            None,
                        ),
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

/// `WebSocket.readyState === OPEN`. A closing or closed peer reports otherwise.
fn socket_open(ws: &WebSocket) -> bool {
    let state: &worker::web_sys::WebSocket = ws.as_ref();
    crate::session::attach::ready_state_open(state.ready_state())
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
    /// run `f` synchronously inside one `transactionSync`, so its DO SQL writes
    /// commit together or not at all. `identity` is `Some` on a fresh connection.
    ///
    /// Any storage failure (a failed statement, or a `transactionSync` that threw)
    /// rolls the transaction back, drops the cached engine so the next call
    /// rebuilds from durable state, and returns `Err`: never `f`'s result, because
    /// `f` may describe work the rollback discarded (see [`run_in_tx`]).
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
        run_in_tx(&self.engine, f, |body| self.transaction_sync(body)).map_err(|e| {
            console_error!("{e}; the cached engine was dropped");
            worker::Error::RustError(e.to_string())
        })
    }

    /// `ctx.storage.transactionSync(body)`: all DO SQL the body issues commits
    /// together; a body that returns `Err` (or a call that throws) rolls them all
    /// back and this returns `Err`. workers-rs 0.9 does not wrap this API, so it is
    /// called through JS. Without the API (a runtime too old for it) this FAILS
    /// CLOSED: the body is not run at all, because running writes non-atomically
    /// could leave a half-applied command (log row without its operation id).
    fn transaction_sync(
        &self,
        body: &mut dyn FnMut() -> std::result::Result<(), ()>,
    ) -> std::result::Result<(), ()> {
        let storage: Option<(JsValue, worker::js_sys::Function)> = self
            .state
            .as_raw()
            .storage()
            .ok()
            .map(JsValue::from)
            .and_then(|st| {
                worker::js_sys::Reflect::get(&st, &JsValue::from_str("transactionSync"))
                    .ok()
                    .and_then(|f| f.dyn_into::<worker::js_sys::Function>().ok())
                    .map(|f| (st, f))
            });
        let Some((st, func)) = storage else {
            console_error!("storage.transactionSync is unavailable; refusing non-atomic writes");
            return Err(());
        };
        let mut js_body = || -> std::result::Result<JsValue, JsValue> {
            body()
                .map(|()| JsValue::UNDEFINED)
                .map_err(|()| JsValue::from_str("storage failure: rolling back"))
        };
        let closure =
            ScopedClosure::<dyn FnMut() -> std::result::Result<JsValue, JsValue>>::borrow_mut_assert_unwind_safe(&mut js_body);
        // An Err here is the rollback signal thrown by `body` (or a runtime error).
        func.call1(&st, closure.as_ref())
            .map(|_| ())
            .map_err(|_| ())
    }

    /// The workspace this object belongs to: from the cached engine, else by
    /// opening it from DO storage (after eviction the cache is empty).
    async fn workspace_id(&self) -> Result<String> {
        if let Some(e) = self.engine.borrow().as_ref() {
            return Ok(e.meta().workspace_id.clone());
        }
        self.with(None, |e| e.meta().workspace_id.clone()).await
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

    /// Fresh authorization for one socket identity: the D1 membership AND the
    /// credential behind it (#65: the session or token row must still be valid,
    /// for this user and, for a token, this workspace). `Ok(Ok(role))` is the role
    /// capped by the credential ([`effective_role`]); `Ok(Err(reason))` means the
    /// socket must be closed; `Err` = D1 unavailable (callers fail closed for edits
    /// and keep sockets open).
    async fn fetch_role(
        &self,
        user_id: i64,
        credential: Option<&str>,
    ) -> Result<std::result::Result<Role, &'static str>> {
        let workspace_id = self.workspace_id().await?;
        #[cfg(feature = "insecure-dev-auth")]
        {
            if self.dev_d1_down.get() {
                return Err(worker::Error::RustError("simulated D1 outage".into()));
            }
            if self.dev_d1_slow.get() {
                worker::Delay::from(std::time::Duration::from_millis(400)).await;
            }
        }
        let db = crate::d1::database(&self.env)?;
        let row = crate::d1::first(&db, &repo::membership_get(&workspace_id, user_id)).await?;
        let Some(role) = row.as_ref().and_then(role_from_row) else {
            return Ok(Err("membership_revoked"));
        };
        let now_s = now_ms().div_euclid(1000);
        match crate::auth_http::revalidate(&db, &workspace_id, user_id, credential, now_s).await? {
            Some(cred) => Ok(Ok(effective_role(role, &cred))),
            None => Ok(Err("credential_revoked")),
        }
    }

    /// The socket's attachment as it is RIGHT NOW, unless it was revoked.
    fn live(&self, ws: &WebSocket) -> Option<Attachment> {
        ws.deserialize_attachment::<Attachment>()
            .ok()
            .flatten()
            .filter(|a| !a.revoked)
    }

    /// Mark revoked, tell the peer and close. Synchronous: the read that fed `att`
    /// and this write have no await between them.
    fn revoke_socket(&self, ws: &WebSocket, att: Attachment, reason: &str) {
        let mut a = att;
        a.revoked = true;
        let _ = ws.serialize_attachment(&a);
        send(ws, &protocol::revoked(reason));
        let _ = ws.close(Some(CLOSE_REVOKED), Some(reason));
    }

    /// Apply a membership result (read BEFORE the caller's await) to the socket's
    /// attachment as it is now. Never writes back a pre-await copy: a revocation
    /// that happened meanwhile stays revoked. `Ok(Some(role))` = still allowed.
    async fn apply_membership(
        &self,
        ws: &WebSocket,
        role: std::result::Result<Role, &'static str>,
        now: i64,
    ) -> Option<Role> {
        let fresh = ws.deserialize_attachment::<Attachment>().ok().flatten();
        let reason = role.err().unwrap_or("membership_revoked");
        let role = role.ok();
        match merge_membership(fresh, role, now) {
            Refresh::Gone => None,
            Refresh::Member(a) => {
                let _ = ws.serialize_attachment(&a);
                role
            }
            Refresh::Removed(a) => {
                let client = a.client_id.clone();
                self.revoke_socket(ws, a, reason);
                self.drop_presence(&client).await;
                None
            }
        }
    }

    /// Re-read membership for `ws`'s user and update/close the socket.
    /// `Ok(Some(role))` = still allowed; `Ok(None)` = revoked/closed (now or earlier).
    async fn recheck(&self, ws: &WebSocket, now: i64) -> Result<Option<Role>> {
        let Some(before) = self.live(ws) else {
            return Ok(None);
        };
        let role = self
            .fetch_role(before.user_id, before.credential.as_deref())
            .await?;
        Ok(self.apply_membership(ws, role, now).await)
    }

    /// Refresh every other connection whose last check is older than the
    /// interval (`force`: all of them). One D1 check per distinct (user, credential).
    async fn recheck_peers(&self, now: i64, force: bool) {
        // One D1 read per distinct (user, credential): two sockets of one user may
        // hold different credentials (a browser session and a CLI token).
        type Seen = (i64, Option<String>, std::result::Result<Role, &'static str>);
        let mut seen: Vec<Seen> = Vec::new();
        for ws in self.state.get_websockets() {
            let Some(att) = self.live(&ws) else {
                continue;
            };
            if !(force || needs_recheck(now, att.authz_checked_ms, self.config.authz_recheck_ms)) {
                continue;
            }
            let role = match seen
                .iter()
                .find(|(u, c, _)| *u == att.user_id && *c == att.credential)
            {
                Some((_, _, r)) => *r,
                None => match self
                    .fetch_role(att.user_id, att.credential.as_deref())
                    .await
                {
                    Ok(r) => {
                        seen.push((att.user_id, att.credential.clone(), r));
                        r
                    }
                    Err(e) => {
                        console_error!("membership recheck failed (keeping socket): {e}");
                        continue;
                    }
                },
            };
            self.apply_membership(&ws, role, now).await;
        }
    }

    /// Worker -> DO internal request: logout, token revocation or rotation
    /// (`credential`: only sockets opened with that tag) and membership removal or
    /// role change (`None`: every socket of the user).
    async fn revoke_user(&self, user_id: i64, credential: Option<&str>) -> usize {
        let mut n = 0;
        for ws in self.state.get_websockets_with_tag(&user_tag(user_id)) {
            // Fresh read, immediate write: no await in between.
            if let Some(att) = self
                .live(&ws)
                .filter(|a| credential.is_none() || a.credential.as_deref() == credential)
            {
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

    /// Send the live presence list to everyone. The read goes through `with` (a
    /// transaction), so a failed read can neither leave a stale failure flag behind
    /// nor be trusted after a rollback. (`with` never suspends when the engine is
    /// cached, so the ordering of the surrounding synchronous block is unchanged.)
    async fn broadcast_presence(&self, now: i64) {
        if let Ok(Ok(rows)) = self.with(None, |e| e.presence_list(now)).await {
            self.broadcast(&protocol::presence(now, &rows), None, now);
        }
    }

    async fn drop_presence(&self, client_id: &str) {
        let removed = self
            .with(None, |e| e.presence_remove(client_id))
            .await
            .is_ok();
        if removed {
            self.broadcast_presence(now_ms()).await;
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
        let sweep = self.config.idle_sweep_ms;
        let planned = match self
            .with(None, |e| e.next_alarm(now, connections, sweep))
            .await
        {
            Ok(Ok(at)) => Some(at),
            _ => None,
        };
        // If the engine cannot open, still arm the idle sweep while sockets are
        // attached: otherwise a removed member's idle socket would never be closed.
        let at = alarm_after(planned, now, connections, sweep);
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
    async fn flush_pending(&self) -> Flush {
        if self.flushing.replace(true) {
            return Flush::Idle;
        }
        let result = self.flush_inner().await;
        self.flushing.set(false);
        result
    }

    /// The workspace owner (author of last resort for a checkpoint whose author
    /// can no longer be referenced). `Err` = D1 unavailable.
    async fn fetch_owner(&self, workspace_id: &str) -> Result<Option<i64>> {
        let db = crate::d1::database(&self.env)?;
        let row = crate::d1::first(&db, &repo::membership_owner(workspace_id)).await?;
        Ok(row
            .as_ref()
            .and_then(|r| r.get("user_id"))
            .and_then(Value::as_i64))
    }

    async fn flush_inner(&self) -> Flush {
        // Two rounds: a permanent failure may be fixed once by re-attribution.
        for _ in 0..2 {
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
                return Flush::Idle;
            };
            let stmt = repo::revision_checkpoint_idempotent(
                &workspace_id,
                &site_id,
                &p.revision_id,
                &p.ir_sha256,
                p.byte_size,
                p.created_by,
            );
            #[cfg(feature = "insecure-dev-auth")]
            let simulated_down = self.dev_ckpt_down.get();
            #[cfg(not(feature = "insecure-dev-auth"))]
            let simulated_down = false;
            let written = if simulated_down {
                Err(worker::Error::RustError("simulated D1 outage".into()))
            } else {
                match crate::d1::database(&self.env) {
                    Ok(db) => crate::d1::run(&db, &stmt).await.map(|_| ()),
                    Err(e) => Err(e),
                }
            };
            let err = match written {
                Ok(()) => {
                    if let Ok(Ok(true)) = self.with(None, |e| e.mark_revision_written(p.seq)).await
                    {
                        self.broadcast(
                            &protocol::checkpoint(p.seq, &p.revision_id, "written", &p.reason),
                            None,
                            now_ms(),
                        );
                    }
                    return Flush::Written;
                }
                Err(e) => e.to_string(),
            };
            console_error!("checkpoint D1 write failed: {err}");
            let owner = if crate::session::engine::is_permanent_d1_error(&err) {
                self.fetch_owner(&workspace_id).await.ok()
            } else {
                None
            };
            match plan_checkpoint_failure(&err, p.created_by, owner) {
                FailurePlan::Retry => {
                    let _ = self.with(None, |e| e.note_revision_failure()).await;
                    return Flush::Pending;
                }
                FailurePlan::Reattribute(owner_id) => {
                    console_error!(
                        "checkpoint {} cannot be written as user {}; re-attributing to workspace owner {owner_id}",
                        p.revision_id,
                        p.created_by
                    );
                    let _ = self.with(None, |e| e.reattribute_pending(owner_id)).await;
                }
                FailurePlan::Abandon => {
                    let _ = self.with(None, |e| e.abandon_pending()).await;
                    console_error!(
                        "ALERT: checkpoint {} (seq {}, workspace {workspace_id}, site {site_id}) can never be written ({err}); dropped so edits resume. The document is intact in the session.",
                        p.revision_id,
                        p.seq
                    );
                    self.broadcast(
                        &protocol::checkpoint(p.seq, &p.revision_id, "abandoned", &p.reason),
                        None,
                        now_ms(),
                    );
                    return Flush::Abandoned;
                }
            }
        }
        Flush::Pending
    }

    // --- message handling ---------------------------------------------------------

    async fn dispatch(&self, ws: &WebSocket, att: Attachment, msg: ClientMessage) -> Result<()> {
        let now = now_ms();
        // Reads and presence: the cached verification is enough while it is
        // fresh; otherwise re-read now. Writes always re-read (below).
        let action = match &msg {
            ClientMessage::Ping
            | ClientMessage::Sync { .. }
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
                match self.recheck(ws, now).await {
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
        // Everything that broadcasts first refreshes stale peers (an await), then
        // re-checks THIS socket was not revoked meanwhile; after that the commit
        // and the broadcast are one synchronous block.
        if matches!(
            msg,
            ClientMessage::Batch { .. }
                | ClientMessage::Undo { .. }
                | ClientMessage::Seed { .. }
                | ClientMessage::Checkpoint { .. }
                | ClientMessage::Presence { .. }
        ) {
            self.recheck_peers(now_ms(), false).await;
        }
        // After every await: the socket as it is NOW (revoked? demoted by a peer's
        // recheck since the role check above?). Immediately before the commit.
        let live = self.live(ws);
        if !allowed_now(live.as_ref(), action) {
            if live.is_some() {
                send(
                    ws,
                    &protocol::error("forbidden", "your role does not allow this", None, None),
                );
            }
            return Ok(());
        }
        let actor = Actor {
            user_id: att.user_id,
            client_id: att.client_id.clone(),
        };
        match msg {
            ClientMessage::Ping => self.on_ping(ws).await,
            ClientMessage::Sync { since_seq } => self.on_sync(ws, since_seq).await,
            ClientMessage::PresenceGet => {
                let now = now_ms();
                let rows = self.with(None, |e| e.presence_list(now)).await?;
                send(ws, &protocol::presence(now, &rows.map_err(engine_err)?));
                Ok(())
            }
            ClientMessage::Presence { state } => {
                // A presence message queued behind a close must not resurrect the
                // closed socket's row (it would linger until the TTL).
                if !socket_open(ws) {
                    return Ok(());
                }
                self.on_presence(ws, &actor, state).await
            }
            ClientMessage::Seed { document } => self.on_seed(ws, &actor, &document).await,
            ClientMessage::Batch {
                op_id,
                base_seq,
                batch,
            } => {
                let now = now_ms();
                let res = self
                    .with(None, |e| {
                        e.submit_batch(&actor, &op_id, base_seq, &batch, now)
                    })
                    .await?;
                self.finish_submit(ws, &op_id, res, now, att.user_id).await
            }
            ClientMessage::Undo { op_id, target_seq } => {
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
                self.broadcast_presence(now).await;
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
        // The flush awaited D1: the sender may have been revoked or demoted meanwhile.
        let action = match reason {
            CheckpointReason::Publish => Action::Publish,
            _ => Action::Save,
        };
        let live = self.live(ws);
        if !allowed_now(live.as_ref(), action) {
            if live.is_some() {
                send(
                    ws,
                    &protocol::error("forbidden", "your role does not allow this", None, None),
                );
            }
            return Ok(());
        }
        let now = now_ms();
        let out = self
            .with(None, |e| e.take_checkpoint(att.user_id, reason, now))
            .await?;
        match out {
            Ok(CheckpointOutcome::Taken(p)) => {
                let flushed = self.flush_pending().await;
                let status = match flushed {
                    Flush::Written => "written",
                    Flush::Abandoned => "abandoned",
                    _ => "pending",
                };
                let m = protocol::checkpoint(p.seq, &p.revision_id, status, reason.as_str());
                if matches!(flushed, Flush::Written | Flush::Abandoned) {
                    // `flush_pending` already broadcast the outcome to everyone.
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
            Ok(CheckpointOutcome::Unchanged { seq, revision_id }) => {
                send(
                    ws,
                    &protocol::checkpoint(seq, &revision_id, "unchanged", reason.as_str()),
                );
            }
            Err(r) => send(ws, &conflict_or_error(None, &r)),
        }
        Ok(())
    }

    /// Test build only: prove `transactionSync` is available and that a failing
    /// body rolls back the DO SQL writes it made.
    #[cfg(feature = "insecure-dev-auth")]
    async fn tx_probe(&self) -> Result<Response> {
        let sql = self.state.storage().sql();
        let ddl = "CREATE TABLE IF NOT EXISTS tx_probe (k TEXT PRIMARY KEY)";
        sql.exec(ddl, None)?;
        sql.exec("DELETE FROM tx_probe", None)?;
        let sql2 = sql.clone();
        let mut body = move || -> std::result::Result<(), ()> {
            sql2.exec("INSERT INTO tx_probe (k) VALUES ('rolled-back')", None)
                .map_err(|_| ())?;
            Err(())
        };
        let available = worker::js_sys::Reflect::get(
            &JsValue::from(
                self.state
                    .as_raw()
                    .storage()
                    .map_err(|_| worker::Error::RustError("no storage".into()))?,
            ),
            &JsValue::from_str("transactionSync"),
        )
        .ok()
        .is_some_and(|f| f.is_function());
        let _ = self.transaction_sync(&mut body);
        let sql3 = self.state.storage().sql();
        let n: Vec<serde_json::Value> = sql3
            .exec("SELECT COUNT(*) AS n FROM tx_probe", None)?
            .to_array()?;
        let rows = n
            .first()
            .and_then(|r| r.get("n"))
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        Response::from_json(&serde_json::json!({
            "transaction_sync_available": available,
            "rows_after_failed_body": rows,
        }))
    }
}

fn engine_err(e: EngineError) -> worker::Error {
    worker::Error::RustError(e.to_string())
}
