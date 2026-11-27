# Editor commands, undo/redo and stable node IDs

`MIT OR Apache-2.0`

The canonical, pure Rust editor reducer lives in `sws_ir::edit` (`crates/sws-ir/src/edit/`). The CLI (`solo edit`), the Wasm bridge (`apply_command_batch`, `upgrade_stable_ids`), and later MCP and Studio all call the same code. Nothing in `sws_ir::edit` performs file or network I/O.

Scope: **site IR only** ("ejected IR"). A hospitality `venue.json` is lowered one way with `solo edit eject`; the lowered IR is never written back over a manifest (enforced: `eject` refuses an `--out` that is the same file as the input, by path, symlink or hard link, and every `solo edit` write refuses a target that already holds a pack manifest), and there are no pack-level commands.

## Stable node IDs (`uid`)

- Every `Node` variant has an optional `uid` field (`skip_serializing_if = "Option::is_none"`). The compiler never renders it; compiled output is byte-identical with and without it. It is not `Node::Element.id` (a rendered HTML attribute).
- Grammar: `^[a-z0-9][a-z0-9._-]{0,63}$`. Unique across the whole document (page bodies and component templates). Opaque: never recomputed from position, tag or text, so moving or editing a node keeps its `uid`.
- `schema_version` stays `1`. The explicit upgrade writes `metadata["sws.ir"] = {"uid_scheme": 1}`.

### Explicit upgrade (`assign_stable_ids`)

`solo edit upgrade <in> --out <out> [--report <file>]`, Wasm `upgrade_stable_ids(ir_json)`, library `assign_stable_ids(&doc)`. Never implicit: `build`, `check`, `export`, `preview` and the MCP tools never add `uid` and never write their input.

- Traversal order: pages in array order; depth-first pre-order; per node `children`, `fallback`, `slots.*` (key order), `then_nodes`, `else_nodes`, `body`; then components in key order, `template` in order.
- Candidate id: `{scope}.{label}`. `scope` is the slugified `Page.id` (`component-{name}` for templates, scope capped at 30 characters). `label` is the slug of `attrs["id"]`, else `Element.id`, else first class, else tag, else the variant name (`text`, `slot`, `instance`, `if`, `loop`). If taken, `-2`, `-3`, ... in traversal order (the first keeps the bare name). Result is truncated to 64 characters.
- Nodes that already hold a valid, unique `uid` are kept. Later duplicates and grammar-invalid uids are reassigned (reported with reason `duplicate_uid` / `invalid_uid`).
- Idempotent and deterministic; changes only `uid` fields and `metadata["sws.ir"]`. The marker is set *inside* an existing `sws.ir` object (other keys of it, and every other `metadata` key, are kept); a marker that is not an object, or whose `uid_scheme` is not `1`, is refused (`unsupported_uid_scheme`). `UpgradeReport` lists `assigned` (`uid`, `path`, `reason`), `kept`, `marker_written`.
- The core knows nothing about pack metadata. `solo edit upgrade` and `solo edit eject` print which `metadata` keys were carried over unchanged, because copies of the document kept elsewhere (for example a pack manifest copy) go stale once the IR is edited.
- Compatibility edge: a legacy file whose node carries a non-string `"uid"` (previously an ignored unknown key) now fails to parse (`invalid_json`), because `uid` is a typed field. Remove or fix that key before upgrading.

### Editability gate (`check_editable`)

Editing is refused (fail closed) unless all hold:

1. marker present with `uid_scheme == 1` (`not_upgraded` / `unsupported_uid_scheme`);
2. every node has a `uid` (`missing_uid`: a marked document damaged by an older writer must be re-upgraded), each valid (`invalid_uid`), all unique (`duplicate_uid`);
3. style rule ids and breakpoint ids are unique (`duplicate_id`; editing mode only, `check` and `build` are unchanged);
4. `validate_ir` is clean (`invalid_document`);
5. the canonical JSON nests at most `MAX_EDIT_JSON_DEPTH` (127) containers deep (`invalid_document`). `serde_json` cannot parse input nested deeper than 127 containers, and one node level costs 2 (3 for component `slots`), so documents of about 62 node levels fit even though `validate_ir` allows 128. `validate_ir` itself, `build` and `check` are unchanged.

The same checks run after every batch, so a batch can never produce a document the editor would refuse. `apply_batch_json` additionally parses its own output again and refuses (rather than returns) text that would not re-parse or round-trip.

## Command batch

```json
{ "version": 1, "label": "optional", "coalesce_key": "optional", "commands": [ { "op": "...", ... } ] }
```

- `version` is the batch format version (`COMMAND_BATCH_VERSION = 1`), independent of `schema_version`. Unknown versions fail with `unsupported_version`.
- Parsing is strict at every depth: unknown fields in the batch, in a command **and inside embedded `node`, `page` and style `rule` payloads** are rejected (`invalid_json`, `paths` lists them, for example `$.commands[0].node.chidren`). So are unknown keys holding `null`, duplicate object keys and numbers that would lose precision (integers beyond `i64`/`u64`, over-precise floats). Known optional fields may be explicit `null`. 1 to 1000 commands (`empty_batch`, `batch_too_large`).
- Apply is atomic: all commands run on a working copy in order; on any error the input is untouched and the error carries `command_index`. The document is re-checked (gate above) before it is returned.

### Container reference

`{"kind":"page_body","page_id":"index"}`, `{"kind":"component_template","name":"card"}`, or `{"kind":"node","uid":"index.main","slot":"children"}`. `slot` defaults to `children` (element), `fallback` (slot_ref), `body` (loop); conditionals need `then_nodes` or `else_nodes`; component instances need `slots.<name>`.

### Commands

| `op` | Fields | Inverse |
| --- | --- | --- |
| `set_site_meta` | `name?`, `locale?`, `base_url?` | `set_site_meta` with previous values |
| `set_color_token` / `remove_color_token` | `key`, `hex` / `key` | `set_color_token` or `remove_color_token` |
| `set_font_token` / `clear_font_token` | `slot` (`body`, `heading`, `mono`), `stack` / `slot` (`heading`, `mono`) | `set_font_token` or `clear_font_token` |
| `set_data_value` / `remove_data_value` | `key`, `value` / `key` | `set_data_value` or `remove_data_value` |
| `upsert_style_rule` | `rule` (replace in place, else append) | `upsert_style_rule` (previous) or `remove_style_rule` |
| `insert_style_rule` / `remove_style_rule` | `rule`, `index?` / `id` | `remove_style_rule` / `insert_style_rule` with index |
| `insert_page` / `upsert_page` / `remove_page` | `page`, `index?` / `page` / `id` | `remove_page`, `upsert_page` or `remove_page`, `insert_page` with index |
| `move_page` | `id`, `index` (final position) | `move_page` to the old index |
| `insert_node` | `container`, `index?`, `node` (full subtree, every node has a unique `uid`) | `remove_node` |
| `remove_node` | `uid` | `insert_node` carrying the whole removed subtree and its old place |
| `move_node` | `uid`, `container`, `index?` (index after detaching; none appends) | `move_node` back to the old container and index |
| `set_attr` | `uid`, `attr`, `value?` (none removes) | `set_attr` (previous) |
| `set_text` | `uid` of a `text` node, `value` | `set_text` (previous) |
| `set_tag` / `set_classes` / `set_style_refs` | `uid` of an element, value | same op with previous value |

Only `set_site_meta`, `set_color_token`, `set_font_token`, `set_data_value` and `upsert_style_rule` go through `apply_patch` (for its validation); page, node, attr, text, tag, class, style-ref, `clear_font_token`, `remove_*` and `insert_*` commands are implemented in the reducer. Legacy `IrPatch` ops and the MCP tools behave as before on documents **without** the `sws.ir` marker (moving MCP onto the reducer is issue #68). On a **marked** document the legacy node-level ops that add or destroy nodes (`set_node_text`, `insert_child_node`, `remove_node`, and `upsert_page` with nodes that lack a `uid`, or whose `uid`s are invalid or collide with another node in the document) are refused, leaving the document unchanged, with an error telling the caller to use an edit command batch, because they would drop child `uid`s or add nodes without one and leave a document the editor refuses (`missing_uid`); `set_node_attr`, `set_site_meta`, tokens, data, styles, `remove_page` and `upsert_page` with fully identified nodes keep working. A page-rename command is intentionally not provided.

`set_tag` accepts the compiler's tag alphabet (ASCII letters, digits, `-`, `_`, starting with a letter), so SVG tags such as `clipPath` work.

`next_id(&doc, prefix)` returns the smallest `"{prefix}-{n}"` (`n >= 1`) that is not used as a node `uid`, page id, style rule id, breakpoint id or component name. Pure and deterministic; use it to mint `uid`s for `insert_node`.

Array order is author order: nothing is sorted or de-duplicated; inserts and moves land exactly at the requested index.

### Errors

`CommandError { code, message, command_index?, paths[] }`, `Display` is `code: message (command k) [paths]`. Codes: `invalid_json`, `unsupported_version`, `empty_batch`, `batch_too_large`, `not_upgraded`, `unsupported_uid_scheme`, `missing_uid`, `invalid_uid`, `duplicate_uid`, `duplicate_id`, `invalid_document`, `not_found`, `already_exists`, `invalid_value`, `invalid_index`, `invalid_container`, `move_into_self`, `wrong_node_kind`, `lossy_document`, `history_corrupt`.

## Transactions, undo, redo, coalescing

`apply_batch(&doc, &batch)` returns the new document and a `Transaction { version, label, coalesce_key, forward, inverse, touched }`. `inverse` is the exact undo batch body, already in application order (each command's inverse, reversed), built from state read before each command, so replaying it restores the previous document byte for byte. Transactions serialize to JSON and can be replayed.

Inverses can hold values the forward grammar would not accept as *new* input (a legacy empty `heading`/`mono` font stack, a tag outside the tag alphabet, ...). Forward commands stay strictly validated; recorded inverses are replayed in restore mode, which skips only those new-input grammar checks and still runs the structural checks, uid and container resolution, batch caps and the whole-document gate. `apply_batch` verifies every transaction before returning it by replaying the inverse in restore mode and requiring the original document back; a batch that could not be undone exactly is refused (`invalid_value`).

Undo entry point: because plain `apply_batch` / `apply_command_batch` of `transaction.inverse` is strict, it fails for such values (`invalid_value`). Undo a recorded transaction with `apply_undo(&doc, &tx)` (`sws_ir::edit`), `apply_undo_json(ir_json, transaction_json, options)`, the Wasm export `apply_undo(ir_json, transaction_json)` (same `{ document, transaction, dropped_paths }` output, `transaction` being the undo step whose `inverse` redoes it) or `solo edit undo <ir> <transaction.json> --out <ir> [--undo-transaction <file>] [--allow-lossy]`. Restore mode is not a general bypass: the input must be a whole transaction, and `apply_undo` replays `inverse` in restore mode, then replays `forward` strictly on the result and requires the original document and the same recorded inverse back, so a batch that is not the recorded inverse of an edit of that exact document is refused (`invalid_value`). What it can write is limited to a state the forward commands would turn into the current document (for example a legacy empty font stack that `set_font_token` then overwrote). Bare restore-mode batches stay library-only (`apply_batch_restore`, `History`).

`History` wraps a document with an undo stack (default cap 200) and a redo stack. `apply` clears redo; `undo` applies `inverse`; `redo` applies `forward`. A failing step changes nothing (`history_corrupt` if a recorded transaction no longer applies).

Coalescing: consecutive batches with the same non-empty `coalesce_key` merge into one undo step, unless an undo or redo happened in between. If every command of both is an idempotent overwrite (`set_*`, `upsert_style_rule`) and both touch exactly the same fields, the step compacts to the later forward commands with the earlier inverse; otherwise forward commands concatenate and inverses chain in reverse. A merge that would push either list past `MAX_BATCH_COMMANDS` (1000) is not done: a new undo step starts instead, so every step can always be replayed by undo and redo.

## Touched fields (conflict addressing)

Sorted, de-duplicated, `/`-separated, addressed by ID rather than index:

```
site/name | site/locale | site/base_url
tokens/colors/<key> | tokens/fonts/<slot> | data/<key>
style:<id> | styles                      (rule / rule order)
page:<id> | pages                        (page / page order)
page:<id>/body | component:<name>/template | node:<uid>/<slot>   (child-list order)
node:<uid>                               (existence / placement; also one per subtree uid on insert/remove)
node:<uid>/attrs/<name> | /text | /tag | /classes | /style_refs
```

`paths_conflict(a, b)` is true when equal or one is a `/`-ancestor of the other (conservative: a move of `node:X` conflicts with an attribute edit of `node:X`). `touched_conflict(&a, &b)` applies it to sets.

## Lossless save

Parsing drops unknown fields (the typed model has no `deny_unknown_fields`). Before editing or upgrading, `check_lossless(original_json, &doc)` re-serializes the typed document and requires every original path to be present and equal, and a text scan (`source_issues`) reports what both parses agree on silently: duplicate object keys (`$.data.k (duplicate key)`), integers outside `i64`/`u64` and floats with more precision than an `f64` keeps (`... (number N loses precision)`). Otherwise the call fails with `lossy_document` listing the paths (for example `$.x_ext`, `$.pages[0].future`) and nothing is written. Allowed differences: defaults added in the output (`null` optionals), an original `null` of a *known* optional field that is absent in the output (an unknown key holding `null` is dropped and therefore reported), number spelling (`1.0` vs `1`, `1e3`, `0.10`), object key order (maps are sorted). `data`, `metadata`, JSON-LD data and component props are preserved as typed extension containers. `--allow-lossy` (CLI) / `EditOptions { allow_lossy: true }` is the explicit, separate confirmation; the dropped paths are then printed and returned as `dropped_paths`.

## Interfaces

CLI (`solo edit`, file I/O only here; every command writes only its explicit `--out`/`--report`/`--transaction` paths). Everything is computed and serialized before any write; each file is written to a temp file in its own directory and renamed over the target, sidecars (`--transaction`, `--report`) first and `--out` last, so a failure never leaves a new `--out` without its sidecar. The only remaining window is a `rename` failing between files, which leaves the earlier files replaced and the later ones untouched (never half-written):

- `solo edit eject <manifest> --out <ir.json>`: one-way lowering of a pack manifest to site IR (fails for IR that is already site IR). The IR is saved *before* pack plugins merge their components and form schemas into it, so contributions are not baked into the file (`build` and `check` merge them again). It refuses an `--out` that is the manifest itself or any file holding a pack manifest.
- `solo edit upgrade <ir.json> --out <out.json> [--report <file>] [--allow-lossy]`
- `solo edit apply <ir.json> <batch.json> --out <out.json> [--transaction <file>] [--allow-lossy]`
- `solo edit undo <ir.json> <transaction.json> --out <out.json> [--undo-transaction <file>] [--allow-lossy]`: restore-mode undo of a transaction written by `apply --transaction`; `<ir.json>` must be the document that edit produced.

Wasm (`crates/sws-wasm`): `upgrade_stable_ids(ir_json)` returns `{ document, report, dropped_paths }`; `apply_command_batch(ir_json, batch_json)` returns `{ document, transaction, dropped_paths }`; `apply_undo(ir_json, transaction_json)` undoes a recorded transaction in restore mode with the same output shape. `document` is the canonical JSON text (pretty, trailing newline), identical to what the CLI writes. Errors are strings `"<code>: <message> ..."`.

Library: `sws_ir::edit::{apply_batch, apply_batch_restore, apply_undo, apply_batch_in_place, apply_batch_json, apply_undo_json, upgrade_json, assign_stable_ids, check_editable, check_lossless, next_id, History, canonical_json, ...}`.

## Editing gate

Before any Studio integration, one scenario proves the reducer end to end: load the legacy fictional demo (lowered IR of `packs/hospitality/demo/venue.json`), upgrade it, change a token, move a section, undo and redo both, save and reopen, then compare production output of the same saved IR.

- Pure Rust (runs in `npm run verify` via `cargo test`): `crates/sws-cli/tests/editing.rs::editing_gate_pure_rust_edit_undo_redo_save_reopen_compile`, plus the property tests in `crates/sws-ir/src/edit/tests.rs`.
- Native vs Wasm byte comparison: `npm run gate:editing` (`crates/sws-wasm/run-editing-gate.sh` then `crates/sws-wasm/tests/editing-gate.mjs`). It needs the `wasm-bindgen` CLI whose version equals the `wasm-bindgen` crate in `Cargo.lock` (`cargo install wasm-bindgen-cli --version <lock version> --locked`) and Node 22.13 or newer. The same edits run natively (`solo edit`) and through Wasm and must produce identical documents and transactions; the saved IR is then compiled by `solo build` and Wasm `compile_site` and every file must match byte for byte. It compares the same file map for a saved IR as raw bytes (not against `solo build venue.json`, which also emits pack assets), and replays redo through Wasm as well as undo. Native `solo build` merges registered plugin contributions into a site IR while Wasm `compile_site` does not; the hospitality pack contributes none today, which is what keeps the two equal. Not part of `npm run verify`; CI runs it in the `wasm-smoke` job.
