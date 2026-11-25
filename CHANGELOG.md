# Changelog

All notable changes to Solo Website System (SWS) will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Pivoted the repository into **Solo Website System (SWS)**, an industry-agnostic, deterministic website compiler and visual studio workspace organized as a Cargo + pnpm monorepo (`crates/`, `apps/`, `packages/`, `packs/`).
- Decoupled core engine crates (`crates/*`) from industry-specific vertical logic, isolating vertical-specific features, templates, and rules under `packs/` (including `packs/hospitality`).
- Replaced `main`-branch `release-plz` automation with a strictly tag-only GitHub Actions release pipeline (`.github/workflows/release.yml` triggered on `v*` and `pack-hospitality-v*` tags, with optional tag-gated Cloudflare Worker deployment).

### Fixed

- Aligned `deploy/cloudflare/wrangler.studio.example.jsonc` paths with `crates/sws-worker-studio`.
- Fixed shell operator precedence for `corepack` fallback in `.devcontainer/devcontainer.json`.
- Updated `README.md`, `CONTRIBUTING.md`, `docs/ARCHITECTURE.md`, `ROADMAP.md`, and `.github/PULL_REQUEST_TEMPLATE.md` for tag-only releases, `npm ci` onboarding, and Rust-first builder/export milestones.

### Added

- Static Landing (`sites/landing`, `sws-landing`, custom domain `solo.jcusoftware.com`) and Documentation (`sites/docs`, `sws-docs`, custom domain `docs.jcusoftware.com`) sites generated deterministically by `sites/build.mjs`, including release CLI download links, Getting Started, User Guide, CLI & MCP Reference, Packs, Live Showcase, Accessibility & Security, Deployment, FAQ & Troubleshooting, and Roadmap & Licensing pages.
- Converted the Cloudflare deploy step in `.github/workflows/release.yml` to build and deploy both static Workers Assets sites (`sws-landing` and `sws-docs`) on `v*` tags with exact release download URLs and security-header parity checks.
- Dual open-source licensing (`MIT OR Apache-2.0`) via `LICENSE-MIT` and `LICENSE-APACHE`.
- Community and governance documentation: `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, and `ROADMAP.md`.
- GitHub issue and pull request templates in `.github/ISSUE_TEMPLATE/` and `.github/PULL_REQUEST_TEMPLATE.md`.
