//! Upload planning, streamed byte enforcement and quota diagnostics.
//!
//! The reservation algorithm (the same on D1, R2 and the local store):
//!
//! 1. [`plan_upload`] decides how many bytes to reserve BEFORE reading the body:
//!    the validated `Content-Length`, or the per-asset cap when it is absent.
//! 2. The backend reserves atomically: usage = committed + live (unexpired)
//!    reservations; the reservation is inserted only if usage + request fits every
//!    cap. On D1 this is one conditional `INSERT ... SELECT ... WHERE`; two
//!    concurrent requests can never both pass a check that only one fits.
//! 3. The body is read through a [`StreamCounter`] limited to the reservation.
//!    `Content-Length` is never trusted: bytes actually received are counted.
//! 4. Commit stores the ACTUAL size and deletes the reservation in one transaction;
//!    failure releases it. A crashed request's reservation simply expires.

use crate::error::AssetError;
use crate::limits::AssetLimits;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadPlan {
    /// Bytes to reserve and to allow on the wire.
    pub reserve_bytes: u64,
    /// True when `reserve_bytes` came from a valid `Content-Length`.
    pub declared: bool,
}

/// Validate framing before any body byte is read.
pub fn plan_upload(
    content_length: Option<&str>,
    limits: &AssetLimits,
) -> Result<UploadPlan, AssetError> {
    let Some(raw) = content_length else {
        return Ok(UploadPlan {
            reserve_bytes: limits.max_asset_bytes,
            declared: false,
        });
    };
    let raw = raw.trim();
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AssetError::ContentLengthMismatch);
    }
    // Digits that overflow u64 are certainly over the cap.
    let n: u64 = raw.parse().map_err(|_| AssetError::AssetTooLarge)?;
    match n {
        0 => Err(AssetError::EmptyAsset),
        n if n > limits.max_asset_bytes => Err(AssetError::AssetTooLarge),
        n => Ok(UploadPlan {
            reserve_bytes: n,
            declared: true,
        }),
    }
}

/// Counts bytes as they arrive; the error names which promise was broken.
#[derive(Debug)]
pub struct StreamCounter {
    limit: u64,
    declared: bool,
    seen: u64,
}

impl StreamCounter {
    pub fn new(plan: UploadPlan) -> Self {
        Self {
            limit: plan.reserve_bytes,
            declared: plan.declared,
            seen: 0,
        }
    }

    pub fn seen(&self) -> u64 {
        self.seen
    }

    /// Stop reading at the first `Err`.
    pub fn push(&mut self, n: usize) -> Result<(), AssetError> {
        self.seen = self.seen.saturating_add(n as u64);
        if self.seen <= self.limit {
            Ok(())
        } else if self.declared {
            Err(AssetError::ContentLengthMismatch)
        } else {
            Err(AssetError::AssetTooLarge)
        }
    }

    /// After the stream ends: a declared length must match exactly.
    pub fn finish(&self) -> Result<u64, AssetError> {
        if self.seen == 0 {
            Err(AssetError::EmptyAsset)
        } else if self.declared && self.seen != self.limit {
            Err(AssetError::ContentLengthMismatch)
        } else {
            Ok(self.seen)
        }
    }
}

/// May this isolate take on `request` more buffered upload bytes? Always admits a
/// request when nothing else is in flight (the per-asset cap is already enforced).
pub fn admit_inflight(current: u64, request: u64, budget: u64) -> bool {
    current == 0 || current.saturating_add(request) <= budget
}

/// Stored plus reserved usage, as read after a refused reservation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub site_bytes: u64,
    pub site_assets: u64,
    pub workspace_bytes: u64,
}

/// Which cap refused `request` bytes (site checked first: it is the more actionable).
pub fn diagnose(usage: &Usage, request: u64, limits: &AssetLimits) -> AssetError {
    if usage.site_assets >= limits.max_site_assets {
        AssetError::SiteAssetCountExceeded
    } else if usage.site_bytes.saturating_add(request) > limits.max_site_bytes {
        AssetError::SiteQuotaExceeded
    } else {
        AssetError::WorkspaceQuotaExceeded
    }
}

/// Would a reservation of `request` bytes fit? Mirrors the SQL `WHERE` clause.
pub fn fits(usage: &Usage, request: u64, limits: &AssetLimits) -> bool {
    usage.site_assets < limits.max_site_assets
        && usage.site_bytes.saturating_add(request) <= limits.max_site_bytes
        && usage.workspace_bytes.saturating_add(request) <= limits.max_workspace_bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::{CARDLESS_D1, MIB};

    #[test]
    fn plan_validates_framing() {
        let l = CARDLESS_D1;
        assert_eq!(
            plan_upload(None, &l),
            Ok(UploadPlan {
                reserve_bytes: MIB,
                declared: false
            })
        );
        assert_eq!(
            plan_upload(Some("10"), &l),
            Ok(UploadPlan {
                reserve_bytes: 10,
                declared: true
            })
        );
        assert_eq!(
            plan_upload(Some(&MIB.to_string()), &l).map(|p| p.reserve_bytes),
            Ok(MIB)
        );
        assert_eq!(
            plan_upload(Some(&(MIB + 1).to_string()), &l),
            Err(AssetError::AssetTooLarge)
        );
        assert_eq!(
            plan_upload(Some("99999999999999999999999"), &l),
            Err(AssetError::AssetTooLarge)
        );
        assert_eq!(plan_upload(Some("0"), &l), Err(AssetError::EmptyAsset));
        for bad in ["", "-1", "1.5", "0x10", "1 2"] {
            assert_eq!(
                plan_upload(Some(bad), &l),
                Err(AssetError::ContentLengthMismatch),
                "{bad}"
            );
        }
    }

    #[test]
    fn counter_enforces_actual_bytes_without_content_length() {
        let mut c = StreamCounter::new(plan_upload(None, &CARDLESS_D1).unwrap());
        assert!(c.push(MIB as usize).is_ok());
        assert_eq!(c.push(1), Err(AssetError::AssetTooLarge));
        // And in many small pieces.
        let mut c = StreamCounter::new(plan_upload(None, &CARDLESS_D1).unwrap());
        let mut err = None;
        for _ in 0..(MIB / 1000 + 2) {
            if let Err(e) = c.push(1000) {
                err = Some(e);
                break;
            }
        }
        assert_eq!(err, Some(AssetError::AssetTooLarge));
    }

    #[test]
    fn counter_catches_a_lying_content_length() {
        let plan = plan_upload(Some("100"), &CARDLESS_D1).unwrap();
        let mut c = StreamCounter::new(plan);
        assert_eq!(c.push(101), Err(AssetError::ContentLengthMismatch));
        let mut short = StreamCounter::new(plan);
        short.push(50).unwrap();
        assert_eq!(short.finish(), Err(AssetError::ContentLengthMismatch));
        let mut ok = StreamCounter::new(plan);
        ok.push(100).unwrap();
        assert_eq!(ok.finish(), Ok(100));
        assert_eq!(
            StreamCounter::new(plan).finish(),
            Err(AssetError::EmptyAsset)
        );
    }

    #[test]
    fn inflight_gate_bounds_buffered_bytes() {
        use crate::limits::MAX_INFLIGHT_UPLOAD_BYTES as B;
        assert!(
            admit_inflight(0, B + 1, B),
            "an idle isolate always admits one"
        );
        assert!(admit_inflight(B - MIB, MIB, B));
        assert!(!admit_inflight(B - MIB, MIB + 1, B));
        assert!(!admit_inflight(B, 1, B));
    }

    #[test]
    fn diagnosis_names_the_cap() {
        let l = CARDLESS_D1;
        let u = Usage {
            site_bytes: l.max_site_bytes,
            site_assets: 3,
            workspace_bytes: 0,
        };
        assert!(!fits(&u, 1, &l));
        assert_eq!(diagnose(&u, 1, &l), AssetError::SiteQuotaExceeded);
        let u = Usage {
            site_bytes: 0,
            site_assets: 3,
            workspace_bytes: l.max_workspace_bytes,
        };
        assert!(!fits(&u, 1, &l));
        assert_eq!(diagnose(&u, 1, &l), AssetError::WorkspaceQuotaExceeded);
        let u = Usage {
            site_bytes: 0,
            site_assets: l.max_site_assets,
            workspace_bytes: 0,
        };
        assert_eq!(diagnose(&u, 1, &l), AssetError::SiteAssetCountExceeded);
        assert!(fits(&Usage::default(), l.max_asset_bytes, &l));
    }
}
