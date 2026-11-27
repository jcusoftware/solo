#!/usr/bin/env bash
# Editing gate (issue #55): build the native `solo` CLI and the sws-wasm
# bindings, then run tests/editing-gate.mjs, which edits the legacy demo IR
# (token change, section move, undo/redo, save, reopen) natively and in Wasm and
# compares the byte output of both compile paths.
#
# Requires the wasm-bindgen CLI on PATH; its version MUST match the
# `wasm-bindgen` crate version in Cargo.lock (same rule as run-smoke.sh).
# Not part of `npm run verify` (verify needs only cargo + node); the pure-Rust
# parts of the gate are ordinary `cargo test`s in sws-ir and sws-cli.
set -euo pipefail
cd "$(dirname "$0")"
repo_root="$(cd ../.. && pwd)"

target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
wasm_path="$target_dir/wasm32-unknown-unknown/debug/sws_wasm.wasm"
pkg_dir="$target_dir/editing-gate-wasm"

cargo build -p sws-cli --features pack-hospitality --locked
cargo build -p sws-wasm --target wasm32-unknown-unknown --locked

crate_version="$(awk -v pkg='name = "wasm-bindgen"' 'index($0, pkg) == 1 { getline; match($0, /[0-9.]+/); print substr($0, RSTART, RLENGTH); exit }' "$repo_root/Cargo.lock")"
cli_version="$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')" || true
if [[ -n "$crate_version" && -n "$cli_version" && "$crate_version" != "$cli_version" ]]; then
  echo "wasm-bindgen CLI version ($cli_version) does not match the wasm-bindgen crate ($crate_version); install wasm-bindgen-cli@$crate_version" >&2
  exit 1
fi

rm -rf "$pkg_dir"
wasm-bindgen --out-dir "$pkg_dir" --target nodejs "$wasm_path"
# The workspace package.json sets "type": "module"; keep the CommonJS entry as .cjs.
mv "$pkg_dir/sws_wasm.js" "$pkg_dir/sws_wasm.cjs"

SOLO_BIN="$target_dir/debug/solo" SWS_WASM_PKG="$pkg_dir" node tests/editing-gate.mjs
