---
id: "006"
title: "Proxy Failover Buffering and Reconnection"
type: "grilling"
status: "open"
assignee: "unassigned"
blocked_by: ["002", "004"]
---

## Question

When the Raft leader changes, how does the L7 proxy pause and buffer in-flight client queries or transactions, drain old backend connections, and seamlessly transparently re-route writes to the newly promoted leader without dropping client TCP connections?
