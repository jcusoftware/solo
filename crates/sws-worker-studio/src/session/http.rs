//! `Resp` -> `worker::Response` for the session glue (wasm only). Mirrors the
//! adapter in `lib.rs`; the headers themselves are decided by the pure layer.

use worker::{Response, Result};

use crate::respond::Resp;

pub fn to_response(r: Resp) -> Result<Response> {
    let resp = match &r.body {
        Some(body) => Response::from_bytes(serde_json::to_vec(body)?)?,
        None => Response::empty()?,
    }
    .with_status(r.status);
    for (k, v) in &r.headers {
        resp.headers().set(k, v)?;
    }
    Ok(resp)
}
