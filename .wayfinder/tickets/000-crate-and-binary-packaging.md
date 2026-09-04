---
id: "000-crate-and-binary-packaging"
title: "Crate & Binary Packaging"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

How should the Rust codebase be structured?

## Resolution

Cargo workspace with separate standalone binaries (`pgvisor-proxy`, `pgvisor-sidecar`, `pgvisor-dashboard`) sharing internal library crates (`pgvisor-core` and common modules).
