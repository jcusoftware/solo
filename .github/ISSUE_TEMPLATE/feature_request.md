---
name: Feature Request
about: Propose an enhancement or new capability for Solo Website System (SWS)
title: "feat: "
labels: ["enhancement"]
assignees: []
---

## Summary

A concise summary of the proposed feature.

## Problem / Use Case

What problem does this solve, and who benefits?

## Scope & Architecture Placement

Select where this capability belongs:

- [ ] Core SWS Engine (`crates/*`) — must remain industry-agnostic and `MIT OR Apache-2.0`
- [ ] Shared Packages / Studio Apps (`packages/*`, `apps/*`)
- [ ] Industry Vertical Pack (`packs/*`)
- [ ] Tooling / CI / Documentation

## Proposed Solution

Describe the API, IR/CSS schema changes, CLI flags, or UI behavior you envision.

## Accessibility, Security & Licensing Notes

- **WCAG 2.2 AA impact:**
- **Security / Worker limits impact (16 KB cap, headers, Turnstile, D1 bounds):**
- **New dependencies (must pass `cargo deny check` with permissive OSI licenses):**

## Alternatives Considered

Any alternative approaches or workarounds you have evaluated.
