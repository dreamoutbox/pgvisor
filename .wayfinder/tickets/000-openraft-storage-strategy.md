---
id: "000-openraft-storage-strategy"
title: "OpenRaft Storage Strategy"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

Which embedded storage engine should sidecars use for OpenRaft consensus log and state?

## Resolution

Build our own custom pure-Rust append-only storage engine for OpenRaft log records and state machine snapshots, avoiding external C/C++ or heavy third-party storage engines.
