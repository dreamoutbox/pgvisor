---
id: "001"
title: "Custom OpenRaft Storage Engine Design"
type: "prototype"
status: "open"
assignee: "unassigned"
blocked_by: []
---

## Question

What is the exact data layout, on-disk file format (e.g., append-only log record format with index headers, state machine key-value store, and snapshot serialization), and concurrency model for our custom pure-Rust OpenRaft storage engine?
