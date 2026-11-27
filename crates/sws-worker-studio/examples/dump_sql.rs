//! Prints every repository statement as JSON (`[{name, sql, tenant}]`) so the
//! Node SQL-level test (`tests/unit/studio-registry.test.mjs`) runs the exact
//! SQL the Worker binds, against the real migration, with no extra dependency.

use serde_json::json;

fn main() {
    let rows: Vec<_> = sws_worker_studio::repo::ALL
        .iter()
        .map(|(name, sql, tenant)| json!({ "name": name, "sql": sql, "tenant": tenant }))
        .collect();
    println!("{}", serde_json::Value::Array(rows));
}
