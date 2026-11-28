//! Version metadata for the site Worker (host-testable).
//!
//! Every Version Upload must restate the Worker's bindings. The target stores its
//! NON-SECRET bindings (`publish_targets.bindings_json`), validated here against a
//! strict allowlist; secrets are kept on the Worker with `keep_bindings`
//! (`secret_text`, `secret_key`), so Solo never stores or sends them.
//!
//! The allowlist is also the standalone guarantee ("published requests must work when
//! Studio is unavailable"): only D1, plain-text vars and rate limiters are accepted, so a
//! published version can carry no service, Durable Object, dispatch, queue or other
//! cross-Worker binding, and therefore no path back to Studio.

use serde_json::{Map, Value, json};

use super::PublishJobRef;
use super::manifest::Manifest;
use super::store::Target;

pub const ASSETS_BINDING: &str = "ASSETS";
/// Only `/api/*` reaches the Worker first; everything else is a static asset
/// (matches `wrangler.jsonc`).
pub const RUN_WORKER_FIRST: &[&str] = &["/api/*"];
pub const NOT_FOUND_HANDLING: &str = "404-page";
pub const HTML_HANDLING: &str = "auto-trailing-slash";
/// Binding types kept from the latest uploaded version (secrets set with
/// `wrangler secret put`, e.g. `TURNSTILE_SECRET_KEY`).
pub const KEEP_BINDINGS: &[&str] = &["secret_text", "secret_key"];
pub const ALLOWED_TYPES: &[&str] = &["d1", "plain_text", "ratelimit"];
/// The site Worker needs its D1 database and the public Turnstile key (#66: "preserve
/// existing D1/Turnstile bindings"); the Turnstile secret is preserved by `keep_bindings`.
pub const REQUIRED: &[(&str, &str)] = &[("d1", "DB"), ("plain_text", "TURNSTILE_SITE_KEY")];

pub const MAX_BINDINGS: usize = 64;
pub const MAX_BINDINGS_JSON_BYTES: usize = 128 * 1024;
pub const MAX_TEXT_BYTES: usize = 32 * 1024;

fn valid_name(n: &str) -> bool {
    let mut b = n.bytes();
    (1..=64).contains(&n.len())
        && b.next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn valid_resource_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
}

fn only_keys(o: &Map<String, Value>, keys: &[&str]) -> bool {
    o.keys().all(|k| keys.contains(&k.as_str()))
}

fn str_of<'a>(o: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    o.get(k).and_then(Value::as_str)
}

/// Validate and normalize the stored bindings. Errors name the problem but never echo
/// binding values.
pub fn validate(json_text: &str) -> Result<Vec<Value>, String> {
    if json_text.len() > MAX_BINDINGS_JSON_BYTES {
        return Err("bindings too large".into());
    }
    let v: Value =
        serde_json::from_str(json_text).map_err(|_| "bindings are not JSON".to_string())?;
    let arr = v.as_array().ok_or("bindings must be a JSON array")?;
    if arr.len() > MAX_BINDINGS {
        return Err("too many bindings".into());
    }
    let mut out = Vec::with_capacity(arr.len());
    let mut names: Vec<String> = Vec::new();
    for b in arr {
        let o = b.as_object().ok_or("binding must be an object")?;
        let ty = str_of(o, "type").ok_or("binding type missing")?;
        let name = str_of(o, "name").ok_or("binding name missing")?;
        if !valid_name(name) || name == ASSETS_BINDING {
            return Err(format!("invalid or reserved binding name for type {ty}"));
        }
        if names.iter().any(|n| n == name) {
            return Err(format!("duplicate binding name {name}"));
        }
        if !ALLOWED_TYPES.contains(&ty) {
            return Err(format!(
                "binding type {ty} is not allowed on a published site"
            ));
        }
        let norm = match ty {
            "d1" => {
                let id = str_of(o, "database_id").or_else(|| str_of(o, "id"));
                if !only_keys(o, &["type", "name", "id", "database_id"])
                    || !id.is_some_and(valid_resource_id)
                {
                    return Err(format!("d1 binding {name} is invalid"));
                }
                json!({ "type": "d1", "name": name, "database_id": id })
            }
            "plain_text" => {
                let text = str_of(o, "text");
                if !only_keys(o, &["type", "name", "text"])
                    || !text.is_some_and(|t| t.len() <= MAX_TEXT_BYTES)
                {
                    return Err(format!("plain_text binding {name} is invalid"));
                }
                json!({ "type": "plain_text", "name": name, "text": text })
            }
            _ => {
                let ns = str_of(o, "namespace_id");
                let simple = o.get("simple").and_then(Value::as_object);
                let limit = simple.and_then(|s| s.get("limit")).and_then(Value::as_u64);
                let period = simple.and_then(|s| s.get("period")).and_then(Value::as_u64);
                let simple_ok = simple
                    .is_some_and(|s| only_keys(s, &["limit", "period", "mitigation_timeout"]));
                if !only_keys(o, &["type", "name", "namespace_id", "simple"])
                    || !ns.is_some_and(|n| {
                        (1..=32).contains(&n.len()) && n.bytes().all(|c| c.is_ascii_digit())
                    })
                    || !simple_ok
                    || !limit.is_some_and(|l| l > 0)
                    || !period.is_some_and(|p| p > 0)
                {
                    return Err(format!("ratelimit binding {name} is invalid"));
                }
                b.clone()
            }
        };
        names.push(name.to_string());
        out.push(norm);
    }
    for (ty, name) in REQUIRED {
        if !out.iter().any(|b| b["type"] == *ty && b["name"] == *name) {
            return Err(format!("required {ty} binding {name} missing"));
        }
    }
    Ok(out)
}

/// `metadata` part of the Version Upload request.
pub fn version_metadata(
    target: &Target,
    bindings: &[Value],
    main_module: &str,
    completion_jwt: &str,
    manifest: &Manifest,
    job: &PublishJobRef,
) -> Value {
    let mut all = vec![json!({ "type": "assets", "name": ASSETS_BINDING })];
    all.extend(bindings.iter().cloned());
    let mut config = json!({
        "run_worker_first": RUN_WORKER_FIRST,
        "not_found_handling": NOT_FOUND_HANDLING,
        "html_handling": HTML_HANDLING,
    });
    if let Some(h) = &manifest.headers {
        config["_headers"] = json!(h);
    }
    if let Some(r) = &manifest.redirects {
        config["_redirects"] = json!(r);
    }
    let message: String = format!(
        "Solo publish {} of revision {} (#{})",
        job.job_id, job.revision_id, job.revision_seq
    )
    .chars()
    .take(1000)
    .collect();
    json!({
        "main_module": main_module,
        "compatibility_date": target.compatibility_date,
        "bindings": all,
        "keep_bindings": KEEP_BINDINGS,
        "assets": { "jwt": completion_jwt, "config": config },
        "annotations": {
            "workers/message": message,
            "workers/tag": job.revision_id.chars().take(100).collect::<String>(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"[{"type":"d1","name":"DB","id":"00000000-0000-0000-0000-000000000001"},
        {"type":"plain_text","name":"TURNSTILE_SITE_KEY","text":"k"}]"#;

    #[test]
    fn normalizes_d1_to_database_id() {
        let v = validate(OK).expect("valid");
        assert_eq!(v[0]["database_id"], "00000000-0000-0000-0000-000000000001");
        assert!(v[0].get("id").is_none());
    }

    #[test]
    fn rejects_unknown_keys_bad_names_and_duplicates() {
        for bad in [
            r#"[{"type":"d1","name":"DB","id":"00000000-0000-0000-0000-000000000001","x":1},{"type":"plain_text","name":"TURNSTILE_SITE_KEY","text":"k"}]"#,
            r#"[{"type":"d1","name":"1DB","id":"aa"},{"type":"plain_text","name":"TURNSTILE_SITE_KEY","text":"k"}]"#,
            r#"[{"type":"d1","name":"DB","id":"aa"},{"type":"plain_text","name":"DB","text":"k"}]"#,
            r#"[{"type":"d1","name":"DB","id":"not hex!"},{"type":"plain_text","name":"TURNSTILE_SITE_KEY","text":"k"}]"#,
            r#"[{"type":"kv_namespace","name":"KV","namespace_id":"a"}]"#,
            r#"[{"type":"secret_key","name":"K"}]"#,
            r#"[{"type":"inherit","name":"DB"}]"#,
        ] {
            assert!(validate(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn errors_never_echo_values() {
        let e = validate(r#"[{"type":"plain_text","name":"X","text":"s3cr3t","extra":1}]"#)
            .expect_err("bad");
        assert!(!e.contains("s3cr3t"), "{e}");
    }
}
