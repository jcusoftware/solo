#!/usr/bin/env bash
# Build sws-wasm for wasm32-unknown-unknown, bind it with wasm-bindgen
# (--target nodejs), and execute the export smoke test with Node.
#
# Requires the wasm-bindgen CLI on PATH; its version MUST match the
# `wasm-bindgen` crate version in the workspace Cargo.lock (see the
# `wasm-smoke` CI job, which pins it via taiki-e/install-action).
# Not part of `npm run verify` — the regular contributor chain never
# builds or executes wasm-bindgen output.
set -euo pipefail
cd "$(dirname "$0")"

target_dir="${CARGO_TARGET_DIR:-$PWD/../../target}"
wasm_path="$target_dir/wasm32-unknown-unknown/debug/sws_wasm.wasm"

cargo build -p sws-wasm --target wasm32-unknown-unknown --locked

crate_version="$(awk -v pkg='name = "wasm-bindgen"' 'index($0, pkg) == 1 { getline; match($0, /[0-9.]+/); print substr($0, RSTART, RLENGTH); exit }' "$PWD/../../Cargo.lock")"
cli_version="$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')" || true
if [[ -n "$crate_version" && -n "$cli_version" && "$crate_version" != "$cli_version" ]]; then
  echo "wasm-bindgen CLI version ($cli_version) does not match the wasm-bindgen crate ($crate_version); install wasm-bindgen-cli@$crate_version" >&2
  exit 1
fi

rm -rf pkg
wasm-bindgen --out-dir pkg --target nodejs "$wasm_path"

# The workspace root package.json sets "type": "module", so the generated
# CommonJS entry must carry a .cjs extension; the ESM snippet the `worker`
# crate (via sws-security's wasm32 dependency) ships is then required at
# module scope through Node's require(esm) support (Node >= 20.19).
mv pkg/sws_wasm.js pkg/sws_wasm.cjs

node tests/smoke.mjs
