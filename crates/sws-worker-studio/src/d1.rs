//! wasm-only glue that runs [`crate::repo::Statement`]s against the `STUDIO_DB`
//! D1 binding with bound parameters.

use worker::wasm_bindgen::JsValue;
use worker::{D1Database, Env, Result};

use crate::repo::{Param, Statement};

pub const BINDING: &str = "STUDIO_DB";

pub fn database(env: &Env) -> Result<D1Database> {
    env.d1(BINDING)
}

fn to_js(p: &Param) -> JsValue {
    match p {
        Param::Int(i) => JsValue::from_f64(*i as f64),
        Param::Text(s) => JsValue::from_str(s),
        Param::Blob(b) => worker::js_sys::Uint8Array::from(b.as_slice()).into(),
        Param::Null => JsValue::NULL,
    }
}

/// First row as raw JSON, or `None` (also the answer for another workspace's rows).
pub async fn first(db: &D1Database, stmt: &Statement) -> Result<Option<serde_json::Value>> {
    let params: Vec<JsValue> = stmt.params.iter().map(to_js).collect();
    db.prepare(stmt.sql)
        .bind(&params)?
        .first::<serde_json::Value>(None)
        .await
}

/// Execute a write; returns the number of rows changed.
pub async fn run(db: &D1Database, stmt: &Statement) -> Result<usize> {
    let params: Vec<JsValue> = stmt.params.iter().map(to_js).collect();
    let result = db.prepare(stmt.sql).bind(&params)?.run().await?;
    Ok(result.meta()?.and_then(|m| m.changes).unwrap_or(0))
}

/// Execute statements as one atomic D1 batch (a single transaction); returns the
/// rows changed by each statement, in order. If any statement errors the whole batch
/// is rolled back and the error is returned.
pub async fn batch(db: &D1Database, stmts: &[Statement]) -> Result<Vec<usize>> {
    let mut prepared = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        let params: Vec<JsValue> = stmt.params.iter().map(to_js).collect();
        prepared.push(db.prepare(stmt.sql).bind(&params)?);
    }
    let results = db.batch(prepared).await?;
    results
        .iter()
        .map(|r| Ok(r.meta()?.and_then(|m| m.changes).unwrap_or(0)))
        .collect()
}

/// Typed first row (used to read BLOB chunks straight into `Vec<u8>` without a
/// `serde_json::Value` detour).
pub async fn first_as<T: serde::de::DeserializeOwned>(
    db: &D1Database,
    stmt: &Statement,
) -> Result<Option<T>> {
    let params: Vec<JsValue> = stmt.params.iter().map(to_js).collect();
    db.prepare(stmt.sql).bind(&params)?.first::<T>(None).await
}

/// All rows (callers bound the result with a `LIMIT` in the statement).
pub async fn all(db: &D1Database, stmt: &Statement) -> Result<Vec<serde_json::Value>> {
    let params: Vec<JsValue> = stmt.params.iter().map(to_js).collect();
    db.prepare(stmt.sql)
        .bind(&params)?
        .all()
        .await?
        .results::<serde_json::Value>()
}
