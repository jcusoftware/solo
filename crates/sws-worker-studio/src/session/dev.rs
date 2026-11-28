//! Test-only routes (`insecure-dev-auth` build, wasm only). They let the local
//! integration tests call the Worker -> Durable Object internals that #65 and #64
//! will call, which no public route exposes. Compiled out of every other build,
//! and even here open only behind [`crate::session::devfence`].
//!
//! * `POST /api/dev/revoke/{workspace}/{site}/{user_id}`: `entry::revoke_user`
//! * `GET  /api/dev/revision/{workspace}/{site}/{revision_id}`: `entry::fetch_revision`
//! * `POST /api/dev/tx-probe/{workspace}/{site}`: DO SQL `transactionSync` probe
//! * `POST /api/dev/d1-fault/{workspace}/{site}/{down|checkpoint|up}`: simulate a D1 outage
//!   (`down`: membership reads and checkpoint writes; `checkpoint`: checkpoint writes only;
//!   `slow`: every membership read takes 400 ms)
//! * `POST /api/dev/publish-drive[?now=<offset s>&crash_after=<phase>&transient=<phase>&job=<ws>/<site>/<id>]`:
//!   run the publish driver (`publish::drive`, the function the Cron Trigger calls)
//!   with a skewed clock and injected faults; without `job` it is one scheduled run

use serde_json::json;
use worker::{Env, Method, Request, RequestInit, Response, Result};

use crate::respond::{json_error, json_ok};
use crate::session::entry;
use crate::session::http::to_response;
use crate::session::{PATH_D1_FAULT, PATH_TX_PROBE, is_valid_id, object_name};

/// `Ok(None)` = not a dev route (or the fence is closed): fall through.
pub async fn handle(
    req: &Request,
    env: &Env,
    method: &str,
    path: &str,
) -> Result<Option<Response>> {
    let Some(rest) = path.strip_prefix("/api/dev/") else {
        return Ok(None);
    };
    if !entry::dev_fence_open(req, env) {
        return Ok(None);
    }
    let parts: Vec<&str> = rest.split('/').collect();
    let resp = match (method, parts.as_slice()) {
        ("POST", ["revoke", w, s, u]) if is_valid_id(w) && is_valid_id(s) => {
            let Ok(user) = u.parse::<i64>() else {
                return Ok(Some(to_response(json_error(400, "bad_request", &[]))?));
            };
            entry::revoke_user(env, w, s, user, None).await?;
            json_ok(json!({ "revoked": true }))
        }
        ("GET", ["revision", w, s, r]) if is_valid_id(w) && is_valid_id(s) && is_valid_id(r) => {
            match entry::fetch_revision(env, w, s, r).await? {
                Some(v) => json_ok(v),
                None => json_error(404, "not_found", &[]),
            }
        }
        ("POST", ["tx-probe", w, s]) if is_valid_id(w) && is_valid_id(s) => {
            let mut init = RequestInit::new();
            init.with_method(Method::Post);
            let probe =
                Request::new_with_init(&format!("https://session.internal{PATH_TX_PROBE}"), &init)?;
            let mut r = env
                .durable_object(entry::BINDING)?
                .id_from_name(&object_name(w, s))?
                .get_stub()?
                .fetch_with_request(probe)
                .await?;
            json_ok(r.json().await?)
        }
        (
            "POST",
            [
                "d1-fault",
                w,
                s,
                mode @ ("down" | "up" | "checkpoint" | "slow"),
            ],
        ) if is_valid_id(w) && is_valid_id(s) => {
            let mut init = RequestInit::new();
            init.with_method(Method::Post);
            let req = Request::new_with_init(
                &format!("https://session.internal{PATH_D1_FAULT}?mode={mode}"),
                &init,
            )?;
            let mut r = env
                .durable_object(entry::BINDING)?
                .id_from_name(&object_name(w, s))?
                .get_stub()?
                .fetch_with_request(req)
                .await?;
            json_ok(r.json().await?)
        }
        ("POST", ["publish-drive"]) => publish_drive(req, env).await?,
        _ => json_error(404, "not_found", &[]),
    };
    Ok(Some(to_response(resp)?))
}

/// The publish driver under test control. Every knob is test-only: a clock offset
/// (`now`, seconds added to the real clock), a crash right after a phase's transition
/// is written (`crash_after`), and a transient failure in a phase (`transient`).
async fn publish_drive(req: &Request, env: &Env) -> Result<crate::respond::Resp> {
    use crate::publish::drive::{self, DriveOptions};
    use crate::publish::job::Phase;
    let url = req.url()?;
    let q = |k: &str| {
        url.query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
    };
    let mut opts = DriveOptions::from_env(env);
    if let Some(off) = q("now").and_then(|v| v.parse::<i64>().ok()) {
        opts.now = opts.now.saturating_add_signed(off);
    }
    opts.faults.crash_after = q("crash_after").as_deref().and_then(Phase::parse);
    opts.faults.transient_in = q("transient").as_deref().and_then(Phase::parse);
    if let Some(target) = q("job") {
        let parts: Vec<&str> = target.split('/').collect();
        let [w, s, j] = parts.as_slice() else {
            return Ok(json_error(400, "bad_request", &[]));
        };
        if !(is_valid_id(w) && is_valid_id(s) && is_valid_id(j)) {
            return Ok(json_error(400, "bad_request", &[]));
        }
        let db = crate::d1::database(env)?;
        let out = drive::drive_one(&db, w, s, j, None, &opts).await?;
        return Ok(json_ok(json!({ "jobs": [out.to_json()] })));
    }
    Ok(json_ok(drive::run_due(env, &opts).await?))
}
