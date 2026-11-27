//! Host tests for the Direct Upload publish pipeline and rollback, driven through the
//! injected seams against the Cloudflare fake in `tests/fakes/cloudflare/fake_api.rs`
//! (doc URLs and date in its header). No network.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use serde_json::Value;

#[path = "../../../../tests/fakes/cloudflare/fake_api.rs"]
mod fake_api;

use fake_api::{FAKE_BASE, FakeCloudflare, Fault, Op};

use super::http::{Runtime, Secrets};
use super::memstore::MemStore;
use super::pipeline::{WorkerBundle, WorkerModule};
use super::secret::ApiToken;
use super::store::{AuditAction, AuditResult, LeaseClaim, PublishStore, Target};
use super::*;
use crate::auth::Principal;
use crate::model::Role;

const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";
const SCRIPT: &str = "hearth-site";
const TOKEN: &str = "cfat_SUPERSECRET_token_value_0123456789";
const WS: &str = "ws-a";
const SITE: &str = "site-a";
const T0: u64 = 1_795_737_600; // 2026-11-27T00:00:00Z

/// The fakes never return Pending, so one poll completes every future.
fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("fake I/O must complete synchronously"),
    }
}

#[derive(Default)]
struct Rt {
    now: Cell<u64>,
    sleeps: RefCell<Vec<u64>>,
    logs: RefCell<Vec<String>>,
}

impl Runtime for Rt {
    fn now_secs(&self) -> u64 {
        self.now.get()
    }
    async fn sleep_ms(&self, ms: u64) {
        self.sleeps.borrow_mut().push(ms);
        self.now.set(self.now.get() + ms.div_ceil(1000));
    }
    fn log(&self, line: &str) {
        self.logs.borrow_mut().push(line.to_string());
    }
}

struct Keys(Option<&'static str>);

impl Secrets for Keys {
    fn api_token(&self, name: &str) -> Option<ApiToken> {
        (name == DEFAULT_TOKEN_SECRET)
            .then_some(self.0)
            .flatten()
            .map(|t| ApiToken::new(t.to_string()))
    }
}

#[derive(Clone)]
struct Art {
    files: BTreeMap<String, Vec<u8>>,
    worker: WorkerBundle,
    schema: i64,
}

impl ArtifactSource for Art {
    fn paths(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }
    async fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("missing {path}"))
    }
    fn worker(&self) -> &WorkerBundle {
        &self.worker
    }
    fn schema_level(&self) -> i64 {
        self.schema
    }
}

fn art(home: &str) -> Art {
    let mut files = BTreeMap::new();
    files.insert(
        "/index.html".into(),
        format!("<h1>{home}</h1>").into_bytes(),
    );
    files.insert("/menu/index.html".into(), b"<h1>Menu</h1>".to_vec());
    files.insert("/styles.css".into(), b"body{color:#241207}".to_vec());
    files.insert("/img/logo.svg".into(), b"<svg/>".to_vec());
    files.insert(
        "/_headers".into(),
        b"/*\n  X-Frame-Options: DENY\n  X-Content-Type-Options: nosniff\n".to_vec(),
    );
    Art {
        files,
        worker: WorkerBundle {
            main_module: "index.js".into(),
            modules: vec![
                WorkerModule {
                    name: "index.js".into(),
                    content_type: "application/javascript+module".into(),
                    bytes: b"import w from './index_bg.wasm'; export default {}".to_vec(),
                },
                WorkerModule {
                    name: "index_bg.wasm".into(),
                    content_type: "application/wasm".into(),
                    bytes: b"\0asm\x01\0\0\0".to_vec(),
                },
            ],
        },
        schema: 4,
    }
}

const BINDINGS: &str = r#"[
  {"type":"d1","name":"DB","id":"00000000-0000-0000-0000-000000000001"},
  {"type":"plain_text","name":"TURNSTILE_SITE_KEY","text":"1x00000000000000000000AA"},
  {"type":"ratelimit","name":"BOOKING_SLOTS_LIMIT","namespace_id":"1001","simple":{"limit":20,"period":60}}
]"#;

#[derive(Clone)]
struct World {
    cf: Rc<FakeCloudflare>,
    rt: Rc<Rt>,
    store: Rc<MemStore>,
    keys: Rc<Keys>,
}

impl World {
    fn env(&self) -> PublishEnv<'_, FakeCloudflare, Rt, MemStore, Keys> {
        PublishEnv {
            http: &self.cf,
            rt: &self.rt,
            store: &self.store,
            secrets: &self.keys,
            api_base: FAKE_BASE,
        }
    }

    fn publish(&self, seq: i64, a: &Art) -> Result<PublishOutcome, PublishFailure> {
        block_on(run_publish(&job(seq), a, &self.env()))
    }

    fn rollback(&self, user: i64, published_id: &str) -> Result<RollbackOutcome, RollbackError> {
        let req = RollbackRequest {
            workspace_id: WS.into(),
            site_id: SITE.into(),
            published_id: published_id.into(),
            principal: Principal { user_id: user },
        };
        block_on(run_rollback(&req, &self.env()))
    }

    fn target(&self) -> Target {
        self.store.target_now()
    }

    fn version_meta(&self, id: &str) -> Value {
        self.cf.state.borrow().versions[id].clone()
    }

    fn secrets_seen(&self) -> Vec<String> {
        let mut out = vec![TOKEN.to_string()];
        for l in &self.cf.state.borrow().log {
            if let Some(a) = &l.authorization {
                out.push(a.trim_start_matches("Bearer ").to_string());
            }
        }
        out
    }
}

fn world() -> World {
    let cf = FakeCloudflare::new(ACCOUNT, SCRIPT, TOKEN);
    let store = MemStore::default();
    {
        let mut s = store.s.borrow_mut();
        s.targets.push(Target {
            workspace_id: WS.into(),
            site_id: SITE.into(),
            id: "target-a".into(),
            account_id: ACCOUNT.into(),
            script_name: SCRIPT.into(),
            token_secret: DEFAULT_TOKEN_SECRET.into(),
            bindings_json: BINDINGS.into(),
            compatibility_date: "2026-11-01".into(),
            site_schema_level: 4,
            live_version_id: Some(cf.initial_version()),
            live_published_id: None,
            live_revision_id: None,
            high_water_seq: 0,
            activating_job: None,
            activating_seq: None,
            activating_version_id: None,
            lease_expires_at: None,
        });
        s.roles.insert((WS.into(), 1), Role::Owner);
        s.roles.insert((WS.into(), 2), Role::Publisher);
        s.roles.insert((WS.into(), 3), Role::Editor);
    }
    let rt = Rt::default();
    rt.now.set(T0);
    World {
        cf: Rc::new(cf),
        rt: Rc::new(rt),
        store: Rc::new(store),
        keys: Rc::new(Keys(Some(TOKEN))),
    }
}

fn job(seq: i64) -> PublishJobRef {
    PublishJobRef {
        workspace_id: WS.into(),
        site_id: SITE.into(),
        job_id: format!("job-{seq}"),
        revision_id: format!("rev-{seq}"),
        revision_seq: seq,
        requested_by: 2,
        artifact_sha256: None,
    }
}

fn no_two_activations_without_reconcile(ops: &[Op]) {
    let mut last_was_deploy = false;
    for op in ops {
        match op {
            Op::CreateDeployment => {
                assert!(!last_was_deploy, "activation retried blindly: {ops:?}");
                last_was_deploy = true;
            }
            Op::ListDeployments => last_was_deploy = false,
            _ => {}
        }
    }
}

// --- happy path, bindings, standalone config ----------------------------------------

#[test]
fn publishes_through_session_upload_version_and_deployment() {
    let w = world();
    let out = w.publish(1, &art("Home")).expect("publish");
    assert_eq!(
        w.cf.ops(),
        vec![
            Op::UploadSession,
            Op::UploadAssets,
            Op::UploadAssets,
            Op::CreateVersion,
            Op::CreateDeployment,
            Op::ListDeployments,
        ]
    );
    assert_eq!(w.cf.live_version(), Some(out.cf_version_id.clone()));
    assert_eq!(
        (out.uploaded_files, out.total_files),
        (4, 4),
        "_headers is config, not an asset"
    );
    let t = w.target();
    assert_eq!(
        t.live_version_id.as_deref(),
        Some(out.cf_version_id.as_str())
    );
    assert_eq!(
        t.live_published_id.as_deref(),
        Some(out.published_id.as_str())
    );
    assert_eq!(t.live_revision_id.as_deref(), Some("rev-1"));
    assert_eq!(t.high_water_seq, 1);
    assert_eq!(t.activating_job, None, "lease released");
    let s = w.store.s.borrow();
    assert_eq!(s.published.len(), 1);
    assert_eq!(s.published[0].schema_compat, 4);
    assert_eq!(s.published[0].revision_id, "rev-1");
    assert_eq!(s.published[0].manifest_sha256, out.manifest_sha256);
    let a = s.audit.last().expect("audit row");
    assert_eq!(
        (a.action, a.result),
        (AuditAction::PublishActivate, AuditResult::Succeeded)
    );
    assert_eq!(a.actor_user_id, Some(2));
    // The upload endpoint only ever sees the upload JWT; the account token never.
    for l in &w.cf.state.borrow().log {
        assert_eq!(l.method == "GET", l.op == Op::ListDeployments, "{:?}", l.op);
        let auth = l.authorization.as_deref().unwrap_or_default();
        if l.op == Op::UploadAssets {
            assert!(auth.starts_with("Bearer upload-jwt-"), "{auth}");
        } else {
            assert_eq!(auth, format!("Bearer {TOKEN}"));
        }
    }
}

#[test]
fn version_preserves_run_worker_first_d1_turnstile_and_secrets() {
    let w = world();
    let out = w.publish(1, &art("Home")).expect("publish");
    let meta = w.version_meta(&out.cf_version_id);
    assert_eq!(meta["main_module"], "index.js");
    assert_eq!(meta["compatibility_date"], "2026-11-01");
    let cfg = &meta["assets"]["config"];
    assert_eq!(cfg["run_worker_first"], serde_json::json!(["/api/*"]));
    assert_eq!(cfg["not_found_handling"], "404-page");
    assert_eq!(cfg["html_handling"], "auto-trailing-slash");
    assert!(
        cfg["_headers"]
            .as_str()
            .unwrap_or_default()
            .contains("X-Frame-Options: DENY"),
        "_headers travels as assets.config._headers"
    );
    let bindings = meta["bindings"].as_array().expect("bindings");
    let find = |name: &str| bindings.iter().find(|b| b["name"] == name).cloned();
    assert_eq!(
        find("ASSETS").map(|b| b["type"].clone()),
        Some("assets".into())
    );
    let db = find("DB").expect("D1 binding preserved");
    assert_eq!(
        (db["type"].as_str(), db["database_id"].as_str()),
        (Some("d1"), Some("00000000-0000-0000-0000-000000000001"))
    );
    assert_eq!(
        find("TURNSTILE_SITE_KEY").map(|b| b["type"].clone()),
        Some("plain_text".into())
    );
    assert_eq!(
        find("BOOKING_SLOTS_LIMIT").map(|b| b["type"].clone()),
        Some("ratelimit".into())
    );
    let keep: Vec<&str> = meta["keep_bindings"]
        .as_array()
        .expect("keep_bindings")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        keep.contains(&"secret_text"),
        "TURNSTILE_SECRET_KEY stays on the Worker"
    );
    assert_eq!(meta["annotations"]["workers/tag"], "rev-1");
    let mods = w.cf.state.borrow().modules[&out.cf_version_id].clone();
    assert_eq!(
        mods,
        vec!["index.js".to_string(), "index_bg.wasm".to_string()]
    );
}

/// "Published requests must work when Studio is unavailable": the uploaded version is
/// self-contained. Nothing in its config can reach Studio: no service, Durable Object,
/// dispatch or other cross-Worker binding, no Studio origin anywhere, and the only code
/// is the site Worker bundle. (Serving with Studio stopped on real Cloudflare is human
/// evidence, #69.)
#[test]
fn published_version_has_no_binding_or_route_to_studio() {
    let w = world();
    let out = w.publish(1, &art("Home")).expect("publish");
    let meta = w.version_meta(&out.cf_version_id);
    let allowed = ["assets", "d1", "plain_text", "ratelimit"];
    for b in meta["bindings"].as_array().expect("bindings") {
        let ty = b["type"].as_str().unwrap_or_default();
        assert!(allowed.contains(&ty), "unexpected binding type {ty}");
    }
    let text = meta.to_string().to_lowercase();
    for needle in [
        "studio",
        "sws-studio-control-plane",
        "site_session",
        "127.0.0.1:8788",
    ] {
        assert!(
            !text.contains(needle),
            "version config mentions {needle}: {text}"
        );
    }
    // Only /api/* reaches the Worker; every other path is served from static assets.
    assert_eq!(
        meta["assets"]["config"]["run_worker_first"],
        serde_json::json!(["/api/*"])
    );
}

#[test]
fn target_bindings_that_could_reach_studio_or_carry_secrets_are_refused() {
    for (bad, why) in [
        (
            r#"[{"type":"service","name":"STUDIO","service":"sws-studio-control-plane"}]"#,
            "service",
        ),
        (
            r#"[{"type":"durable_object_namespace","name":"S","class_name":"SiteSession"}]"#,
            "do",
        ),
        (
            r#"[{"type":"secret_text","name":"TURNSTILE_SECRET_KEY","text":"x"}]"#,
            "secret",
        ),
        (r#"[{"type":"assets","name":"ASSETS"}]"#, "assets is Solo's"),
        (
            r#"[{"type":"plain_text","name":"TURNSTILE_SITE_KEY","text":"k"}]"#,
            "no DB",
        ),
        (
            r#"[{"type":"d1","name":"DB","id":"00000000-0000-0000-0000-000000000001"}]"#,
            "no turnstile key",
        ),
        (r#"{"type":"d1"}"#, "not an array"),
    ] {
        let w = world();
        w.store.s.borrow_mut().targets[0].bindings_json = bad.into();
        let e = w.publish(1, &art("Home")).expect_err(why);
        assert_eq!(
            (e.stage, e.code),
            (Stage::Prepare, "invalid_target_bindings"),
            "{why}"
        );
        assert!(
            w.cf.ops().is_empty(),
            "{why}: no API call before validation"
        );
    }
}

// --- dedup ---------------------------------------------------------------------------

#[test]
fn identical_republish_uploads_zero_files_and_one_change_uploads_one() {
    let w = world();
    w.publish(1, &art("Home")).expect("first");
    let first_uploads = w.cf.state.borrow().uploaded.len();
    assert_eq!(first_uploads, 4);

    let out = w.publish(2, &art("Home")).expect("identical");
    assert_eq!(out.uploaded_files, 0);
    assert_eq!(
        w.cf.state.borrow().uploaded.len(),
        first_uploads,
        "zero files uploaded"
    );
    let ops = w.cf.ops();
    assert_eq!(
        ops.iter().filter(|o| **o == Op::UploadAssets).count(),
        2,
        "only the first publish uploaded"
    );
    assert_eq!(w.target().high_water_seq, 2);

    let out = w.publish(3, &art("New home")).expect("one change");
    assert_eq!(out.uploaded_files, 1);
    assert_eq!(w.cf.state.borrow().uploaded.len(), first_uploads + 1);
    // Every path is still in the new version's manifest (unchanged files included).
    assert_eq!(out.total_files, 4);
}

// --- failures before activation ------------------------------------------------------

#[test]
fn every_pre_activation_failure_keeps_the_last_good_version_and_reports_stage_and_revision() {
    for (op, stage) in [
        (Op::UploadSession, Stage::UploadSession),
        (Op::UploadAssets, Stage::UploadAssets),
        (Op::CreateVersion, Stage::CreateVersion),
    ] {
        let w = world();
        let good = w.publish(1, &art("Good")).expect("good");
        w.cf.fault(op, Fault::Status(400));
        let e = w.publish(2, &art("Broken")).expect_err("fails");
        assert_eq!(e.stage, stage, "{op:?}");
        assert_eq!((e.revision_id.as_str(), e.revision_seq), ("rev-2", 2));
        assert_eq!(e.activation, Activation::NotAttempted);
        assert!(!e.retryable, "a 400 is not retryable");
        assert_eq!(
            e.live_version_id.as_deref(),
            Some(good.cf_version_id.as_str())
        );
        assert_eq!(
            w.cf.live_version(),
            Some(good.cf_version_id.clone()),
            "live untouched"
        );
        let t = w.target();
        assert_eq!(t.live_version_id, Some(good.cf_version_id.clone()));
        assert_eq!((t.high_water_seq, t.activating_job), (1, None));
        assert_eq!(
            w.cf.count(Op::CreateDeployment),
            1,
            "only the good publish activated"
        );
        assert!(e.to_string().contains(stage.as_str()) && e.to_string().contains("rev-2"));
    }
}

#[test]
fn rejected_activation_keeps_the_last_good_version() {
    let w = world();
    let good = w.publish(1, &art("Good")).expect("good");
    w.cf.fault(Op::CreateDeployment, Fault::Status(400));
    let e = w.publish(2, &art("Next")).expect_err("rejected");
    assert_eq!(
        (e.stage, e.activation),
        (Stage::Activate, Activation::NotActivated)
    );
    assert_eq!(w.cf.live_version(), Some(good.cf_version_id.clone()));
    assert_eq!(w.target().live_version_id, Some(good.cf_version_id));
    assert_eq!(w.target().activating_job, None);
    assert_eq!(
        w.cf.count(Op::CreateDeployment),
        2,
        "a definitive 4xx is never retried"
    );
    let a = w.store.s.borrow().audit.last().cloned().expect("audit");
    assert_eq!((a.result, a.stage), (AuditResult::Failed, Some("activate")));
}

#[test]
fn missing_token_artifact_pin_and_schema_mismatch_fail_in_prepare() {
    let mut w = world();
    w.keys = Rc::new(Keys(None));
    let e = w.publish(1, &art("x")).expect_err("no token");
    assert_eq!((e.stage, e.code), (Stage::Prepare, "token_missing"));

    let w = world();
    let mut j = job(1);
    j.artifact_sha256 = Some("0".repeat(64));
    let e = block_on(run_publish(&j, &art("x"), &w.env())).expect_err("pin");
    assert_eq!((e.stage, e.code), (Stage::Prepare, "artifact_mismatch"));

    let w = world();
    let mut a = art("x");
    a.schema = 5;
    let e = w.publish(1, &a).expect_err("schema");
    assert_eq!((e.stage, e.code), (Stage::Prepare, "schema_mismatch"));
    assert!(w.cf.ops().is_empty());

    // A matching pin passes.
    let w = world();
    let digest = block_on(manifest::Manifest::build(&art("x")))
        .expect("manifest")
        .digest();
    let mut j = job(1);
    j.artifact_sha256 = Some(digest);
    block_on(run_publish(&j, &art("x"), &w.env())).expect("pinned publish");
}

// --- retries -------------------------------------------------------------------------

#[test]
fn idempotent_steps_retry_429_5xx_and_transport_with_bounded_backoff() {
    let w = world();
    w.cf.fault(Op::UploadSession, Fault::RetryAfter(429, 3));
    w.cf.fault(Op::UploadSession, Fault::Status(503));
    w.cf.fault(Op::UploadAssets, Fault::TransportBefore);
    w.cf.fault(Op::CreateVersion, Fault::Status(502));
    w.publish(1, &art("Home")).expect("recovers");
    assert_eq!(w.cf.count(Op::UploadSession), 3);
    assert_eq!(w.cf.count(Op::CreateVersion), 2);
    let sleeps = w.rt.sleeps.borrow().clone();
    assert_eq!(sleeps[0], 3000, "Retry-After honoured");
    assert!(
        sleeps
            .iter()
            .all(|ms| *ms <= api::RetryPolicy::DEFAULT.max_delay_ms)
    );
}

#[test]
fn retries_stop_after_the_attempt_cap_and_report_a_retryable_failure() {
    let w = world();
    for _ in 0..10 {
        w.cf.fault(Op::UploadSession, Fault::Status(503));
    }
    let e = w.publish(1, &art("Home")).expect_err("gives up");
    assert_eq!(
        w.cf.count(Op::UploadSession),
        api::RetryPolicy::DEFAULT.max_attempts as usize
    );
    assert_eq!((e.stage, e.retryable), (Stage::UploadSession, true));
    assert_eq!(w.cf.live_version(), Some(w.cf.initial_version()));
}

// --- activation: never blind, ambiguous outcomes reconciled --------------------------

#[test]
fn activation_lost_before_effect_is_reconciled_then_retried_once() {
    let w = world();
    w.cf.fault(Op::CreateDeployment, Fault::TransportBefore);
    let out = w.publish(1, &art("Home")).expect("publish");
    assert_eq!(w.cf.count(Op::CreateDeployment), 2);
    no_two_activations_without_reconcile(&w.cf.ops());
    assert_eq!(w.cf.live_version(), Some(out.cf_version_id));
}

#[test]
fn ambiguous_activation_that_applied_is_recorded_without_a_second_request() {
    for fault in [Fault::TransportAfter, Fault::StatusAfter(502)] {
        let w = world();
        w.cf.fault(Op::CreateDeployment, fault.clone());
        let out = w.publish(1, &art("Home")).expect("reconciled as live");
        assert_eq!(
            w.cf.count(Op::CreateDeployment),
            1,
            "{fault:?}: no second activation"
        );
        assert_eq!(w.cf.live_version(), Some(out.cf_version_id.clone()));
        assert_eq!(w.target().live_version_id, Some(out.cf_version_id));
    }
}

#[test]
fn activation_retries_are_bounded_and_each_follows_a_reconcile() {
    let w = world();
    for _ in 0..10 {
        w.cf.fault(Op::CreateDeployment, Fault::Status(503));
    }
    let e = w.publish(1, &art("Home")).expect_err("gives up");
    assert_eq!(
        w.cf.count(Op::CreateDeployment),
        api::MAX_ACTIVATION_ATTEMPTS as usize
    );
    no_two_activations_without_reconcile(&w.cf.ops());
    assert_eq!(
        (e.stage, e.activation, e.retryable),
        (Stage::Activate, Activation::NotActivated, true)
    );
    assert_eq!(w.cf.live_version(), Some(w.cf.initial_version()));
    assert_eq!(
        w.target().activating_job,
        None,
        "proved not live: lease released"
    );
}

#[test]
fn unreconcilable_activation_is_unknown_keeps_the_lease_and_the_next_holder_reconciles() {
    let w = world();
    w.cf.fault(Op::CreateDeployment, Fault::TransportAfter);
    for _ in 0..10 {
        w.cf.fault(Op::ListDeployments, Fault::Status(503));
    }
    let e = w.publish(5, &art("A")).expect_err("unknown");
    assert_eq!(
        (e.stage, e.code, e.activation),
        (Stage::Activate, "activation_unknown", Activation::Unknown)
    );
    assert_eq!(
        w.cf.count(Op::CreateDeployment),
        1,
        "never retried while unknown"
    );
    let t = w.target();
    assert_eq!(
        t.activating_job.as_deref(),
        Some("job-5"),
        "lease kept until expiry"
    );
    let applied = w.cf.live_version().expect("live");
    w.cf.state.borrow_mut().reset_faults();

    // While the lease is live, another job is refused.
    let e = w.publish(6, &art("B")).expect_err("busy");
    assert_eq!(
        (e.stage, e.code, e.retryable),
        (Stage::Lock, "target_busy", true)
    );

    // After expiry an OLDER job reconciles first: job-5's version is live, so the
    // high-water mark rises to 5 and the older job is refused.
    w.rt.now.set(w.rt.now.get() + store::LEASE_SECS + 1);
    let e = w
        .publish(4, &art("old"))
        .expect_err("older than the adopted version");
    assert_eq!((e.stage, e.code), (Stage::Lock, "stale_revision"));
    let t = w.target();
    assert_eq!(
        (t.live_version_id.as_deref(), t.high_water_seq),
        (Some(applied.as_str()), 5)
    );
    assert_eq!(t.activating_job, None);
    assert!(
        w.store
            .s
            .borrow()
            .audit
            .iter()
            .any(|a| a.action == AuditAction::LeaseTakeover && a.result == AuditResult::Succeeded)
    );
    // A newer job proceeds.
    let out = w.publish(6, &art("B")).expect("newer proceeds");
    assert_eq!(w.cf.live_version(), Some(out.cf_version_id));
}

#[test]
fn expired_lease_whose_version_never_went_live_is_cleared() {
    let w = world();
    w.cf.external_deploy(&fake_api::uuid(900)); // exists, then superseded below
    w.cf.external_deploy(&w.cf.initial_version());
    {
        let mut s = w.store.s.borrow_mut();
        let t = &mut s.targets[0];
        t.activating_job = Some("ghost".into());
        t.activating_seq = Some(9);
        t.activating_version_id = Some(fake_api::uuid(900));
        t.lease_expires_at = Some("2026-11-26T00:00:00Z".into());
    }
    let out = w
        .publish(6, &art("Home"))
        .expect("ghost cleared, 6 proceeds");
    assert_eq!(
        w.target().high_water_seq,
        6,
        "the ghost's seq 9 was not adopted"
    );
    assert_eq!(w.cf.live_version(), Some(out.cf_version_id));
}

#[test]
fn superseded_after_activation_is_reported_as_verify_failure() {
    let w = world();
    let cf = w.cf.clone();
    w.cf.on(Op::ListDeployments, move || {
        cf.external_deploy(&fake_api::uuid(777))
    });
    let e = w.publish(1, &art("Home")).expect_err("superseded");
    assert_eq!(
        (e.stage, e.code, e.activation),
        (
            Stage::Verify,
            "activation_superseded",
            Activation::Activated
        )
    );
    assert_eq!(w.target().activating_job, None);
    assert!(
        w.store.s.borrow().published.is_empty(),
        "unverified versions are not retained"
    );
}

// --- ordering and serialization ------------------------------------------------------

#[test]
fn an_older_job_is_refused_before_any_api_call() {
    let w = world();
    let newer = w.publish(5, &art("v5")).expect("v5");
    let before = w.cf.ops().len();
    let e = w.publish(3, &art("v3")).expect_err("stale");
    assert_eq!(
        (e.stage, e.code, e.retryable),
        (Stage::Prepare, "stale_revision", false)
    );
    assert_eq!(w.cf.ops().len(), before, "no API call for a stale job");
    assert_eq!(w.cf.live_version(), Some(newer.cf_version_id));
    let e = w
        .publish(5, &art("v5 again"))
        .expect_err("same seq is not newer");
    assert_eq!(e.code, "stale_revision");
}

#[test]
fn an_older_job_cannot_activate_over_a_newer_one_that_finished_meanwhile() {
    let w = world();
    let inner = Rc::new(RefCell::new(None));
    {
        let (w2, inner) = (w.clone(), inner.clone());
        // While job 3 is creating its version, job 5 runs to completion.
        w.cf.on(Op::CreateVersion, move || {
            *inner.borrow_mut() = Some(w2.publish(5, &art("v5")));
        });
    }
    let e = w.publish(3, &art("v3")).expect_err("older loses");
    let v5 = inner
        .borrow_mut()
        .take()
        .expect("inner ran")
        .expect("v5 published");
    assert_eq!(
        (e.stage, e.code, e.activation),
        (Stage::Lock, "stale_revision", Activation::NotAttempted)
    );
    assert_eq!(w.cf.live_version(), Some(v5.cf_version_id.clone()));
    assert_eq!(w.target().live_version_id, Some(v5.cf_version_id));
    assert_eq!(w.target().high_water_seq, 5);
}

#[test]
fn concurrent_activation_on_the_same_target_is_refused_by_the_lease() {
    let w = world();
    let inner = Rc::new(RefCell::new(None));
    {
        let (w2, inner) = (w.clone(), inner.clone());
        // Job 6 arrives while job 5's activation request is in flight.
        w.cf.on(Op::CreateDeployment, move || {
            *inner.borrow_mut() = Some(w2.publish(6, &art("v6")));
        });
    }
    let v5 = w.publish(5, &art("v5")).expect("holder wins");
    let e = inner
        .borrow_mut()
        .take()
        .expect("inner ran")
        .expect_err("busy");
    assert_eq!(
        (e.stage, e.code, e.retryable, e.activation),
        (Stage::Lock, "target_busy", true, Activation::NotAttempted)
    );
    assert_eq!(w.cf.live_version(), Some(v5.cf_version_id));
    // Retried later, the newer job goes through.
    let v6 = w.publish(6, &art("v6")).expect("retry");
    assert_eq!(w.cf.live_version(), Some(v6.cf_version_id));
}

#[test]
fn a_lost_lease_stops_before_the_activation_request() {
    let w = world();
    w.store.s.borrow_mut().steal_on_renew = true;
    let e = w.publish(1, &art("x")).expect_err("lease lost");
    assert_eq!(
        (e.stage, e.code, e.activation),
        (Stage::Lock, "lease_lost", Activation::NotAttempted)
    );
    assert_eq!(w.cf.count(Op::CreateDeployment), 0);
}

#[test]
fn lease_sql_semantics_in_the_store() {
    let w = world();
    let t = w.target();
    let claim = |job: &str, seq| LeaseClaim {
        job: job.into(),
        seq,
        version_id: fake_api::uuid(1),
        until: "2026-11-27T00:02:00Z".into(),
        now: "2026-11-27T00:00:00Z".into(),
    };
    assert!(block_on(w.store.acquire(&t, &claim("a", Some(1)))).expect("ok"));
    assert!(
        !block_on(w.store.acquire(&t, &claim("b", Some(2)))).expect("ok"),
        "held"
    );
    assert!(
        !block_on(w.store.clear_stale(&t, "a", "2026-11-27T00:01:00Z", false)).expect("ok"),
        "not expired"
    );
    assert!(block_on(w.store.clear_stale(&t, "a", "2026-11-27T00:02:00Z", false)).expect("ok"));
    w.store.s.borrow_mut().targets[0].high_water_seq = 7;
    assert!(
        !block_on(w.store.acquire(&t, &claim("c", Some(7)))).expect("ok"),
        "seq order"
    );
    assert!(
        block_on(w.store.acquire(&t, &claim("rb", None))).expect("ok"),
        "rollback has no seq"
    );
}

// --- rollback ------------------------------------------------------------------------

#[test]
fn rollback_reactivates_a_retained_version_without_uploading_or_replaying() {
    let w = world();
    let v1 = w.publish(1, &art("v1")).expect("v1");
    let v2 = w.publish(2, &art("v2")).expect("v2");
    let before = w.cf.ops().len();
    let out = w.rollback(1, &v1.published_id).expect("rollback");
    let rb_ops = w.cf.ops()[before..].to_vec();
    assert!(
        rb_ops
            .iter()
            .all(|o| matches!(o, Op::CreateDeployment | Op::ListDeployments)),
        "rollback never uploads or creates versions: {rb_ops:?}"
    );
    assert_eq!(
        rb_ops
            .iter()
            .filter(|o| **o == Op::CreateDeployment)
            .count(),
        1
    );
    assert_eq!(out.cf_version_id, v1.cf_version_id);
    assert_eq!(
        out.previous_live_version_id.as_deref(),
        Some(v2.cf_version_id.as_str())
    );
    assert_eq!(w.cf.live_version(), Some(v1.cf_version_id.clone()));
    let t = w.target();
    assert_eq!(
        t.live_published_id.as_deref(),
        Some(v1.published_id.as_str())
    );
    assert_eq!(t.live_revision_id.as_deref(), Some("rev-1"));
    assert_eq!(t.high_water_seq, 2, "rollback keeps the high-water mark");
    let a = w.store.s.borrow().audit.last().cloned().expect("audit");
    assert_eq!(
        (a.action, a.result, a.actor_user_id),
        (AuditAction::Rollback, AuditResult::Succeeded, Some(1))
    );
    assert_eq!(a.published_id.as_deref(), Some(v1.published_id.as_str()));
    assert_eq!(
        w.store.s.borrow().published.len(),
        2,
        "no new retained row; nothing replayed"
    );

    // Old jobs stay refused; the way forward is a NEWER revision (forward fix).
    assert_eq!(
        w.publish(2, &art("v2")).expect_err("replay").code,
        "stale_revision"
    );
    let v3 = w.publish(3, &art("v3 fix")).expect("forward fix");
    assert_eq!(w.cf.live_version(), Some(v3.cf_version_id));
}

#[test]
fn rollback_across_a_site_schema_change_is_blocked_and_requires_a_forward_fix() {
    let w = world();
    let v1 = w.publish(1, &art("v1")).expect("v1 at level 4");
    w.store.s.borrow_mut().targets[0].site_schema_level = 5;
    let mut a = art("v2");
    a.schema = 5;
    let v2 = w.publish(2, &a).expect("v2 at level 5");
    let before = w.cf.ops().len();
    let e = w.rollback(1, &v1.published_id).expect_err("unsafe");
    assert_eq!((e.status, e.code), (409, "rollback_schema_incompatible"));
    assert!(e.message.contains("forward fix"), "{}", e.message);
    assert_eq!(w.cf.ops().len(), before, "no API call");
    assert_eq!(w.cf.live_version(), Some(v2.cf_version_id));
    let a = w.store.s.borrow().audit.last().cloned().expect("audit");
    assert_eq!(
        (a.action, a.result, a.error_code),
        (
            AuditAction::Rollback,
            AuditResult::Refused,
            Some("rollback_schema_incompatible")
        )
    );
}

#[test]
fn rollback_needs_owner_or_publisher_and_a_retained_version() {
    let w = world();
    let v1 = w.publish(1, &art("v1")).expect("v1");
    w.publish(2, &art("v2")).expect("v2");
    let e = w.rollback(3, &v1.published_id).expect_err("editor");
    assert_eq!((e.status, e.code), (403, "forbidden"));
    let audits = w.store.s.borrow().audit.len();
    let e = w.rollback(99, &v1.published_id).expect_err("non-member");
    assert_eq!((e.status, e.code), (403, "forbidden"));
    assert_eq!(
        w.store.s.borrow().audit.len(),
        audits,
        "no audit write into a tenant for a non-member"
    );
    let e = w.rollback(1, "pv-unknown").expect_err("unknown");
    assert_eq!((e.status, e.code), (404, "deployment_not_found"));
    w.rollback(2, &v1.published_id)
        .expect("publisher may roll back");
    let e = w.rollback(1, &v1.published_id).expect_err("already live");
    assert_eq!((e.status, e.code), (409, "already_live"));
}

#[test]
fn rollback_rejected_by_cloudflare_keeps_live_and_points_at_a_forward_fix() {
    let w = world();
    let v1 = w.publish(1, &art("v1")).expect("v1");
    let v2 = w.publish(2, &art("v2")).expect("v2");
    w.cf.fault(Op::CreateDeployment, Fault::Status(400));
    let e = w.rollback(1, &v1.published_id).expect_err("rejected");
    assert_eq!(
        (e.status, e.code, e.activation),
        (409, "rollback_rejected", Activation::NotActivated)
    );
    assert!(e.message.contains("forward fix"));
    assert_eq!(w.cf.live_version(), Some(v2.cf_version_id.clone()));
    assert_eq!(w.target().live_version_id, Some(v2.cf_version_id));
    assert_eq!(w.target().activating_job, None);
    let ok =
        w.cf.state
            .borrow()
            .log
            .iter()
            .all(|l| !l.url.contains("force"));
    assert!(ok, "never forces a blocked rollback");
}

#[test]
fn rollback_is_serialized_with_publishes() {
    let w = world();
    let v1 = w.publish(1, &art("v1")).expect("v1");
    w.publish(2, &art("v2")).expect("v2");
    let inner = Rc::new(RefCell::new(None));
    {
        let (w2, inner, id) = (w.clone(), inner.clone(), v1.published_id.clone());
        w.cf.on(Op::CreateDeployment, move || {
            *inner.borrow_mut() = Some(w2.rollback(1, &id));
        });
    }
    w.publish(3, &art("v3")).expect("publish holds the lease");
    let e = inner.borrow_mut().take().expect("ran").expect_err("busy");
    assert_eq!((e.status, e.code), (409, "target_busy"));
}

// --- secrets -------------------------------------------------------------------------

#[test]
fn the_token_and_upload_jwts_never_appear_in_errors_logs_or_audit() {
    let scenarios: Vec<(Op, Fault)> = vec![
        (Op::UploadSession, Fault::EchoAuth(403)),
        (Op::UploadAssets, Fault::EchoAuth(401)),
        (Op::CreateVersion, Fault::EchoAuth(403)),
        (Op::CreateDeployment, Fault::EchoAuth(403)),
        (Op::CreateDeployment, Fault::TransportAfter),
        (Op::UploadSession, Fault::TransportAfter),
        (Op::UploadAssets, Fault::TransportAfter),
    ];
    for (op, fault) in scenarios {
        let w = world();
        for _ in 0..6 {
            w.cf.fault(op, fault.clone());
        }
        if matches!(fault, Fault::TransportAfter) && op == Op::CreateDeployment {
            for _ in 0..10 {
                w.cf.fault(Op::ListDeployments, Fault::TransportAfter);
            }
        }
        let result = w.publish(1, &art("Home"));
        let mut haystack = format!("{result:?}");
        if let Err(e) = &result {
            haystack.push_str(&e.to_string());
        }
        haystack.push_str(&w.rt.logs.borrow().join("\n"));
        haystack.push_str(&format!("{:?}", w.store.s.borrow().audit));
        for secret in w.secrets_seen() {
            assert!(
                !haystack.contains(&secret),
                "{op:?}/{fault:?} leaked {secret}: {haystack}"
            );
        }
    }
    assert_eq!(
        format!("{:?}", ApiToken::new(TOKEN.into())),
        "ApiToken(<redacted>)"
    );
}
