//! Actionable, backend-independent error codes for asset operations.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetError {
    Unauthenticated,
    Forbidden,
    SiteNotFound,
    AssetNotFound,
    InvalidSha256,
    EmptyAsset,
    ContentLengthMismatch,
    AssetTooLarge,
    UnsupportedContentType,
    ContentTypeMismatch,
    Sha256Mismatch,
    SiteQuotaExceeded,
    WorkspaceQuotaExceeded,
    SiteAssetCountExceeded,
    GeneratedBudgetExceeded,
    ReservationExpired,
    R2NotConfigured,
    BackendInvalid,
    UploadBusy,
    StoreFailed,
}

impl AssetError {
    pub fn status(self) -> u16 {
        use AssetError::*;
        match self {
            Unauthenticated => 401,
            Forbidden => 403,
            SiteNotFound | AssetNotFound => 404,
            InvalidSha256 | EmptyAsset | ContentLengthMismatch => 400,
            AssetTooLarge => 413,
            UnsupportedContentType | ContentTypeMismatch => 415,
            Sha256Mismatch => 422,
            SiteQuotaExceeded
            | WorkspaceQuotaExceeded
            | SiteAssetCountExceeded
            | GeneratedBudgetExceeded
            | ReservationExpired => 409,
            R2NotConfigured | BackendInvalid | UploadBusy => 503,
            StoreFailed => 502,
        }
    }

    pub fn code(self) -> &'static str {
        use AssetError::*;
        match self {
            Unauthenticated => "unauthenticated",
            Forbidden => "forbidden",
            SiteNotFound => "site_not_found",
            AssetNotFound => "asset_not_found",
            InvalidSha256 => "invalid_sha256",
            EmptyAsset => "empty_asset",
            ContentLengthMismatch => "content_length_mismatch",
            AssetTooLarge => "asset_too_large",
            UnsupportedContentType => "unsupported_content_type",
            ContentTypeMismatch => "content_type_mismatch",
            Sha256Mismatch => "sha256_mismatch",
            SiteQuotaExceeded => "site_quota_exceeded",
            WorkspaceQuotaExceeded => "workspace_quota_exceeded",
            SiteAssetCountExceeded => "site_asset_count_exceeded",
            GeneratedBudgetExceeded => "generated_budget_exceeded",
            ReservationExpired => "reservation_expired",
            R2NotConfigured => "r2_not_configured",
            BackendInvalid => "asset_backend_invalid",
            UploadBusy => "upload_busy",
            StoreFailed => "store_failed",
        }
    }

    /// What the caller (or operator) can do about it.
    pub fn hint(self) -> &'static str {
        use AssetError::*;
        match self {
            Unauthenticated => "Sign in; Studio authentication is required for asset routes.",
            Forbidden => "You are not a member of this workspace.",
            SiteNotFound => "No such site in this workspace.",
            AssetNotFound => "No asset with this sha256 in this site.",
            InvalidSha256 => "Use the lowercase 64-character hex SHA-256 of the file bytes.",
            EmptyAsset => "Upload a non-empty file.",
            ContentLengthMismatch => {
                "The body was longer than the declared Content-Length; send the exact length."
            }
            AssetTooLarge => "Reduce the file below the per-asset byte cap reported in `limit`.",
            UnsupportedContentType => {
                "Allowed types: image/png, image/jpeg, image/gif, image/webp, image/avif, font/woff2."
            }
            ContentTypeMismatch => "The file bytes do not match the declared Content-Type.",
            Sha256Mismatch => "The SHA-256 of the uploaded bytes differs from the one in the URL.",
            SiteQuotaExceeded => "Delete unused assets from this site to free space.",
            WorkspaceQuotaExceeded => {
                "Delete unused assets elsewhere in the workspace to free space."
            }
            SiteAssetCountExceeded => {
                "Delete unused assets from this site; the file count cap is reached."
            }
            GeneratedBudgetExceeded => {
                "Preview/export budget reached; old artifacts expire, or run reconciliation."
            }
            ReservationExpired => "The upload took too long; retry it.",
            R2NotConfigured => {
                "STUDIO_ASSET_BACKEND=r2 but no MEDIA_BUCKET R2 binding exists. Add the opt-in R2 binding (see deploy/cloudflare/wrangler.studio.example.jsonc) or unset STUDIO_ASSET_BACKEND to use cardless D1 storage."
            }
            BackendInvalid => "STUDIO_ASSET_BACKEND must be `d1` (default) or `r2`.",
            UploadBusy => "Too many uploads are in flight on this instance; retry after a moment.",
            StoreFailed => "The storage backend failed; the reservation was released. Retry later.",
        }
    }
}

impl core::fmt::Display for AssetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.code(), self.hint())
    }
}

impl std::error::Error for AssetError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_snake_case_with_a_hint() {
        use AssetError::*;
        let all = [
            Unauthenticated,
            Forbidden,
            SiteNotFound,
            AssetNotFound,
            InvalidSha256,
            EmptyAsset,
            ContentLengthMismatch,
            AssetTooLarge,
            UnsupportedContentType,
            ContentTypeMismatch,
            Sha256Mismatch,
            SiteQuotaExceeded,
            WorkspaceQuotaExceeded,
            SiteAssetCountExceeded,
            GeneratedBudgetExceeded,
            ReservationExpired,
            R2NotConfigured,
            BackendInvalid,
            UploadBusy,
            StoreFailed,
        ];
        let mut seen = std::collections::HashSet::new();
        for e in all {
            assert!(seen.insert(e.code()), "{e:?}");
            assert!(
                e.code()
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            );
            assert!(!e.hint().is_empty());
            assert!((400..600).contains(&e.status()));
        }
    }
}
