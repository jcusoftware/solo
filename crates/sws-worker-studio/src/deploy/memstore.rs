//! Test-only in-memory [`PublishStore`] mirroring the conditional semantics of the
//! D1 statements in [`super::sql`] (those statements are exercised against real SQLite
//! in `tests/unit/studio-publish-sql.test.mjs`).

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::model::Role;

use super::store::{AuditRow, LeaseClaim, PublishStore, PublishedVersion, StoreError, Target};

#[derive(Default)]
pub struct MemState {
    pub targets: Vec<Target>,
    pub roles: BTreeMap<(String, i64), Role>,
    pub published: Vec<PublishedVersion>,
    pub audit: Vec<AuditRow>,
    /// Make the next renew report a lost lease.
    pub steal_on_renew: bool,
}

#[derive(Default)]
pub struct MemStore {
    pub s: RefCell<MemState>,
}

impl MemStore {
    fn with_target<R>(&self, t: &Target, f: impl FnOnce(&mut Target) -> R) -> Option<R> {
        let mut s = self.s.borrow_mut();
        s.targets
            .iter_mut()
            .find(|x| x.workspace_id == t.workspace_id && x.id == t.id)
            .map(f)
    }

    pub fn target_now(&self) -> Target {
        self.s.borrow().targets[0].clone()
    }
}

fn clear_lease(x: &mut Target) {
    x.activating_job = None;
    x.activating_seq = None;
    x.activating_version_id = None;
    x.lease_expires_at = None;
}

impl PublishStore for MemStore {
    async fn target(&self, ws: &str, site: &str) -> Result<Option<Target>, StoreError> {
        Ok(self
            .s
            .borrow()
            .targets
            .iter()
            .find(|t| t.workspace_id == ws && t.site_id == site)
            .cloned())
    }

    async fn role(&self, ws: &str, user_id: i64) -> Result<Option<Role>, StoreError> {
        Ok(self
            .s
            .borrow()
            .roles
            .get(&(ws.to_string(), user_id))
            .copied())
    }

    async fn acquire(&self, t: &Target, c: &LeaseClaim) -> Result<bool, StoreError> {
        Ok(self
            .with_target(t, |x| {
                let free = x.activating_job.is_none();
                let ordered = c.seq.is_none_or(|s| x.high_water_seq < s);
                if free && ordered {
                    x.activating_job = Some(c.job.clone());
                    x.activating_seq = c.seq;
                    x.activating_version_id = Some(c.version_id.clone());
                    x.lease_expires_at = Some(c.until.clone());
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false))
    }

    async fn renew(
        &self,
        t: &Target,
        job: &str,
        until: &str,
        now: &str,
    ) -> Result<bool, StoreError> {
        if std::mem::take(&mut self.s.borrow_mut().steal_on_renew) {
            self.with_target(t, |x| x.activating_job = Some("thief".into()));
            return Ok(false);
        }
        Ok(self
            .with_target(t, |x| {
                let held = x.activating_job.as_deref() == Some(job)
                    && x.lease_expires_at.as_deref().is_some_and(|e| e > now);
                if held {
                    x.lease_expires_at = Some(until.to_string());
                }
                held
            })
            .unwrap_or(false))
    }

    async fn release(&self, t: &Target, job: &str) -> Result<(), StoreError> {
        self.with_target(t, |x| {
            if x.activating_job.as_deref() == Some(job) {
                clear_lease(x);
            }
        });
        Ok(())
    }

    async fn clear_stale(
        &self,
        t: &Target,
        stale_job: &str,
        now: &str,
        adopt: bool,
    ) -> Result<bool, StoreError> {
        Ok(self
            .with_target(t, |x| {
                let stale = x.activating_job.as_deref() == Some(stale_job)
                    && x.lease_expires_at.as_deref().is_some_and(|e| e <= now);
                if !stale {
                    return false;
                }
                if adopt {
                    x.live_version_id = x.activating_version_id.clone();
                    x.live_published_id = None;
                    x.live_revision_id = None;
                    x.high_water_seq = x.high_water_seq.max(x.activating_seq.unwrap_or(0));
                }
                clear_lease(x);
                true
            })
            .unwrap_or(false))
    }

    async fn record_publish(
        &self,
        t: &Target,
        job: &str,
        v: &PublishedVersion,
        audit: &AuditRow,
    ) -> Result<bool, StoreError> {
        let applied = self
            .with_target(t, |x| {
                let ok =
                    x.activating_job.as_deref() == Some(job) && x.high_water_seq < v.revision_seq;
                if ok {
                    x.live_version_id = Some(v.cf_version_id.clone());
                    x.live_published_id = Some(v.id.clone());
                    x.live_revision_id = Some(v.revision_id.clone());
                    x.high_water_seq = v.revision_seq;
                    clear_lease(x);
                }
                ok
            })
            .unwrap_or(false);
        let mut s = self.s.borrow_mut();
        if applied && !s.published.iter().any(|p| p.id == v.id) {
            s.published.push(v.clone());
        }
        s.audit.push(audit.clone());
        Ok(applied)
    }

    async fn record_rollback(
        &self,
        t: &Target,
        job: &str,
        v: &PublishedVersion,
        audit: &AuditRow,
    ) -> Result<bool, StoreError> {
        let applied = self
            .with_target(t, |x| {
                let ok = x.activating_job.as_deref() == Some(job);
                if ok {
                    x.live_version_id = Some(v.cf_version_id.clone());
                    x.live_published_id = Some(v.id.clone());
                    x.live_revision_id = Some(v.revision_id.clone());
                    clear_lease(x);
                }
                ok
            })
            .unwrap_or(false);
        self.s.borrow_mut().audit.push(audit.clone());
        Ok(applied)
    }

    async fn published(
        &self,
        ws: &str,
        site: &str,
        id: &str,
    ) -> Result<Option<PublishedVersion>, StoreError> {
        Ok(self
            .s
            .borrow()
            .published
            .iter()
            .find(|p| p.workspace_id == ws && p.site_id == site && p.id == id)
            .cloned())
    }

    async fn audit(&self, row: &AuditRow) -> Result<(), StoreError> {
        self.s.borrow_mut().audit.push(row.clone());
        Ok(())
    }
}
