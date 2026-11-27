# Wasm toolchain: pins, builds and target features

The Worker (`crates/sws-worker-site`) is built with `worker-build`, which drives
Cargo, the `wasm-bindgen` CLI and Binaryen's `wasm-opt`. This page records what is
pinned, where, and how to bump it. The Gate 1 fallback criteria (AGENTS.md section 5,
`docs/ARCHITECTURE.md` section 6) are unchanged by anything here.

## What is pinned

| Tool | Pin | Source of truth | How it is obtained |
| --- | --- | --- | --- |
| Rust | `1.100.0` (+ `clippy`, `wasm32-unknown-unknown`) | `rust-toolchain.toml` | rustup reads the file; CI and release use `dtolnay/rust-toolchain@master` with `toolchain: 1.100.0`; the devcontainer runs `rustup toolchain install 1.100.0` |
| `worker-build` | `0.9.0` (exact) | `wasm-toolchain.json` (`workerBuild`) | `cargo install worker-build@0.9.0 --locked` (no `^0.9`) |
| Binaryen (`wasm-opt`) | `133` | `wasm-toolchain.json` (`binaryen`) | Downloaded by `worker-build` 0.9.0, which hardcodes `version_133`; cached in `~/.cache/worker-build` |
| `wasm-bindgen` | `0.2.130` | `Cargo.lock` (the crate) | `worker-build` downloads the CLI that matches the lockfile; the `wasm-smoke` CI job installs `wasm-bindgen-cli@0.2.130` and `crates/sws-wasm/run-smoke.sh` checks the match |
| esbuild | `0.28.2` | pinned inside `worker-build` 0.9.0 | Downloaded by `worker-build` |

Binaryen and esbuild are pinned indirectly: they change only when `worker-build` changes,
so the exact `worker-build` pin plus the recorded Binaryen number is the contract. The
`wasm-bindgen` CLI must equal the crate version in `Cargo.lock` exactly.

## Drift checks

- `npm run check:toolchain` (static, no network, part of `npm run verify`): fails if
  CI/release workflows, the devcontainer, `package.json` or `wrangler.jsonc` pin a
  different Rust, `worker-build` or `wasm-bindgen-cli` version, use a floating pin
  (`dtolnay/rust-toolchain@stable`, `worker-build@^0.9`), or a `wasm-bindgen-cli@X` pin
  differs from `Cargo.lock`.
- `npm run check:toolchain:installed`: additionally compares installed `rustc`,
  `worker-build`, a `wasm-bindgen` CLI on `PATH` (optional) and the downloaded `wasm-opt`
  with the pins. CI runs it in the `web` job; the release build runs it strictly (a
  missing `wasm-opt` is an error) after every real build.
- `npm run check:wasm-features`: checks the target features of the built artifact.

## Builds: dev vs release

| Script | What it does |
| --- | --- |
| `npm run build:wasm:dev` | `worker-build --dev` (unoptimized, debug info; Cargo `target/.../debug`) |
| `npm run build:wasm:release` | `worker-build --release` (`opt-level = "z"`, LTO, `wasm-opt`; Cargo `target/.../release`) |
| `npm run build` | alias of `build:wasm:release` (CI, `test:checkout`) |

Both go through `scripts/build-wasm.mjs`, which hashes only the Rust inputs (root
`Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `wasm-toolchain.json`, the Worker crate
and its transitive `path` dependencies) and stores the hash and profile in
`target/wasm-build/<crate>-stamp.json` (one stamp per Worker crate). If the stamp matches and the artifacts exist, it prints
`skipping Cargo` and exits: edits to static assets, `sites/`, `core/public`, JS, CSS or
HTML do not run Cargo or `worker-build`. `wrangler.jsonc` `build.command` uses the same
script (release profile), so `npm run dev` no longer rebuilds on every start. Use
`--force` to rebuild regardless. The dev and release profiles share `crates/sws-worker-site/build/`,
so switching profile rebuilds that output (Cargo keeps separate caches for each profile).
Do not deploy a dev build.

CI caches Cargo with `Swatinem/rust-cache` (keyed on the toolchain and lockfile; the
`wasm-dev` job uses its own key) and `~/.cache/worker-build` keyed on
`wasm-toolchain.json` and `Cargo.lock`.

## Required Wasm target features

`core/tools/check-wasm-features.mjs` reads the `target_features` custom section and fails
if the module does not validate, the section is missing (wasm-bindgen needs it, hence
`strip = "debuginfo"` in `Cargo.toml`), or any feature is outside the allowlist in
`wasm-toolchain.json`. Observed on a real release build with the pins above:

| Stage | Features |
| --- | --- |
| Cargo output (`target/wasm32-unknown-unknown/release/sws_worker_site.wasm`) | `bulk-memory`, `bulk-memory-opt`, `call-indirect-overlong`, `multivalue`, `mutable-globals`, `nontrapping-fptoint`, `reference-types`, `sign-ext` |
| After `worker-build --release` (`build/index_bg.wasm`) | the above plus `exception-handling` (added by `wasm-opt`; the dev build skips `wasm-opt` and keeps the eight Cargo features) |

- The eight Cargo-stage features are the rustc/LLVM defaults for `wasm32-unknown-unknown`
  at Rust 1.100.0; they are not opt-in. A Rust bump may change this set, which is why the
  check fails on unknown features instead of silently accepting them.
- `exception-handling` is enabled by `worker-build`'s `wasm-opt` invocation for the
  wasm-bindgen catch wrappers. `worker-build` deliberately enables an explicit list rather
  than `--all-features` so newer Binaryen encodings are not emitted before workerd
  supports them.
- Anything else (`simd128`, `atomics`, `relaxed-simd`, `tail-call`, `gc`,
  `extended-const`, ...) fails the check. The check proves the module validates in the
  host Node/V8 and uses only allowlisted features; it does not run the module inside
  workerd.

## Bumping a pin

1. Rust: edit `channel` in `rust-toolchain.toml` and every `toolchain:` / `rustup
   toolchain install` occurrence in `.github/workflows/*.yml` and
   `.devcontainer/devcontainer.json`. `npm run check:toolchain` lists any miss.
2. `worker-build`: edit `workerBuild` in `wasm-toolchain.json` and each
   `worker-build@X` (workflows, devcontainer, `package.json`, `wrangler.jsonc`). Read the
   new version's `src/versions.rs` for its Binaryen number and wasm-bindgen range, update
   `binaryen`, and keep `Cargo.lock`'s `wasm-bindgen` inside that range.
3. `wasm-bindgen`: updates through `Cargo.lock`; update `wasm-bindgen-cli@X` in
   `.github/workflows/ci.yml` to the same version.
4. Run `npm run build:wasm:release` (re-verifies Binaryen and features) and
   `npm run verify`. If the feature set changed, update `wasmFeatures` in
   `wasm-toolchain.json` and the table above after confirming workerd support.

Any time spent debugging this toolchain counts toward Gate 1.
