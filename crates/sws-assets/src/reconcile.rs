//! Reconciliation and retention decisions (pure; the backends execute them).

use std::collections::HashSet;

use crate::limits::GeneratedPolicy;
use crate::validate::is_sha256;

/// Objects younger than this are never swept: an upload that put its object but has
/// not committed the metadata yet is still in flight. Equal to the reservation TTL,
/// after which the uploading request can no longer commit.
pub const ORPHAN_MIN_AGE_SECS: u64 = crate::limits::RESERVATION_TTL_SECS;

/// Should the R2 object `key` (listed under `prefix` = `ws/site/`) be deleted?
/// True for an object no asset row references (or whose name is not a SHA-256) once
/// it is older than [`ORPHAN_MIN_AGE_SECS`]. Keys outside the prefix are never touched.
pub fn object_is_orphan(
    prefix: &str,
    key: &str,
    referenced: bool,
    uploaded: u64,
    now: u64,
) -> bool {
    let Some(name) = key.strip_prefix(prefix) else {
        return false;
    };
    let unreferenced = !is_sha256(name) || !referenced;
    unreferenced && now.saturating_sub(uploaded) >= ORPHAN_MIN_AGE_SECS
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub id: String,
    /// Creation time, unix seconds.
    pub created_at: u64,
}

/// Which artifacts to delete: expired ones, then everything beyond `keep_latest`
/// (newest first, ties broken by id descending). Mirrors the SQL retention statements.
pub fn retention_plan(artifacts: &[Artifact], policy: &GeneratedPolicy, now: u64) -> Vec<String> {
    let mut live: Vec<&Artifact> = artifacts
        .iter()
        .filter(|a| now < a.created_at.saturating_add(policy.retention_secs))
        .collect();
    live.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    let keep: HashSet<&str> = live
        .iter()
        .take(policy.keep_latest as usize)
        .map(|a| a.id.as_str())
        .collect();
    artifacts
        .iter()
        .filter(|a| !keep.contains(a.id.as_str()))
        .map(|a| a.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::PREVIEW;

    #[test]
    fn orphans_are_old_unreferenced_keys_inside_the_prefix() {
        let sha = "a".repeat(64);
        let key = format!("ws/s/{sha}");
        let now = 100_000;
        let old = now - ORPHAN_MIN_AGE_SECS;
        assert!(object_is_orphan("ws/s/", &key, false, old, now));
        assert!(
            !object_is_orphan("ws/s/", &key, true, old, now),
            "referenced"
        );
        assert!(
            !object_is_orphan("ws/s/", &key, false, old + 1, now),
            "possibly in flight"
        );
        assert!(
            object_is_orphan("ws/s/", "ws/s/not-a-hash", true, old, now),
            "malformed"
        );
        assert!(
            !object_is_orphan("ws/s/", &format!("ws/other/{sha}"), false, 0, now),
            "other prefix"
        );
    }

    #[test]
    fn retention_drops_expired_then_excess() {
        let now = 10_000_000;
        let fresh = |i: &str, age: u64| Artifact {
            id: i.into(),
            created_at: now - age,
        };
        let arts = vec![
            fresh("old", PREVIEW.retention_secs + 1),
            fresh("p1", 10),
            fresh("p2", 20),
            fresh("p3", 30),
            fresh("p4", 40),
        ];
        let mut del = retention_plan(&arts, &PREVIEW, now);
        del.sort();
        assert_eq!(del, vec!["old".to_string(), "p4".to_string()]);
        assert!(retention_plan(&[], &PREVIEW, now).is_empty());
    }
}
