//! Draft previews served from an isolated origin (#67).
//!
//! * [`origin`]: `STUDIO_PREVIEW_ORIGIN` validation and the per-request surface
//!   decision (preview host = preview routes only; fail closed).
//! * [`bundle`]: the `sws-preview-bundle/1` format, upload validation and path
//!   resolution.
//! * [`serve`]: preview-host routing, tokens, the 404/410 decision and the
//!   `preview` header set; producer route parsing on the Studio host.
//! * [`sql`]: the `previews` table statements (`migrations/0006_previews.sql`).
//! * `http` (wasm only): the I/O glue for both hosts.
//!
//! Everything a preview stores goes through the #63 generated-artifact store
//! under the existing Preview budget; there is no second budget.

pub mod bundle;
pub mod origin;
pub mod serve;
pub mod sql;

#[cfg(target_arch = "wasm32")]
pub mod http;
