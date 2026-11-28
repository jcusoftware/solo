//! Prints every repository statement as JSON (`[{name, sql, tenant}]`) so the
//! Node SQL-level tests run the exact SQL the Worker binds, against the real
//! migrations, with no extra dependency. Default: the registry statements
//! (`tests/unit/studio-registry.test.mjs`); `--assets`: the asset-store statements
//! (`tests/unit/studio-assets.test.mjs`); `--publish`: the publish-job statements
//! (`tests/unit/studio-publish-sql.test.mjs`); `--deploy`: the publish-target, lease,
//! retained-version, bundle and audit statements (`tests/unit/studio-deploy-sql.test.mjs`);
//! `--auth`: the authentication statements (`tests/unit/studio-auth.test.mjs`,
//! `tenant` is always false there: those statements are scoped by credential id and user instead).

use serde_json::json;

fn main() {
    let flag = |f: &str| std::env::args().any(|a| a == f);
    if flag("--auth") {
        let rows: Vec<_> = sws_worker_studio::auth_repo::ALL
            .iter()
            .map(|(name, sql)| json!({ "name": name, "sql": sql, "tenant": false }))
            .collect();
        println!("{}", serde_json::Value::Array(rows));
        return;
    }
    let all = if flag("--assets") {
        sws_worker_studio::asset_repo::ALL
    } else if flag("--publish") {
        sws_worker_studio::publish::sql::ALL
    } else if flag("--deploy") {
        sws_worker_studio::deploy::sql::ALL
    } else {
        sws_worker_studio::repo::ALL
    };
    let rows: Vec<_> = all
        .iter()
        .map(|(name, sql, tenant)| json!({ "name": name, "sql": sql, "tenant": tenant }))
        .collect();
    println!("{}", serde_json::Value::Array(rows));
}
