//! sws-assets: pure, host-testable asset-store policy for the Studio control plane.
//!
//! Everything here is plain Rust with no I/O, so the Worker (`sws-worker-studio`),
//! the CLI and host tests share one definition of the limits and decisions:
//!
//! - [`limits`]: provisional cardless/R2/generated budgets, retention constants and
//!   the CLI publish-artifact manifest bounds (#64).
//! - [`validate`]: id/sha256 validation, content-type allowlist, magic-byte sniffing,
//!   and storage key/path builders that cannot traverse.
//! - [`hash`] and [`chunk`]: SHA-256 and the D1 chunking plan.
//! - [`quota`]: upload planning, the streamed byte counter and quota diagnostics.
//! - [`reconcile`]: retention and orphan decisions.
//! - [`error`]: actionable error codes shared by every backend.
//! - `local` (native only, never compiled for `wasm32`): the offline filesystem store.
//!
//! The limits are PROVISIONAL assumptions until verified on a fresh Cloudflare
//! account (see `docs/ARCHITECTURE.md`). Application caps and alerts reduce billing
//! risk; they are not provider-side hard stops.

pub mod chunk;
pub mod error;
pub mod hash;
pub mod limits;
pub mod quota;
pub mod reconcile;
pub mod time;
pub mod validate;

#[cfg(not(target_arch = "wasm32"))]
pub mod local;
