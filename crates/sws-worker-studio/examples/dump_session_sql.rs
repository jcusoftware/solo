//! Prints the Durable Object SQLite schema, its migrations and every named session statement as
//! JSON (`{ schema: [...], migrations: [...], statements: [{name, sql}] }`) so the Node SQL test
//! (`tests/unit/studio-session-sql.test.mjs`) runs the exact SQL the object
//! executes, against a real SQLite, with no extra dependency.

use serde_json::json;

fn main() {
    let statements: Vec<_> = sws_worker_studio::session::sql::ALL
        .iter()
        .map(|(name, sql)| json!({ "name": name, "sql": sql }))
        .collect();
    println!(
        "{}",
        json!({
            "schema": sws_worker_studio::session::sql::SCHEMA,
            "migrations": sws_worker_studio::session::sql::MIGRATIONS,
            "statements": statements,
        })
    );
}
