---
id: "000-bootstrapping-and-discovery"
title: "Bootstrapping & Discovery"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

How should cluster nodes discover each other and bootstrap OpenRaft on first boot?

## Resolution

Static configuration / environment seeds: a preset list of peer sidecar addresses is provided via config/environment variables; node-1 bootstraps the initial OpenRaft group, and peer nodes register upon startup.
