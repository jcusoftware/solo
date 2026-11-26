# IR Compatibility and Stable Node ID Audit

Audit for issue #54. Baseline: workspace version 0.1.2 (`Cargo.toml`), commit `1cae11f`.
Consumers of these findings: #55 (canonical Rust editor command reducer with undo/redo) and #56 (TypeScript types and JSON Schema generated from Rust).
`MIT OR Apache-2.0`.

## 0. Scope, method and conventions

This is a documentation-only audit. It changes no serialized format and no Rust or JavaScript behaviour.

- The issues cite a "Studio Control Plane and Visual Builder design" document that is not in this repository. Everything below comes from the issue bodies and the code. Each claim about current behaviour cites a file and a symbol (or line, at the audited commit).
- Behaviour marked **(probed)** was also confirmed by running the built `solo` binary on a scratch input (outside the repo) via `solo mcp` `read_site_ir`, `solo check` and `solo build`. No file in the repo was written by those probes.
- Anything the issues do not determine is labelled **Proposed (needs maintainer confirmation)**. Those items are consolidated in section 10.
- Examples use only the fictional `shirlington-hearth-demo` (`packs/hospitality/demo/venue.json`) and generic `example` domains, per the Zone A rule in `AGENTS.md`.
- Terminology: "IR" is `sws_ir::SiteDocument` (`crates/sws-ir/src/types.rs`). The "pack input" is the hospitality `VenueManifest` (`packs/hospitality/crate/src/venue.rs`). "Legacy document" means any file that is valid under v0.1.2 and carries no editor-assigned IDs.

## 1. Current serialized formats

### 1.1 Global serde facts (`crates/sws-ir`)

| Fact | Evidence |
| --- | --- |
| No `#[serde(deny_unknown_fields)]` anywhere in the workspace. Unknown keys are silently ignored on parse and silently absent on re-serialize. | `grep deny_unknown_fields crates packs` returns nothing. **(probed)**: top-level `x_ext`, a page-level `future` and a node-level `unknown` were all dropped by `read_site_ir`. |
| No `#[serde(flatten)]` in `sws-ir`, `sws-compiler`, `sws-plugin` or the pack. | grep. |
| `skip_serializing_if` is used in `CompileOptions.extra_files` (`crates/sws-compiler/src/lib.rs`), `CheckIssue.citation`, `PluginDiagnostic.citation` and heavily in the pack `VenueManifest`, but **never in `sws-ir`**. So every IR `Option` serializes as `null` and every empty `Vec`/`BTreeMap` as `[]`/`{}`. | `crates/sws-ir/src/types.rs` has none; **(probed)** output contains `"description": null`, `"heading": null`, `"id": null`. |
| `#[serde(rename_all = "snake_case")]` on `PropKind`, `FormFieldKind`. Internal tagging: `Node` uses `#[serde(tag = "type", rename_all = "snake_case")]`, `RoutePattern` uses `#[serde(tag = "kind", rename_all = "snake_case")]`, `IrPatch` uses `#[serde(tag = "op", rename_all = "snake_case")]`. The only field-level rename in the workspace IR/pack path is `VenueManifest.schema` renamed to `$schema`. | `types.rs`, `patch.rs`, `venue.rs`. |
| Defaults: `schema_version` defaults to 1, `locale` to `en-US`, `FormSchema.method` to `POST`; all other optional or collection fields use `#[serde(default)]`. Required (no default): `slug`, `name`, `base_url`, `Page.id`, `Page.route`, `Page.title`, `Node::Element.tag`, `Node::Text.value`, and the other tag-specific fields. | `types.rs` (`default_schema_version`, `default_locale`, `default_form_method`). |
| Unknown enum tags are hard parse errors, not ignored (`Node.type`, `RoutePattern.kind`, `PropKind`, `FormFieldKind`). | serde internally-tagged enum semantics; `solo check` reports `ir_parse_error` (`run_checks_on_value` in `crates/sws-cli/src/commands.rs`). |
| Maps are `BTreeMap`, so key order is deterministic (sorted by key). Arrays (`Vec`) keep author order. `serde_json` is `"1"` without `preserve_order` in every crate manifest, so free-form `serde_json::Value` objects are also key-sorted on re-serialize. **(probed)**: `metadata.k` given as `{z, a}` came back `{a, z}`. | `types.rs` module doc and field types; `crates/*/Cargo.toml`. |
| The hand-written JSON Schema (`crates/sws-ir/src/schema.rs`, `site_document_json_schema`) declares `additionalProperties: false` on the root and on nearly every `$defs` object, which the Rust parser does **not** enforce. The schema is hand-maintained JSON (`json!`), not derived from the types, and no test compares it with the serde shape (the only schema test, `json_schema_is_draft_2020_12_and_deterministic` in `crates/sws-ir/src/lib.rs`, checks `$schema`, title, and the `oneOf` lengths 6 and 2). | `schema.rs` lines 14 onwards; `lib.rs` tests. |

### 1.2 `SiteDocument` structure (`crates/sws-ir/src/types.rs`)

Field order in the struct, which is the serialization order (serde emits struct fields in declaration order):

| Field | Type | Collection kind / ordering |
| --- | --- | --- |
| `schema_version` | `u32` (default 1) | scalar; `validate_ir` rejects anything other than 1 (`validate.rs`, `schema_version != 1`) |
| `slug` | `String` | scalar; must match `^[a-z0-9]+(-[a-z0-9]+)*$` (`is_valid_slug`) |
| `name`, `locale`, `base_url` | `String` | scalars; `base_url` must be https (`is_valid_https_url`) |
| `tokens` | `DesignTokens` | `colors`, `spacing`, `radii`: `BTreeMap<String,String>` (sorted); `fonts`: `FontTokens {body, heading?, mono?}`; `contrast_pairs`: `Vec<ContrastPair>` (author order) |
| `breakpoints` | `Vec<Breakpoint>` | array, author order; each has required `id` |
| `styles` | `Vec<IrStyleRule>` | array, author order; each has required `id`; `declarations` is a `BTreeMap` |
| `components` | `BTreeMap<String, ComponentDef>` | map keyed by component name, sorted |
| `forms` | `BTreeMap<String, FormSchema>` | map keyed by form key, sorted; `FormSchema.fields` is an ordered array |
| `pages` | `Vec<Page>` | array, author order (this order is semantic: it drives output iteration) |
| `seo` | `SeoMeta` | arrays of strings, author order |
| `scripts` | `Vec<ScriptRef>` | array, author order |
| `data` | `BTreeMap<String, Value>` | free-form extension map |
| `metadata` | `BTreeMap<String, Value>` | free-form extension map |

`Page` (`types.rs`): `id`, `route`, `title`, `description?`, `canonical?`, `noindex`, `is_not_found`, `json_ld: Vec<JsonLdBlock>`, `scripts`, `body: Vec<Node>`. `route` is `RoutePattern::Static{path}` or `Dynamic{pattern, params}`.

`Node` (`types.rs`) has six variants. Only `Element` has an `id`:

| Variant | Fields | Children containers |
| --- | --- | --- |
| `element` | `id?`, `tag`, `attrs: BTreeMap<String,String>`, `classes: Vec<String>`, `style_refs`, `children: Vec<Node>` | `children` |
| `text` | `value` | none |
| `slot_ref` | `name`, `fallback: Vec<Node>` | `fallback` |
| `component_instance` | `component`, `props: BTreeMap<String,Value>`, `slots: BTreeMap<String, Vec<Node>>` | each slot array |
| `conditional` | `expr`, `then_nodes`, `else_nodes` | both arrays |
| `loop` | `item_var`, `collection_expr`, `body` | `body` |

There is no "section" or "block" type in the IR. A "section" is an `Element` whose tag is `section` (or any structural child of `main`). Pack "blocks" exist only on the manifest side (section 1.4) and are lowered into plain `Element` trees.

Where deterministic ordering holds: all maps are sorted, so equal documents serialize byte-identically regardless of insertion order (test `serde_round_trip_and_deterministic_json`, `crates/sws-ir/src/lib.rs`). Arrays preserve author order and are never re-sorted by the IR layer.

### 1.3 Route structures

`RoutePattern` is internally tagged by `kind`. Validation (`validate_ir`, `validate.rs`): static paths must pass `is_valid_static_route`; dynamic patterns `is_valid_dynamic_route` and must declare at least one param; routes are deduplicated through `canonical_route_key` (trailing slash trimmed); `Page.id` must be non-empty and unique (`pages[i].id` / "duplicate page id", `validate.rs` around line 272 to 286). Page ID content is otherwise unconstrained.

### 1.4 Hospitality pack input (`packs/hospitality`)

- **Source file:** `packs/hospitality/demo/venue.json` (fictional `shirlington-hearth-demo`). Typed by `VenueManifest` in `packs/hospitality/crate/src/venue.rs`. `schema_version` must be 1 (`validate_venue`). Top-level keys in the demo: `$schema`, `schema_version`, `slug`, `name`, `legal_disclaimer`, `fictional`, `tagline`, `description`, `category_label`, `business_types`, `cuisine`, `price_range`, `locale`, `timezone`, `nap`, `geo`, `hours`, `holiday_overrides`, `booking_url`, `venue_accessibility`, `brand`, `modules`, `integrations`, `newsletter`, `scheduling`, `checkout`, `happy_hour`, `menu`, `seo`.
- **Schema:** `packs/hospitality/schema/venue.schema.json` (`additionalProperties: false` at the root and in nested objects). As in the IR, the Rust `VenueManifest` does not use `deny_unknown_fields`, so unknown keys parse and are dropped; the JSON Schema is the only strict gate and `solo check` does not run it.
- **Serialization style:** `VenueManifest` uses `skip_serializing_if` on most optionals (the opposite of the IR), so a venue round trip drops absent options rather than writing `null`. `booking_url` and a few nested option fields are `#[serde(default)]` without `skip_serializing_if` and serialize as `null`.
- **Maps vs arrays:** `hours` and `modules` and `brand.palette` are `BTreeMap` (sorted keys, not the author's day order); `menu.sections`, `menu.sections[].items`, `holiday_overrides`, `checkout.items`, `cuisine`, `happy_hour.days` are arrays and keep author order.
- **Lowering into IR:** `HospitalityPlugin::can_transform_input` claims any object with `menu`, `nap` and `hours` keys (`packs/hospitality/crate/src/compiler.rs`, `can_transform_input`). `transform_input` parses the manifest, runs `validate_venue`, calls `build_site_document` (same file), then inserts the **entire raw input** into `doc.metadata["venue_manifest"]`. `build_site_document` hard-codes: `schema_version: 1`, three breakpoints (`sm`, `md`, `print`), the style rules, and pages in this order: `index` (always), `events` (if `scheduling.events.enabled`), `book-a-call` (if `scheduling.bookings.enabled`), `not-found` (always), `takeout` (if `checkout` is present). Pages get fixed IDs; the index page has one JSON-LD block with `id: Some("venue")`. **(probed)**: the lowered demo has 5 pages and 861 nodes, 79 style rules, 0 components, 0 forms.
- **Lowering is not reversible.** The IR produced from a venue is generated output. There is no inverse function from IR back to `venue.json`, and the pack builds its HTML from the manifest, not from IR edits.
- **Pack extra files are not part of the IR.** `pack_extra_files` (`crates/sws-cli/src/commands.rs`) derives `events-config.json`, `booking-config.json`, `demo-checkout-catalog.json` and `js/events-client.js`/`js/booking-client.js` from the raw manifest only when the top level has `menu`, `nap` and `hours`. **(probed)**: for the demo, `solo build` on the lowered-and-saved IR produced byte-identical HTML/CSS and the same files as `solo build venue.json`, except those five pack assets, which are absent from the IR build. This matters for #55's native-vs-Wasm byte comparison (section 8).

## 2. Stable-ID inventory

### 2.1 Existing identifiers

| Entity | Identifier today | Required? | Uniqueness enforced? | Notes |
| --- | --- | --- | --- | --- |
| `SiteDocument` | `slug` | required | n/a | Kebab slug; document identity, not a node ID |
| `Page` | `id: String` | **required** | **yes**, non-empty and unique (`validate_ir`) | Also addressed by `route` (unique after `canonical_route_key`). Charset unconstrained. |
| `Node::Element` | `id: Option<String>` | optional | **no** | See 2.2: doubles as the rendered HTML `id`. |
| `Node::{Text,SlotRef,ComponentInstance,Conditional,Loop}` | none | n/a | n/a | Addressable only by position. `SlotRef` has `name`, `ComponentInstance` has `component` (a reference, not an ID). |
| `JsonLdBlock` | `id: Option<String>` | optional | no | Pack sets `"venue"` on the index page. |
| `Breakpoint` | `id` | required | **no** (compiler defines by id: `sheet.define_breakpoint(&bp.id, ...)`, `sws-compiler/src/lib.rs`) | |
| `IrStyleRule` | `id` | required | **no**; `UpsertStyleRule` replaces the first match (`patch.rs`) | |
| `FormSchema` | `id` plus the map key | `id` required | no check that key equals `id`; plugin merge is keyed by `FormSchema::id` (`PluginRegistry::apply_to_document`, `sws-plugin/src/lib.rs`) | |
| `ComponentDef` | `name` plus the map key | required | no check that key equals `name`; plugin merge keyed by `ComponentDef::name` | |
| `FormFieldSchema` | `name` | required | no | |
| `FormFieldOption` | `value` | required | no | |
| `ContrastPair`, `ScriptRef` | none | n/a | n/a | Positional only (`tokens.contrast_pairs[idx]` is how `validate_ir` addresses errors). |
| Design tokens | map key (`tokens.colors["accent"]`) | n/a | yes (map keys) | Addressed by key in `SetColorToken`. Font slots by fixed name (`body`/`heading`/`mono`). |
| `data` / `metadata` entries | map key | n/a | yes | `SetDataValue` addresses `data` by key. No patch writes `metadata`. |

### 2.2 `Node::Element.id` is a rendered attribute, not a pure editor ID

`expand.rs` (`expand_single`, the `Node::Element` arm, lines 372 to 376) copies `Node::Element.id` into the output attribute `id` and overrides any `attrs["id"]`. So populating `Node.id` on a legacy document **changes compiled HTML**. A stable editor ID must not be stored there unless the maintainers accept an output change. Evidence against reusing it:

- The pack never sets `Node.id`: its `el()` helper (`compiler.rs`, `fn el`) always writes `id: None`, and the pack sets HTML IDs through `attrs["id"]` instead. **(probed)**: the lowered demo has 0 elements with `id` set and 50 with `attrs.id` (for example `main`, `visit`, `happy-hour`, `menu`, `menu-coffee`, `newsletter`).
- Those HTML IDs are not document-unique: `main` appears on the index, events and booking pages. They are unique only per rendered page.
- `HTML id` uniqueness is not validated by `validate_ir`. **(probed)**: two sibling elements with `"id": "a"` pass `solo check` with no ID diagnostic.

### 2.3 Hospitality pack identifiers

| Pack entity | Identifier | Required | Uniqueness enforced |
| --- | --- | --- | --- |
| `menu.sections[]` | `id` slug (`MenuSection.id`) | required; slug-checked by `validate_venue` (`menu section id invalid`) | **no** duplicate check (no `BTreeSet`/"duplicate" logic in `venue.rs`) |
| `menu.sections[].items[]` | none | n/a | positional only |
| `checkout.items[]`, `checkout.subscription` | `id` | required | **yes**: `validate_checkout_config` requires "distinct ids" |
| `modules` | map key `M1`..`M10` | n/a | map keys |
| `hours` | map key (day name) | n/a | map keys; the set of days is validated |
| `holiday_overrides[]`, `happy_hour` | none (`date` and `label` are data, not IDs) | n/a | positional |
| Lowered pages | hard-coded page IDs (`index`, `events`, `book-a-call`, `not-found`, `takeout`) | required in IR | page-ID uniqueness holds by construction |
| Lowered section elements | HTML `attrs.id`, derived: menu heading id is `menu-{section.id}` (`menu_section_nodes`) | n/a | derived, so inherits the no-uniqueness gap above |

"Pack blocks" therefore have stable IDs at the manifest level only for menu sections and checkout items; nothing else does, and nothing in the lowered IR carries an ID that traces back to the manifest.

### 2.4 How nodes are addressed today

| Consumer | Addressing |
| --- | --- |
| `IrPatch` (`patch.rs`) | Pages by `Page.id`; nodes by `(page_id, node_id)` where `node_id` matches `Node::Element.id` only, via depth-first first-match search (`find_element_mut`, `remove_element_by_id`). Insert position by parent id plus `index` (an array index, `None` means append). Tokens by key or slot name; styles by `IrStyleRule.id`; data by key. |
| `validate_ir` diagnostics | JSON-pointer-like index paths (`pages[0].body[2].children[1]`, `validate_nodes` in `validate.rs`). |
| Compiler diagnostics (`sws-compiler/src/diagnostics.rs`) | Index paths (`pages[0]`) and page IDs in messages. |
| Pack ABC lint | JSON paths into the raw manifest (`lint_json_value`). |

Gaps: nodes with `id: None` (all pack-generated nodes and all non-`Element` variants) cannot be targeted by any patch. `find_element_mut` does not search `components[*].template`, only `page.body`.

### 2.5 Existing structural patch semantics (`crates/sws-ir/src/patch.rs`)

`IrPatch` has 11 operations; `apply_patch(&mut SiteDocument, &IrPatch) -> Result<(), IrError>` applies one:

| Op | Semantics | Notable behaviour for a reducer |
| --- | --- | --- |
| `set_site_meta` | optional `name`, `locale`, `base_url`; validated before mutation | Atomic per op. |
| `set_color_token` | `tokens.colors.insert(key, hex)`; hex must be `#RRGGBB` | Insert-or-overwrite; no "previous value" returned. |
| `set_font_token` | slot `body`/`heading`/`mono` | Same. |
| `upsert_page` | replaces the page with the same `id` in place, else **pushes to the end** | Replaces the whole subtree. Does not validate route or ID uniqueness (only `validate_ir` does). |
| `remove_page` | remove by `id` | Errors `PageNotFound`. |
| `set_node_text` | replaces **all** children of the target element with one `Text` node | Destructive: nested elements under the target are lost. No inverse available. |
| `set_node_attr` | set or remove (`value: None`) one attr | Setting `attrs["id"]` is allowed (no check). |
| `insert_child_node` | insert a whole `Node` (with arbitrary descendants) under `parent_id` or the page body at `index` | No check that inserted subtree IDs are unique; no check `index <= len` other than `InvalidIndex`. Parent must be an `Element`. |
| `remove_node` | remove first `Element` whose `id` matches | Removes the subtree; returns nothing, so an inverse needs the subtree captured beforehand. |
| `upsert_style_rule` | replace first rule with same `id`, else push | |
| `set_data_value` | `data.insert(key, value)` | |

Missing for #55: **no move/reorder** (pages, sections or nodes), **no rename** (page ID, node ID, token key), **no delete for tokens, styles, data**, **no component-template editing**, and no batch or transaction type: operations are independent.

Batching today: only the MCP tool `apply_ir_patch` (`crates/sws-cli/src/mcp.rs`, `tool_apply_ir_patch`) applies an array in order against an in-memory document and returns the whole updated document; if patch `k` fails, the function returns `Err("patch[k] failed: ...")` and the partially mutated document is discarded, so the tool is atomic at the call level, but `apply_patch` itself has no rollback and no undo information.

Tests covering patches: `apply_all_patch_operations` (`crates/sws-ir/src/lib.rs`), `mcp_stdio_round_trip_supports_all_methods_and_tools` (`crates/sws-cli/src/tests.rs`).

## 3. Specification: stable IDs

Everything in this section is **Proposed (needs maintainer confirmation)**. It is designed so that no valid v0.1.2 file changes meaning or build output.

### 3.1 Where the ID lives

- **Proposed:** add a new optional additive field (working name `uid`, to be confirmed) to every `Node` variant, declared `#[serde(default, skip_serializing_if = "Option::is_none")]`. It is **never rendered**: the compiler must ignore it (contrast `Node::Element.id`, which is rendered). It is not stored in `Node::Element.id` for the reasons in 2.2.
- `Page.id` is the stable ID for pages. It is already required and unique, so no new field is needed.
- Other keyed entities keep their existing identifier as the stable ID: styles (`IrStyleRule.id`), breakpoints (`Breakpoint.id`), forms and components (map key, with a new check that it equals `id`/`name`), tokens (map key), `data`/`metadata` (map key), JSON-LD (`JsonLdBlock.id`, optional).
- Unaddressable-by-ID items stay addressed as `(owner ID, position)` and are documented as unstable under reorder: `contrast_pairs`, `scripts`, `breakpoints` order, `seo` string lists, `FormFieldSchema` (could use `name`).
- Alternative considered and **not** recommended: a sidecar mapping (for example in `metadata`, from structural path to ID). It breaks as soon as a reorder is made outside the reducer, and cannot cover pack-generated nodes. Open question Q1.

Adding an optional field with `skip_serializing_if` leaves every legacy file's bytes unchanged on re-serialization with respect to the new field. Older (v0.1.2) binaries ignore the field on read (no `deny_unknown_fields`) and build identical output, but would drop it if they ever re-saved. See 3.8.

### 3.2 Grammar and namespaces

- **Proposed:** a generated or command-supplied stable ID matches `^[a-z0-9][a-z0-9._-]{0,63}$` (ASCII, lowercase, 1 to 64 characters). Existing `Page.id`, `IrStyleRule.id` and other legacy identifiers are grandfathered verbatim and are never rewritten or re-validated against this grammar.
- **Proposed:** node IDs are unique across the **whole document** (all `pages[*].body` and all `components[*].template`), not per page. This lets a command address a node without a `page_id` hint (the current `(page_id, node_id)` pair stays accepted as a redundant check). Pages, styles, breakpoints, forms, components are separate namespaces (a page and a node may share a string).

### 3.3 ID creation

1. **Existing nodes in an upgraded legacy document:** see 3.5 (deterministic upgrade).
2. **New nodes created by an editor command:** the command carries the new ID (client-supplied, so a recorded command batch replays and redoes to the same result). The reducer rejects an ID that is empty, fails the grammar, or already exists.
3. **Convenience generator (pure, deterministic):** `next_id(doc, prefix)` returns the smallest `"{prefix}-{n}"`, `n >= 1`, not present in the document. It depends only on document state, never on time or randomness (`std::time`, RNG and hashing crates are not needed; `sws-ir` has only `serde`/`serde_json` as dependencies and must keep that for `cargo deny`).
4. **Pack-generated content** (lowered from a manifest): IDs derive from manifest identifiers, see 3.7.

### 3.4 Collision handling and duplicate-ID rejection

| Situation | Behaviour (Proposed) |
| --- | --- |
| New `uid` field duplicated in a document | `validate_ir` error `duplicate_node_id` with both paths. Safe to make an error because no v0.1.2 file can contain the field. |
| Legacy `Node::Element.id` or `attrs["id"]` duplicated (**probed**: accepted today by `check` and `build`) | Must **stay buildable and checkable** (section 5). Not an error in `build`/`check`. At most a new `warning` (does not change `CheckReport.ok`, which depends only on `error_count` and `plugin_finding_count`). The **editor refuses** to open such a document for editing until the duplicates are resolved by the explicit upgrade (3.5), which does not touch rendered `id`s: it only assigns the new `uid`. |
| A command creates, moves-in or pastes a subtree containing an ID already in the document | The whole batch is rejected atomically with a typed error naming the ID and the colliding path. No auto-renaming inside a batch, so redo and replay stay deterministic. |
| Paste/duplicate of a subtree as a feature | The command must carry a complete old-ID to new-ID map; the reducer validates every new ID. |
| Upgrade collisions (two nodes derive the same candidate ID) | Resolved deterministically by document order, see 3.5 step 4. |
| Duplicate `Page.id` | Already an error (`validate_ir`); unchanged. |
| Duplicate `IrStyleRule.id` / `Breakpoint.id` / menu section `id` | Not an error today (2.1, 2.3). **Proposed:** a new `validate_ir` error for IR style/breakpoint IDs is a behaviour change to `check` for existing files; defer it to editing mode only (the editor refuses to open), and ask maintainers (Q4) before tightening `check`. |

### 3.5 Explicit, deterministic upgrade for legacy documents

The upgrade is an explicit operation, never implicit. **Proposed** name: `assign_stable_ids(&SiteDocument) -> (SiteDocument, UpgradeReport)` in `sws-ir`, surfaced as an explicit CLI/MCP/Wasm action (names in 6.1 are placeholders).

Determinism contract: same input document (after normal parse, so field order does not matter) yields the same IDs, byte for byte, on every platform (the crate is `wasm32`-clean and uses only `BTreeMap` and `Vec`).

Algorithm:

1. Traverse in document order: `pages` in array order; within a page, `body` depth-first pre-order; within a node, child containers in this fixed order: `children`; `fallback`; `slots` in `BTreeMap` key order (each slot's array in order); `then_nodes` then `else_nodes`; `body` for loops. Then `components` in `BTreeMap` key order, `template` in order.
2. Skip nodes that already have a valid unique `uid` (idempotence).
3. Candidate for a node: `{scope}.{label}` where `scope` is the owning `Page.id` (or `component:{name}` for templates) reduced to the grammar, and `label` is, in this order: a non-empty `attrs["id"]`, else `Element.id`, else the first class, else the tag name, else the variant name (`text`, `slot`, `instance`, `if`, `loop`).
4. Slugify the candidate (lowercase, runs of other characters become `-`, trim to 64 chars). If it is already taken, append `-2`, `-3`, and so on, in traversal order (the first in document order keeps the bare name).
5. Never read time, randomness, a counter stored in the document, or a hash dependency.

Properties: the upgrade only adds the new field. It does not change any existing field, does not reorder any array, and does not change compiled output (the compiler ignores `uid`). Running it twice yields the same document (idempotent). The `UpgradeReport` lists every assigned ID with its path so the operation is reviewable. Writing the result is a separate explicit save; the input file is written only when the caller explicitly targets it (otherwise a new path).

Example, using the fictional demo (lowered IR, `index` page): the `<section id="menu">` element becomes `index.menu`, and its heading child `index.menu-heading` would only be assigned if the heading is an `Element` with `attrs.id = "menu-heading"` (it is, in the lowered demo). Two later `main` elements on different pages become `events.main` and `book-a-call.main`, so the same HTML `id` string on different pages no longer collides in the ID space.

### 3.6 Stability after reorder and rename

- IDs are assigned once and **persisted**; they are never recomputed from position, tag, text or class after the upgrade. Therefore moving a node (changing its parent or index), reordering pages, or changing its tag, classes, text or `attrs["id"]` leaves its `uid` unchanged.
- Renaming a page changes `Page.id` (and, optionally, its route) only through an explicit rename command that rewrites every reference atomically. Page IDs are therefore "stable until explicitly renamed"; the audit finds no existing reference to a `Page.id` outside `patch.rs` and diagnostics messages. A reducer must still search `seo`, `data`, `metadata` for references it knows about (Q6).
- Node `uid` has no rename operation (it is opaque). Undo of a delete restores the original `uid`; the inverse command must carry the full removed subtree including all descendant `uid`s.
- Duplicate-ID-after-merge: collaborative or batch merge conflict handling is #55's concern (touched-field addressing). The audit's requirement is only that addressing is by `uid`, not by index.

### 3.7 Pack-generated nodes and manifest sources

**Proposed (Q2):** treat the two input kinds differently.

- `site_ir` documents are directly editable with stable IDs.
- A `venue_manifest` is the source of truth for pack content. Lowered IR is derived output with no inverse. The reducer must **not** save lowered IR back into `venue.json`, and must not present edits to lowered nodes as edits to the manifest. Edits to manifest-level content use pack-level addressing by manifest identifiers (`menu.sections[].id`, `checkout.items[].id`, map keys).
- Opening a venue for node-level editing is an explicit one-way "eject" to `site_ir` (this is what MCP `read_site_ir` does today, `tool_read_site_ir`). The ejected document keeps `metadata["venue_manifest"]` verbatim (preserved as supported extension data) but marks it stale in the upgrade report, because the pack lints it (`HospitalityPlugin::lint`) as if it were current.
- Pack-lowered nodes get deterministic IDs derived from manifest IDs when lowering gains ID support: for example menu section `coffee` gives `index.menu.coffee`, rather than a generic path-derived name. Not implementable without a change in `packs/hospitality/crate/src/compiler.rs`; that is out of scope here.
- Menu section IDs are not unique-checked today (2.3). Any reducer operating on the manifest must reject duplicates before use.

### 3.8 Compatibility and migration

- **Version marker. Proposed (Q3):** keep `schema_version: 1` because the change is additive; record "IDs assigned" with a namespaced marker `metadata["sws.ir"] = {"uid_scheme": 1}`. A bump to `schema_version: 2` is the alternative, but `validate_ir` rejects anything other than 1 (`validate.rs`), so v0.1.2 binaries would refuse every upgraded file even though they could build it. Without a bump, a v0.1.2 binary reads and builds an upgraded file with identical output (it ignores `uid`) but silently drops `uid` and the marker if it re-serializes (for example MCP `read_site_ir` / `apply_ir_patch` output). Newer tools must therefore treat a document whose marker is present but whose nodes lack `uid` as "damaged by an older writer" and refuse to edit it until re-upgraded.
- **Forward reads:** a v0.1.2 `solo build`/`check` of an upgraded file succeeds (probed behaviour for unknown fields: ignored).
- **Backward reads:** newer tools reading a legacy file with no marker and no `uid`s load it read-only (build, check, preview, export) and require the explicit upgrade before editing.

### 3.9 Unknown fields and extension data

Current behaviour (section 1.1): unknown fields are dropped silently; free-form `data`/`metadata` values are kept, with object keys sorted. **Proposed** policy for any save path that writes an IR or manifest back to disk:

1. **Supported extension data is preserved.** These containers are typed `BTreeMap<String, Value>`/`Value` and round trip: `SiteDocument.data`, `SiteDocument.metadata` (including the pack's `venue_manifest` copy), `JsonLdBlock.data`, `PropSpec.default`, `Node::ComponentInstance.props`. Caveat to document and test: key order inside these values is normalized to sorted order, and numbers go through `serde_json` (no `arbitrary_precision` feature), so integers outside `i64`/`u64` range and non-canonical number text (for example `1.0`, `1e3`) may be re-formatted. That is a representation change, not data loss, and it must be called out in the save report.
2. **Unsupported fields are never silently lost.** Before writing, the saver computes a lossless check: parse the original bytes to a `Value`, serialize the typed document to a `Value`, and verify that every key path present in the original exists in the re-serialized value with an equal value (modulo documented number normalization). Keys that appear only in the re-serialized value (defaults such as `"description": null`) are allowed. Any original path missing from the output (for example `x_ext`, `pages[0].future`, `body[0].unknown`) makes the save fail with a typed `lossy_document` error listing the paths. The refusal is the default; an explicit, separate "drop unsupported fields" confirmation is the only way through, and it must be recorded in the save report.
3. **Unknown enum tags and unknown node kinds** already fail parsing (1.1), so they cannot be silently dropped; they surface as `ir_parse_error` and the editor refuses to open the document.
4. **Unsupported nodes** (for example nodes inside `Conditional`, `Loop`, `ComponentInstance` slots) are preserved structurally because the typed model includes them. The reducer may decline to edit inside them, but must refuse (not flatten) a save that would lose them. Component templates are preserved even though no current patch edits them.
5. **Venue manifests:** the same rule applies to `VenueManifest`; `$schema` is preserved (it is a typed field). Because `VenueManifest` also ignores unknown keys, the same lossless check applies.

## 4. Specification summary table

| Concern | Rule | Status |
| --- | --- | --- |
| ID field | new optional `uid` on all `Node` variants, never rendered | Proposed |
| Page/style/breakpoint/form/component identity | existing identifiers, grandfathered | Existing, plus Proposed uniqueness checks in editing mode |
| Creation | command-supplied; deterministic `next_id` helper | Proposed |
| Collision | typed atomic rejection; new field duplicates are validation errors; legacy HTML-id duplicates stay legal for build/check | Proposed |
| Legacy upgrade | explicit, deterministic, idempotent, additive only | Proposed |
| Reorder/rename | IDs persisted, not derived from position/tag; page rename is an explicit command | Proposed |
| Compatibility | `schema_version` stays 1; marker in `metadata` | Proposed (Q3) |
| Extension data | preserve typed containers; refuse lossy save | Proposed |

## 5. Read-only guarantee: valid v0.1.2 files stay buildable with no input writes

### 5.1 How it is true today

- `cmd_build` (`crates/sws-cli/src/commands.rs`) reads the input with `fs::read_to_string(input)`, compiles in memory, and writes **only** through `write_compiled_files(outdir, ...)`, which creates `outdir` and the relative paths in the compiled file map (`safe_relative_path` rejects `..`, absolute paths and prefixes). With `--check` it instead calls `check_compiled_files`, which only reads and compares bytes. No code path in `cmd_build` writes to `input`.
- `cmd_check` reads the input, parses to `serde_json::Value`, runs `run_checks_on_value(&raw, ...)` and prints. It has no write call. The optional `--headers-file` is read only (`check_headers_parity`).
- `parse_value_to_ir(&Value, ...)`, `compile_from_json(&str, ...)`, `compile_site*` and `check_site_diagnostics` take shared references or clone; nothing serializes a document back to the input path. `apply_to_document` mutates only the in-memory copy.
- The MCP server (`crates/sws-cli/src/mcp.rs`) contains no `fs::write`/`create_dir`; its only filesystem access is `read_site_ir` reading a jailed path (`resolve_safe_read_path`), and `apply_ir_patch` returns the patched document as text, it does not save.
- `sws-wasm` exposes only string-in/string-out functions (`compile_site_inner` and others); it has no filesystem access.
- The only CLI commands that write are `init` (new directory), `export` (new bundle directory) and `build` (output directory). The `grep` for `fs::write`/`create_dir` in `crates/sws-cli/src` outside tests and templates lists exactly those (`cmd_init`, `cmd_export`, `write_compiled_files`).
- Adding the optional `uid` field (3.1) cannot break a legacy file: it is `serde(default)`, and the compiler ignores it. Compiled output depends only on fields that exist today.

Residual risks to name: (a) there is **no existing test** that asserts the input file is byte-for-byte (or mtime) unchanged after `build`/`check`; the guarantee is by code inspection, not by test (`grep mtime|modified|unchanged` over the CLI tests is empty); (b) `solo build input out` where `out` equals the directory containing the input, and the compiled file map contains a path equal to the input's name, would overwrite it; no guard exists (benign for typical `site.ir.json`/`venue.json` names, but not enforced); (c) `solo check` accepts arbitrary JSON as `partial_json` with `ok: true` when neither the venue nor IR branch applies (`run_checks_on_value`), so "valid" for check is looser than "valid IR".

### 5.2 What #55 must test to keep it true

1. For each fixture, hash the input bytes (and record mtime), run `solo build` (writing to a separate temp `outdir`), `solo build --check`, `solo check` and `solo check --json`, `solo export`, and the MCP `read_site_ir` / `compile_preview` / `run_a11y_and_plugin_checks`; assert the input bytes and mtime are unchanged and the directory listing of the input's directory is unchanged.
2. Property: for every legacy fixture (section 7), compiled output of the legacy file equals compiled output of its explicitly upgraded form (`BTreeMap<path, bytes>` equality), proving `uid` is never rendered.
3. Legacy build and check succeed with no `uid`, no marker, duplicate legacy HTML IDs and unknown fields present (the probe document from this audit is a ready negative/positive fixture shape).
4. The upgrade is only reachable through an explicit entry point: assert that `build`, `check`, `preview`, `export`, MCP `read_site_ir`, `apply_ir_patch` and `compile_preview` never emit `uid` fields for a legacy input.
5. Upgrade determinism: upgrading the same legacy input twice, in two fresh processes (pattern: `crates/sws-cli/tests/determinism.rs`), yields byte-identical documents; upgrading an upgraded document is a no-op.
6. A save that would drop `x_ext`-style unknown fields fails with `lossy_document` and leaves the target file unchanged (assert unchanged hash).
7. Reorder and rename: moving a section and renaming a page keep every `uid`, and legacy output is otherwise identical.

## 6. Affected interfaces and fixtures

### 6.1 CLI commands (`crates/sws-cli/src/main.rs`, `commands.rs`)

| Command | Reads IR/manifest | Writes IR | Affected by stable IDs |
| --- | --- | --- | --- |
| `solo init [--pack hospitality]` | no | **yes**: writes `site.ir.json` (serialized `starter_site_document`, `templates.rs`) or `venue.json` | New IR written by `init` would either include no `uid` (legacy-shaped) or be pre-upgraded; decision needed (Q5). It serializes with `null`s today (no `skip_serializing_if`). |
| `solo build <input> <outdir> [--check]` | yes | no (output dir only) | Must ignore `uid`; output unchanged. |
| `solo check <input> [--json] [--headers-file]` | yes | no | Duplicate-ID diagnostics must not turn legacy documents red (3.4). `CheckReport` shape is public JSON. |
| `solo export <input> <outdir>` | yes | no (bundle dir) | Same as build; `render_export_*` templates derive from `doc.slug` and `base_url` only. |
| `solo preview <outdir>` | no (serves output) | no | Not affected. |
| `solo mcp [--root]` | yes | no | See 6.2. |
| New (placeholders, names Proposed): explicit upgrade, apply-command-batch / save | | yes | Belong to #55; must follow section 5 and 3.9. |

### 6.2 MCP tools (`crates/sws-cli/src/mcp.rs`, `tool_definitions`)

| Tool | Input | Output | Affected |
| --- | --- | --- | --- |
| `read_site_ir` | `path` or `ir_json` (IR or venue) | pretty `SiteDocument` JSON (venue lowered, `metadata.venue_manifest` included) | Output today is a lossy normalization (drops unknown fields, adds `null`s). A save based on it is exactly the silent-loss path 3.9 forbids. |
| `apply_ir_patch` | `ir_json` plus `patch_json`/`patches` (array of `IrPatch`) | updated `SiteDocument` JSON | Direct predecessor of the reducer; node ops address `Element.id` only. #55 should keep it working (legacy patch ops continue to apply to legacy documents) or deprecate it explicitly. |
| `compile_preview` | `ir_json` | files, byte sizes, diagnostics | Must ignore `uid`. Uses `compile_options_with_pack_extras`, so venues include pack assets. |
| `run_a11y_and_plugin_checks` | `input_json` | `CheckReport` | Same as `check`. |

Tests: `mcp_stdio_round_trip_supports_all_methods_and_tools`, `mcp_read_site_ir_jails_paths_and_blocks_sensitive_files`, `mcp_rejects_oversized_stdin_line_and_recovers` in `crates/sws-cli/src/tests.rs`.

### 6.3 Wasm exports (`crates/sws-wasm/src/lib.rs`)

| Export | IR access | Affected |
| --- | --- | --- |
| `compile_site(ir_json)` | parses `SiteDocument` only (not venue manifests), `CompileOptions::default()`, no plugin registry and no pack extra files | Must ignore `uid`. Because it never runs `apply_to_document` or pack extras, its output differs from native for a venue-derived IR (section 8). |
| `validate_site(ir_json)` | `validate_ir` plus compiler diagnostics | Place for `duplicate_node_id` (new-field) validation. |
| `compile_css`, `validate_css`, `check_contrast` | CSS-only | Not affected. |

There is **no patch/apply export** in Wasm today; the browser cannot apply `IrPatch`. #55's Wasm path is new surface. Smoke test: `crates/sws-wasm/tests/smoke.mjs` (run by `crates/sws-wasm/run-smoke.sh`, CI job `wasm-smoke`).

### 6.4 Library interfaces

`sws_ir::{SiteDocument, Node, Page, IrPatch, apply_patch, IrError, validate_ir, site_document_json_schema}`; `sws_plugin::SwsPlugin` (`transform_input`, `transform_ir`, `components`, `form_schemas`; ABI v1 promises serde compatibility "fields only added behind `#[serde(default)]`", which the `uid` field satisfies; note `PluginRegistry::apply_to_document` mutates the in-memory document by merging contributed components and forms, so an editor must **not** save a document after `apply_to_document`); `sws_compiler::{compile_from_json, compile_site, compile_site_with_raw, check_site_diagnostics}`; `sws_pack_hospitality::{compiler::HospitalityPlugin, venue::VenueManifest, venue::validate_venue}`.

### 6.5 Fixtures and tests that read or write IR

| File | Role | Affected |
| --- | --- | --- |
| `packs/hospitality/demo/venue.json` | canonical fictional pack fixture (`shirlington-hearth-demo`) | primary legacy fixture; lowered to IR on every build |
| `packs/hospitality/schema/venue.schema.json` | pack JSON Schema | #56 |
| `packs/hospitality/tools/fixtures/abc-bad.json` | negative lint fixture (partial JSON) | `partial_json` path of `check` |
| `core/public/*` | rendered output of `npm run render` (`solo build venue.json core/public`) | must stay byte-identical (`npm run render:check` in `npm run verify`); never hand-edit |
| `crates/sws-cli/src/templates.rs` (`starter_site_document`, `starter_hospitality_venue`) | source of `solo init` fixtures | Q5 |
| `crates/sws-cli/src/tests.rs` | CLI/MCP/preview tests that build starter IR and the demo venue | all build/check/MCP tests |
| `crates/sws-cli/tests/determinism.rs` | two-process byte-identical build of a multi-page IR fixture (`fixture_json`) | pattern for upgrade-determinism test |
| `crates/sws-cli/tests/export_bundle.rs` | `solo export` of a starter IR | export path |
| `crates/sws-ir/src/lib.rs` (tests) | `sample_doc` IR fixture; `apply_all_patch_operations`; `serde_round_trip_and_deterministic_json`; `defaults_populate_when_optional_fields_omitted`; `validation_catches_all_invalid_conditions`; `json_schema_is_draft_2020_12_and_deterministic` | patch/ID/schema tests extend here |
| `crates/sws-compiler/src/lib.rs`, `diagnostics.rs`, `expand.rs` (tests) | in-code IR fixtures | `uid` must not change rendering |
| `crates/sws-plugin/src/lib.rs` (tests) | dummy plugins mutating IR | ABI serde compatibility |
| `packs/hospitality/crate/src/compiler.rs` (tests) | lowering tests (for example `metadata.contains_key("venue_manifest")`) | lowering ID work |
| `crates/sws-wasm/src/lib.rs` (tests) and `tests/smoke.mjs` | inline IR fixtures for the Wasm exports | Wasm contract |
| `.github/workflows/ci.yml` | `render:check`, `wasm-smoke`, export job | CI gates that consume IR |

Gate scoping per `AGENTS.md` section 9: `npm run verify` covers `render:check`, Rust tests, clippy and `cargo deny`; the Wasm smoke and server-dependent gates are CI-only.

## 7. Open facts to carry forward (not decisions)

- IR `Option`s serialize as `null`; the pack skips them. A "save" for each kind must pick and document one canonical form, otherwise the first save churns a legacy file's bytes even without semantic change (Q7).
- `CompileOptions` differs by entry point: CLI uses `include_headers_file: true` plus pack extras; Wasm uses `CompileOptions::default()` (also `include_headers_file: true`) and no extras.

## 8. Hand-off to #55 (reducer)

### Requirements

1. **Addressing.** Commands address nodes by stable ID (`uid`, or `Page.id`/map key/style ID for other entities), never by array index alone. Indices appear only as insertion positions (`index` within a known parent). The reducer must support nodes of all six `Node` variants, not only `Element`. See 3.1 to 3.2.
2. **New operations the current `IrPatch` lacks:** move node (parent and index), reorder page, rename page (atomic reference update), delete token/style/data, edit component templates, set an element's text without destroying nested children (the current `set_node_text` replaces all children, 2.5). Existing `IrPatch` ops stay accepted for legacy documents.
3. **Atomic apply.** A command batch either fully applies or leaves the document untouched, with a typed error naming the failing command index. (Today `apply_patch` mutates in place and only the MCP tool's discard-on-error provides atomicity; the reducer must own this, for example by validating on a working copy and swapping.)
4. **Validate after apply.** Run `validate_ir` plus the new uniqueness rules (3.4) before committing a batch, and reject duplicates of the new field. Do not make `validate_ir` stricter for legacy fields (5.2 item 3).
5. **Inverse commands.** Each command produces a deterministic inverse capturing exactly what it destroyed: removed subtrees with all `uid`s, previous token values, previous attrs, previous index. `remove_node` and `set_node_text` currently return nothing, so the reducer must read the before-state itself.
6. **Touched-field set.** Every command reports the set of addressed fields (for example `node:{uid}.attrs.class`, `tokens.colors.accent`, `pages` order) for conflict addressing; paths are by ID, not index.
7. **Preserve author order and extensions.** Never sort or dedupe arrays (`pages`, `styles`, `breakpoints`, `contrast_pairs`, `children`, `seo.*`). Apply 3.9: preserve `data`, `metadata`, `JsonLd` data, props; refuse a lossy save with `lossy_document` and leave the file unchanged.
8. **Explicit upgrade only.** Implement `assign_stable_ids` per 3.5 as a separate operation with a report; no implicit upgrade on load, build, check, preview, export or MCP calls (5.2 items 3 and 4).
9. **Read-only guarantee tests.** Implement 5.2 items 1 to 7 as automated tests in `crates/sws-cli` and `crates/sws-ir`.
10. **Pack handling.** Do not save lowered IR over `venue.json`. Decide whether #55 supports editing `venue.json` (pack-level commands keyed by manifest IDs) or only ejected IR (3.7, Q2). For the acceptance scenario "load a legacy demo, change a token, move a section, undo and redo both, save and reopen", the practical path is the ejected IR of `shirlington-hearth-demo`; moving a section means moving a `section` element under `main` on the `index` page, addressed by its assigned `uid`.
11. **Native vs Wasm byte comparison.** To compare outputs for the same saved IR: compile the saved IR with identical options. Native `solo build` on a saved IR and Wasm `compile_site` both go through `CompileOptions` with `include_headers_file: true`, but the native path additionally runs `registry.apply_to_document` (a no-op for the hospitality pack, which contributes no components or forms; verify when reusing for other packs) and Wasm does not run plugins. The pack asset files are absent from both for a saved IR (**probed**, 1.4), so the comparison is of the same file map. Do not compare against `solo build venue.json`, which has five extra files. Wasm needs a new export for the reducer; today none applies patches (6.3). Reuse `apply_patch` internals where practical, per the issue text.
12. **Property tests to add:** undo-then-redo identity; apply-then-inverse identity on the document bytes; batch atomicity (error at command k leaves bytes unchanged); `uid` uniqueness after any batch; ID stability after move; upgrade idempotence and determinism; order preservation of arrays.
13. **Keep the public surface additive.** No change to `schema_version`, to the 11 existing `IrPatch` op names or to `CheckReport`/MCP tool names without a version note (3.8).

### Open questions for #55

- Q1: Is the new field on the node (`uid`) acceptable to the maintainers, versus a sidecar? (3.1)
- Q2: Does the Studio edit `venue.json` directly (pack-level commands) or only ejected IR? Is eject one-way? (3.7)
- Q4: May `solo check` gain duplicate-ID diagnostics for legacy fields at warning level, or must `check` output for legacy files remain exactly as is? (3.4)
- Q5: Should `solo init` write new documents already upgraded (with `uid` and marker), so new projects are editable immediately? (6.1)
- Q6: Which references to a `Page.id` must a rename rewrite (`seo`, `data`, `metadata`, nav links in `attrs.href`)? Only IDs found by a typed traversal, or also free-form values? (3.6)
- Q7: Canonical save form: keep `null`s for IR optionals (matches current serialization) or add `skip_serializing_if`? The latter changes every re-serialized legacy file once. (7)
- Q8: Node IDs unique per document (proposed) or per page (current patch scoping)? (3.2)
- Q9: Coalescing and batch versioning (the issue says "versioned command batch"): what is the version field's relationship to `schema_version`?

## 9. Hand-off to #56 (schema/codegen)

### Requirements

1. **Source of truth is the Rust types, not `schema.rs`.** The existing `site_document_json_schema()` is a hand-written `json!` (1.1) with no test linking it to the serde shape and with `additionalProperties: false` that the parser does not honor. #56 must replace it with a generated schema (or generate both and add a conformance test) and add a drift gate to `npm run verify` as the issue requires. Decide what happens to the existing `site_document_json_schema` public function (keep as a thin wrapper over the generated schema, to preserve the API).
2. **Generated schema must describe the serde reality:**
   - Internally tagged unions: `Node` (tag `type`, six variants), `RoutePattern` (tag `kind`), and the `IrPatch` command type (tag `op`) with `snake_case` tags.
   - Defaults: fields with `#[serde(default)]` are not `required`; `schema_version` default 1; `locale` default `en-US`; `FormSchema.method` default `POST`.
   - Optionals serialize as `null` in the IR (no `skip_serializing_if`): generated types should be `T | null` and optional on input. For the pack, skipped optionals are absent, not `null` (except the few without `skip_serializing_if`).
   - `BTreeMap<String,_>` as `additionalProperties`/`Record<string, T>`; the free-form `Value` fields as unconstrained JSON (`unknown` in TypeScript).
   - `PropKind` and `FormFieldKind` as snake_case string enums.
3. **Unknown-field policy must be explicit in the schema.** Today's schema says `additionalProperties: false` and the parser ignores unknown keys. Decide and encode one policy (3.9 recommends: strict schema for authoring tools, tolerant parser for build/check, lossy-save refusal in the saver). A schema that rejects what `solo build` accepts would contradict the read-only/buildable guarantee; make the difference deliberate and documented.
4. **ID-bearing types.** Include the proposed `uid` field and the ID grammar `^[a-z0-9][a-z0-9._-]{0,63}$` (3.2) as a `pattern`, but only after #55 and the maintainers confirm names (Q1). Until then, generate from the current types so no public format changes. JSON Schema cannot express document-wide uniqueness; the generated artefacts need a documented "also validate with `validate_ir`" note.
5. **Command types.** The reducer's command batch types (#55) are part of the contract; coordinate so the generator covers `SiteDocument`, the command batch, the upgrade report and the typed errors in one run.
6. **Reproducible command and no JS runtime.** Generation runs in Rust (no `node` at generation time, per the issue). Output into `packages/@solo-website-system/ir-types` (new package directory; `ARCHITECTURE.md` currently lists `packages/` as planned for v0.3.0, so this creates it). The generated `.d.ts` and Draft 2020-12 JSON are committed and `npm run verify` fails on drift (same pattern as `npm run render:check`, `sites:check`).
7. **License hygiene.** Any schema/TS generator crate (candidates must be evaluated; none is currently a dependency, and `sws-ir` depends only on `serde` and `serde_json`) must pass `cargo deny check` against the allowlist in `AGENTS.md` section 2 (MIT, Apache-2.0, BSD-2/3, ISC, Unicode-DFS-2016, CC0-1.0). Do not broaden `deny.toml`; ask first if a `Unicode-3.0` or other exception would be needed. Prefer keeping the generator a dev/build tool behind a feature or separate crate so it is not compiled into `sws-wasm` or the Worker (size limits, `AGENTS.md` section 3).
8. **Wasm size.** Derive macros that add runtime code to `sws-ir` affect `sws-wasm`; prefer a feature-gated derive or a separate generator crate that depends on `sws-ir`.
9. **Pack schema.** Decide whether `packs/hospitality/schema/venue.schema.json` (hand-written, strict, validated nowhere in Rust) is generated from `VenueManifest` too. If so, note that `VenueManifest`'s `skip_serializing_if` conventions differ from the IR's, and its schema carries constraints (patterns, `oneOf` for hours, conditional `scheduling.events` rules) that derive-based generation will not reproduce without annotations; scope accordingly or exclude the pack from #56.
10. **Round-trip conformance tests.** For each fixture in 6.5, validate the serialized `SiteDocument` against the generated schema, and validate the generated schema against the serde parser with a set of unknown-field and bad-tag mutations.

### Open questions for #56

- Q1/Q3 (shared with #55): final name of the ID field and whether the marker lives in `metadata`.
- Q10: Which Draft 2020-12 `$id` is canonical? The existing one is `https://solo-website-system.example/schemas/site-document.v1.json` (`schema.rs`); keep it or version it?
- Q11: Should TypeScript types model the in-memory `null` optionals or an input type with optional keys plus an output type with `null`?
- Q12: Is the pack manifest in scope (item 9)?

## 10. Consolidated list of "Proposed (needs maintainer confirmation)" items

1. New optional additive node field (`uid`), never rendered; no reuse of `Node::Element.id` (3.1, Q1, Q8).
2. ID grammar `^[a-z0-9][a-z0-9._-]{0,63}$` and document-wide node ID uniqueness; legacy identifiers grandfathered (3.2).
3. Command-supplied IDs plus pure `next_id(doc, prefix)` helper (3.3).
4. Collision policy: atomic typed rejection; new-field duplicates are validation errors; legacy HTML-ID duplicates stay legal for `build`/`check` and block only editing until upgrade (3.4, Q4).
5. Explicit, deterministic, idempotent, additive upgrade `assign_stable_ids` with the traversal and naming algorithm in 3.5.
6. IDs persisted after upgrade, not recomputed; explicit page rename command that rewrites references (3.6, Q6).
7. Treat `venue.json` as the source of truth for the pack; lowered IR is derived and never saved back; eject is one-way; manifest-derived IDs for lowered nodes (3.7, Q2).
8. `schema_version` stays 1; marker `metadata["sws.ir"] = {"uid_scheme": 1}`; refuse to edit a marked document whose nodes lack `uid` (3.8, Q3).
9. Lossless-save policy: preserve typed extension containers, document number/key-order normalization, refuse a lossy save with `lossy_document`, explicit separate confirmation to drop fields (3.9).
10. Editing-mode-only duplicate checks for style/breakpoint/menu-section IDs (3.4).
11. `solo init` output form and canonical null-vs-absent save form (6.1, Q5, Q7).
12. All names in section 6.1 (new CLI/MCP/Wasm actions) are placeholders.
13. Generated-schema unknown-field policy and the pack-schema scope in #56 (9, Q10 to Q12).
