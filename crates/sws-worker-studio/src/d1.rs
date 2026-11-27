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
