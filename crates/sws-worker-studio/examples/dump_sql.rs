//! Prints every repository statement as JSON (`[{name, sql, tenant}]`) so the
//! Node SQL-level tests run the exact SQL the Worker binds, against the real
//! migrations, with no extra dependency. Default: the registry statements
//! (`tests/unit/studio-registry.test.mjs`); `--assets`: the asset-store statements
//! (`tests/unit/studio-assets.test.mjs`); `--publish`: the publish-job statements
//! (`tests/unit/studio-publish-sql.test.mjs`).

use serde_json::json;

fn main() {
    let flag = |f: &str| std::env::args().any(|a| a == f);
    let all = if flag("--assets") {
        sws_worker_studio::asset_repo::ALL
    } else if flag("--publish") {
        sws_worker_studio::publish::sql::ALL
    } else {
        sws_worker_studio::repo::ALL
    };
    let rows: Vec<_> = all
        .iter()
        .map(|(name, sql, tenant)| json!({ "name": name, "sql": sql, "tenant": tenant }))
        .collect();
    println!("{}", serde_json::Value::Array(rows));
}
