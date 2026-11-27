//! JSON Schema (Draft 2020-12) for [`crate::SiteDocument`].
//!
//! The schema is **generated from the Rust types** by `sws-codegen`
//! (`npm run codegen`) and committed under
//! `packages/@solo-website-system/ir-types/schema/`. This module embeds that
//! committed file, so the public function keeps working with no derive code in
//! the Wasm / Worker builds. With the host-only `jsonschema` feature (the
//! generator build) the schema is built live from the types instead, so
//! `sws-codegen` never needs the committed file to compile and recreate it;
//! a test asserts the two are identical. `npm run codegen:check` fails if the
//! file drifts from the types.
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

/// Canonical `$id` of the `SiteDocument` schema (kept from the original
/// hand-written schema so existing references stay valid). The single source
/// of truth: `sws-codegen` re-exports it.
pub const SITE_DOCUMENT_ID: &str =
    "https://solo-website-system.example/schemas/site-document.v1.json";

/// Generator builds (`jsonschema` feature) derive the schema live, so they
/// never depend on the committed file existing.
#[cfg(not(feature = "jsonschema"))]
const GENERATED: &str = include_str!(
    "../../../packages/@solo-website-system/ir-types/schema/site-document.schema.json"
);

/// Return the Draft 2020-12 JSON Schema describing [`crate::SiteDocument`].
///
/// Its `$id` is `https://solo-website-system.example/schemas/site-document.v1.json`.
#[must_use]
pub fn site_document_json_schema() -> Value {
    #[cfg(feature = "jsonschema")]
    {
        crate::jsonschema::root_schema::<crate::SiteDocument>(SITE_DOCUMENT_ID, true)
    }
    #[cfg(not(feature = "jsonschema"))]
    {
        serde_json::from_str(GENERATED).unwrap_or(Value::Null)
    }
}

#[cfg(all(test, feature = "jsonschema"))]
mod tests {
    use super::*;

    #[test]
    fn live_schema_equals_the_committed_embedded_schema() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packages/@solo-website-system/ir-types/schema/site-document.schema.json"
        );
        let committed: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("committed schema"))
                .expect("committed schema parses");
        assert_eq!(site_document_json_schema(), committed);
        assert_eq!(committed["$id"], SITE_DOCUMENT_ID);
    }
}
