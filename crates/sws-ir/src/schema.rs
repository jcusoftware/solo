//! JSON Schema (Draft 2020-12) for [`crate::SiteDocument`].
//!
//! The schema is **generated from the Rust types** by `sws-codegen`
//! (`npm run codegen`) and committed under
//! `packages/@solo-website-system/ir-types/schema/`. This module embeds that
//! committed file, so the public function keeps working with no derive code in
//! the Wasm / Worker builds. `npm run codegen:check` fails if the file drifts
//! from the types.
//!
//! # Unknown-field policy
//!
//! The schema is strict: `additionalProperties: false` on every struct-like
//! object, so authoring tools reject typos. The Rust parser is tolerant: it
//! ignores unknown keys, so `solo build` / `solo check` accept documents the
//! schema rejects. That difference is deliberate. The schema also cannot
//! express document-wide rules (unique page ids, route canonicalisation,
//! https URLs); run [`crate::validate_ir`] as well.

use serde_json::Value;

const GENERATED: &str = include_str!(
    "../../../packages/@solo-website-system/ir-types/schema/site-document.schema.json"
);

/// Return the Draft 2020-12 JSON Schema describing [`crate::SiteDocument`].
///
/// Its `$id` is `https://solo-website-system.example/schemas/site-document.v1.json`.
#[must_use]
pub fn site_document_json_schema() -> Value {
    serde_json::from_str(GENERATED).unwrap_or(Value::Null)
}
