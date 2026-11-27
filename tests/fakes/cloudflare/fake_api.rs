//! In-memory fake of the Cloudflare Workers API surface used by Solo's Direct Upload
//! publish pipeline (`crates/sws-worker-studio/src/publish`). Host tests drive the real
//! client through the injected `HttpClient` trait; nothing here touches a network.
//!
//! Modelled on the Cloudflare docs as read on 2026-11-27:
//! - https://developers.cloudflare.com/workers/static-assets/direct-upload/
//! - https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/assets/subresources/upload/methods/create/
//! - https://developers.cloudflare.com/api/resources/workers/subresources/assets/subresources/upload/methods/create/
//! - https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/versions/methods/create/
//! - https://developers.cloudflare.com/api/resources/workers/subresources/scripts/methods/update/ (assets.config fields)
//! - https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/deployments/methods/create/
//! - https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/deployments/methods/list/
//! - https://developers.cloudflare.com/workers/configuration/multipart-upload-metadata/
//!
//! What it enforces (each mirrors a documented behaviour):
//! - upload session: `{manifest: {path: {hash, size}}}` -> `{jwt, buckets}`; hashes the
//!   account already holds are NOT returned in `buckets`; when nothing is missing,
//!   `buckets` is empty and `jwt` is already a completion token.
//! - upload: `POST /workers/assets/upload?base64=true`, multipart, `Authorization: Bearer
//!   <upload jwt>` (never the account token); part name = hash, body = base64 content;
//!   201 + completion `jwt` once every pending file is in, 200 before that. The fake
//!   re-derives each hash (sha256(base64 + extension), first 32 hex) and rejects a mismatch.
//! - version upload: multipart with a JSON `metadata` part whose `assets.jwt` must be a
//!   completion token, and a module part named by `main_module`.
//! - deployments: create `{strategy: "percentage", versions: [{version_id, percentage}]}`
//!   (sum 100, versions must exist); list returns newest first ("the first deployment in
//!   the list is the latest deployment actively serving traffic").
//!
//! Assumptions the docs do not pin down (kept deliberately simple here): completion
//! tokens are reusable within the fake; error codes are representative, not exact.
//!
//! Fault injection: [`Fault`]s queued per [`Op`] fire on the next matching request,
//! before (`Status`, `TransportBefore`, `EchoAuth`) or after (`TransportAfter`,
//! `StatusAfter`) the request takes effect, so tests can produce ambiguous outcomes.
//! One-shot hooks run when a request of a given op arrives (before it is processed),
//! letting a test interleave a second publish job deterministically.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Value, json};

use crate::publish::http::{HttpClient, HttpRequest, HttpResponse, TransportError};

pub const FAKE_BASE: &str = "https://api.cloudflare.test/client/v4";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Op {
    UploadSession,
    UploadAssets,
    CreateVersion,
    CreateDeployment,
    ListDeployments,
}

#[derive(Debug, Clone)]
pub enum Fault {
    /// Reject before taking effect.
    Status(u16),
    /// 429/503 with a `Retry-After` (seconds), before taking effect.
    RetryAfter(u16, u64),
    /// No response; the request never took effect.
    TransportBefore,
    /// The request took effect, then the connection died (the ambiguous case).
    TransportAfter,
    /// The request took effect, then the server answered with this status.
    StatusAfter(u16),
    /// Reject, echoing the Authorization header in the error message (hostile upstream).
    EchoAuth(u16),
}

#[derive(Debug, Clone)]
pub struct Logged {
    pub op: Op,
    pub method: &'static str,
    pub url: String,
    pub authorization: Option<String>,
}

struct Session {
    manifest: BTreeMap<String, (String, u64)>,
    pending: BTreeSet<String>,
}

pub struct State {
    pub account_id: String,
    pub script: String,
    pub token: String,
    /// Content hashes the account already holds (dedup across versions).
    pub stored: BTreeSet<String>,
    sessions: BTreeMap<String, Session>,
    completions: BTreeSet<String>,
    /// version id -> metadata JSON (as uploaded).
    pub versions: BTreeMap<String, Value>,
    /// version id -> module part names.
    pub modules: BTreeMap<String, Vec<String>>,
    /// Newest first.
    pub deployments: Vec<Value>,
    faults: BTreeMap<Op, VecDeque<Fault>>,
    pub log: Vec<Logged>,
    /// Every hash received by the upload endpoint, in order.
    pub uploaded: Vec<String>,
    pub bucket_size: usize,
    counter: u64,
}

impl State {
    pub fn reset_faults(&mut self) {
        self.faults.clear();
    }
}

type Hook = Box<dyn FnOnce()>;

pub struct FakeCloudflare {
    pub state: RefCell<State>,
    hooks: RefCell<BTreeMap<Op, Hook>>,
}

pub fn uuid(n: u64) -> String {
    format!("{:08x}-0000-4000-8000-{:012x}", 0x5010_0000u64 + n, n)
}

impl FakeCloudflare {
    /// A Worker that already exists (created once by `wrangler deploy`) with one
    /// version live at 100%.
    pub fn new(account_id: &str, script: &str, token: &str) -> Self {
        let initial = uuid(0);
        let mut versions = BTreeMap::new();
        versions.insert(initial.clone(), json!({ "main_module": "index.js" }));
        Self {
            state: RefCell::new(State {
                account_id: account_id.into(),
                script: script.into(),
                token: token.into(),
                stored: BTreeSet::new(),
                sessions: BTreeMap::new(),
                completions: BTreeSet::new(),
                versions,
                modules: BTreeMap::new(),
                deployments: vec![deployment_json("dep-0", &initial)],
                faults: BTreeMap::new(),
                log: Vec::new(),
                uploaded: Vec::new(),
                bucket_size: 2,
                counter: 1,
            }),
            hooks: RefCell::new(BTreeMap::new()),
        }
    }

    pub fn initial_version(&self) -> String {
        uuid(0)
    }

    pub fn fault(&self, op: Op, f: Fault) {
        self.state
            .borrow_mut()
            .faults
            .entry(op)
            .or_default()
            .push_back(f);
    }

    pub fn on(&self, op: Op, f: impl FnOnce() + 'static) {
        self.hooks.borrow_mut().insert(op, Box::new(f));
    }

    /// Version id currently serving 100% of traffic (first deployment, single version).
    pub fn live_version(&self) -> Option<String> {
        let st = self.state.borrow();
        let d = st.deployments.first()?;
        let vs = d["versions"].as_array()?;
        (vs.len() == 1 && vs[0]["percentage"].as_f64() == Some(100.0))
            .then(|| vs[0]["version_id"].as_str().map(str::to_string))
            .flatten()
    }

    pub fn ops(&self) -> Vec<Op> {
        self.state.borrow().log.iter().map(|l| l.op).collect()
    }

    pub fn count(&self, op: Op) -> usize {
        self.ops().iter().filter(|o| **o == op).count()
    }

    /// Simulate a deploy made outside Solo (dashboard, wrangler).
    pub fn external_deploy(&self, version_id: &str) {
        let mut st = self.state.borrow_mut();
        st.versions
            .entry(version_id.to_string())
            .or_insert_with(|| json!({ "main_module": "index.js" }));
        st.deployments
            .insert(0, deployment_json("dep-external", version_id));
    }

    fn classify(&self, req: &HttpRequest) -> Option<Op> {
        let st = self.state.borrow();
        let rest = req.url.strip_prefix(FAKE_BASE)?;
        let acct = format!("/accounts/{}/workers/", st.account_id);
        let rest = rest.strip_prefix(&acct)?;
        if rest == "assets/upload?base64=true" && req.method == "POST" {
            return Some(Op::UploadAssets);
        }
        let script = format!("scripts/{}/", st.script);
        match (req.method, rest.strip_prefix(&script)?) {
            ("POST", "assets-upload-session") => Some(Op::UploadSession),
            ("POST", "versions") => Some(Op::CreateVersion),
            ("POST", "deployments") => Some(Op::CreateDeployment),
            ("GET", "deployments") => Some(Op::ListDeployments),
            _ => None,
        }
    }

    fn next_id(&self) -> u64 {
        let mut st = self.state.borrow_mut();
        st.counter += 1;
        st.counter
    }

    fn process(&self, op: Op, req: &HttpRequest) -> HttpResponse {
        let auth = req.header("Authorization").map(str::to_string);
        let account_auth = format!("Bearer {}", self.state.borrow().token);
        if op != Op::UploadAssets && auth.as_deref() != Some(account_auth.as_str()) {
            return err(403, 10000, "Authentication error");
        }
        match op {
            Op::UploadSession => self.upload_session(req),
            Op::UploadAssets => self.upload_assets(req, auth.as_deref()),
            Op::CreateVersion => self.create_version(req),
            Op::CreateDeployment => self.create_deployment(req),
            Op::ListDeployments => {
                let st = self.state.borrow();
                ok(200, json!({ "deployments": st.deployments }))
            }
        }
    }

    fn upload_session(&self, req: &HttpRequest) -> HttpResponse {
        let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
            return err(400, 10001, "invalid JSON");
        };
        let Some(m) = body["manifest"].as_object() else {
            return err(400, 10001, "manifest required");
        };
        let mut manifest = BTreeMap::new();
        for (path, e) in m {
            let (Some(hash), Some(size)) = (e["hash"].as_str(), e["size"].as_u64()) else {
                return err(400, 10001, "bad manifest entry");
            };
            if !path.starts_with('/')
                || hash.len() != 32
                || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return err(400, 10001, "bad manifest entry");
            }
            manifest.insert(path.clone(), (hash.to_string(), size));
        }
        let missing: BTreeSet<String> = {
            let st = self.state.borrow();
            manifest
                .values()
                .map(|(h, _)| h.clone())
                .filter(|h| !st.stored.contains(h))
                .collect()
        };
        let n = self.next_id();
        let mut st = self.state.borrow_mut();
        if missing.is_empty() {
            let jwt = format!("completion-jwt-{n}");
            st.completions.insert(jwt.clone());
            return ok(200, json!({ "jwt": jwt, "buckets": [] }));
        }
        let all: Vec<String> = missing.iter().cloned().collect();
        let buckets: Vec<Vec<String>> = all.chunks(st.bucket_size).map(|c| c.to_vec()).collect();
        let jwt = format!("upload-jwt-{n}");
        st.sessions.insert(
            jwt.clone(),
            Session {
                manifest,
                pending: missing,
            },
        );
        ok(200, json!({ "jwt": jwt, "buckets": buckets }))
    }

    fn upload_assets(&self, req: &HttpRequest, auth: Option<&str>) -> HttpResponse {
        let Some(jwt) = auth.and_then(|a| a.strip_prefix("Bearer ")) else {
            return err(401, 10000, "missing upload token");
        };
        let Some(parts) = parse_multipart(req) else {
            return err(400, 10001, "multipart/form-data required");
        };
        let n = self.next_id();
        let mut st = self.state.borrow_mut();
        let State {
            sessions,
            stored,
            uploaded,
            completions,
            ..
        } = &mut *st;
        let Some(session) = sessions.get_mut(jwt) else {
            return err(401, 10000, "invalid upload token");
        };
        for p in &parts {
            // Each file part carries its Content-Type (served with the asset).
            if p.filename.as_deref() != Some(p.name.as_str()) || p.content_type.is_none() {
                return err(400, 10001, "file part needs filename and Content-Type");
            }
            if !session.pending.contains(&p.name) {
                return err(400, 10001, "hash not pending in this session");
            }
            let Some((path, size)) = session
                .manifest
                .iter()
                .find(|(_, (h, _))| *h == p.name)
                .map(|(path, (_, s))| (path.clone(), *s))
            else {
                return err(400, 10001, "unknown hash");
            };
            let Some(decoded) = base64_decode(&p.body) else {
                return err(400, 10001, "body is not base64");
            };
            if decoded.len() as u64 != size {
                return err(400, 10001, "size mismatch");
            }
            let ext = path
                .rsplit('/')
                .next()
                .and_then(|f| f.rsplit_once('.'))
                .map_or("", |(_, e)| e);
            let mut pre = p.body.clone();
            pre.extend_from_slice(ext.as_bytes());
            let h = &crate::session::sha256::sha256_hex(&pre)[..32];
            if h != p.name {
                return err(400, 10001, "hash mismatch");
            }
        }
        for p in parts {
            session.pending.remove(&p.name);
            stored.insert(p.name.clone());
            uploaded.push(p.name);
        }
        if session.pending.is_empty() {
            sessions.remove(jwt);
            let c = format!("completion-jwt-{n}");
            completions.insert(c.clone());
            return ok(201, json!({ "jwt": c }));
        }
        ok(200, json!({}))
    }

    fn create_version(&self, req: &HttpRequest) -> HttpResponse {
        let Some(parts) = parse_multipart(req) else {
            return err(400, 10001, "multipart/form-data required");
        };
        let Some(meta) = parts
            .iter()
            .find(|p| p.name == "metadata")
            .and_then(|p| serde_json::from_slice::<Value>(&p.body).ok())
        else {
            return err(400, 10021, "metadata part required");
        };
        let Some(main) = meta["main_module"].as_str() else {
            return err(400, 10021, "main_module required");
        };
        let modules: Vec<String> = parts
            .iter()
            .filter(|p| p.name != "metadata")
            .map(|p| p.name.clone())
            .collect();
        if parts
            .iter()
            .filter(|p| p.name != "metadata")
            .any(|p| p.filename.as_deref() != Some(p.name.as_str()) || p.content_type.is_none())
        {
            return err(400, 10021, "module part needs filename and Content-Type");
        }
        if !modules.iter().any(|m| m == main) {
            return err(400, 10021, "main_module part missing");
        }
        let jwt = meta["assets"]["jwt"].as_str().unwrap_or_default();
        if !self.state.borrow().completions.contains(jwt) {
            return err(400, 10021, "assets completion token invalid");
        }
        let id = uuid(self.next_id());
        let mut st = self.state.borrow_mut();
        st.versions.insert(id.clone(), meta);
        st.modules.insert(id.clone(), modules);
        ok(200, json!({ "id": id, "number": st.versions.len() }))
    }

    fn create_deployment(&self, req: &HttpRequest) -> HttpResponse {
        let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
            return err(400, 10001, "invalid JSON");
        };
        let vs = body["versions"].as_array().cloned().unwrap_or_default();
        let sum: f64 = vs.iter().filter_map(|v| v["percentage"].as_f64()).sum();
        if body["strategy"] != "percentage" || vs.is_empty() || sum != 100.0 {
            return err(400, 10001, "invalid deployment");
        }
        let n = self.next_id();
        let mut st = self.state.borrow_mut();
        for v in &vs {
            let id = v["version_id"].as_str().unwrap_or_default();
            if !st.versions.contains_key(id) {
                return err(400, 10230, "version not found");
            }
        }
        let d = json!({
            "id": format!("dep-{n}"),
            "created_on": "2026-11-27T00:00:00Z",
            "source": "api",
            "strategy": "percentage",
            "versions": vs,
        });
        st.deployments.insert(0, d.clone());
        ok(200, d)
    }
}

fn deployment_json(id: &str, version: &str) -> Value {
    json!({
        "id": id,
        "created_on": "2026-11-27T00:00:00Z",
        "source": "wrangler",
        "strategy": "percentage",
        "versions": [{ "version_id": version, "percentage": 100 }],
    })
}

fn ok(status: u16, result: Value) -> HttpResponse {
    HttpResponse {
        status,
        retry_after_secs: None,
        body: serde_json::to_vec(
            &json!({ "success": true, "errors": [], "messages": [], "result": result }),
        )
        .unwrap_or_default(),
    }
}

fn err(status: u16, code: i64, message: &str) -> HttpResponse {
    HttpResponse {
        status,
        retry_after_secs: None,
        body: serde_json::to_vec(&json!({
            "success": false,
            "errors": [{ "code": code, "message": message }],
            "messages": [],
            "result": null,
        }))
        .unwrap_or_default(),
    }
}

impl HttpClient for FakeCloudflare {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let Some(op) = self.classify(&req) else {
            return Ok(err(404, 7003, "no route"));
        };
        let hook = self.hooks.borrow_mut().remove(&op);
        if let Some(h) = hook {
            h();
        }
        let fault = self
            .state
            .borrow_mut()
            .faults
            .get_mut(&op)
            .and_then(VecDeque::pop_front);
        self.state.borrow_mut().log.push(Logged {
            op,
            method: req.method,
            url: req.url.clone(),
            authorization: req.header("Authorization").map(str::to_string),
        });
        match &fault {
            Some(Fault::TransportBefore) => return Err(TransportError::Timeout),
            Some(Fault::Status(s)) => return Ok(err(*s, 10013, "injected failure")),
            Some(Fault::RetryAfter(s, secs)) => {
                let mut r = err(*s, 10429, "slow down");
                r.retry_after_secs = Some(*secs);
                return Ok(r);
            }
            Some(Fault::EchoAuth(s)) => {
                let echoed = format!(
                    "bad credentials: {}",
                    req.header("Authorization").unwrap_or_default()
                );
                return Ok(err(*s, 10000, &echoed));
            }
            _ => {}
        }
        let resp = self.process(op, &req);
        match fault {
            Some(Fault::TransportAfter) => Err(TransportError::Network(format!(
                "connection reset after {} (Authorization: {})",
                req.url,
                req.header("Authorization").unwrap_or_default()
            ))),
            Some(Fault::StatusAfter(s)) => Ok(err(s, 10013, "injected failure after effect")),
            _ => Ok(resp),
        }
    }
}

// --- multipart / base64 decoding (fake side only) ---------------------------------

pub struct Part {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

pub fn parse_multipart(req: &HttpRequest) -> Option<Vec<Part>> {
    let ct = req.header("Content-Type")?;
    let boundary = ct
        .strip_prefix("multipart/form-data; boundary=")?
        .trim_matches('"');
    let delim = format!("--{boundary}");
    let body = &req.body;
    let mut parts = Vec::new();
    let mut pos = find(body, delim.as_bytes(), 0)? + delim.len();
    loop {
        if body.get(pos..pos + 2) == Some(b"--") {
            return Some(parts);
        }
        if body.get(pos..pos + 2) != Some(b"\r\n") {
            return None;
        }
        pos += 2;
        let head_end = find(body, b"\r\n\r\n", pos)?;
        let head = std::str::from_utf8(&body[pos..head_end]).ok()?;
        let next = find(body, format!("\r\n{delim}").as_bytes(), head_end + 4)?;
        let content = body[head_end + 4..next].to_vec();
        let mut name = None;
        let mut filename = None;
        let mut content_type = None;
        for line in head.split("\r\n") {
            let (k, v) = line.split_once(':')?;
            if k.eq_ignore_ascii_case("Content-Disposition") {
                for attr in v.split(';').map(str::trim) {
                    if let Some(n) = attr.strip_prefix("name=") {
                        name = Some(n.trim_matches('"').to_string());
                    } else if let Some(f) = attr.strip_prefix("filename=") {
                        filename = Some(f.trim_matches('"').to_string());
                    }
                }
            } else if k.eq_ignore_ascii_case("Content-Type") {
                content_type = Some(v.trim().to_string());
            }
        }
        parts.push(Part {
            name: name?,
            filename,
            content_type,
            body: content,
        });
        pos = next + 2 + delim.len();
    }
}

pub fn base64_decode(s: &[u8]) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    if !s.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for chunk in s.chunks(4) {
        let pad = chunk.iter().rev().take_while(|c| **c == b'=').count();
        if pad > 2 {
            return None;
        }
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = if i >= 4 - pad { 0 } else { val(*c)? };
            n = (n << 6) | v;
        }
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}
