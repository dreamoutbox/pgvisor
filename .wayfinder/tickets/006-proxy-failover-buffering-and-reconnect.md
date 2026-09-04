---
id: "006"
title: "Proxy Failover Buffering and Reconnection"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

When the Raft leader changes, how does the L7 proxy pause and buffer in-flight client queries or transactions, drain old backend connections, and seamlessly transparently re-route writes to the newly promoted leader without dropping client TCP connections?

## Resolution

1. **Topology Notification & Stale Drain**:
   - `ConnectionPool` tracks `topology_version` via `tokio::sync::watch`.
   - When `update_topology(new_leader, standbys)` is called, stale idle leader connections are immediately drained to prevent split-brain writes, and subscribers are notified.

2. **Pause & Buffer Window (`acquire_with_retry`)**:
   - When a query requires a backend and none is available or the cluster is undergoing election, `ClientSession` enters a buffering window (configurable via `FailoverConfig`, default 10s timeout, 100ms polling/notification wakeup).
   - The client TCP connection remains connected without dropping.

3. **Transparent Query Replay**:
   - The proxy buffers outbound query frames (`FrontendMessage::Query`).
   - If the backend connection disconnects before any response bytes are written to the client (`client_bytes_written == 0`), the proxy discards the broken backend, re-acquires a connection to the newly promoted leader, and transparently replays the buffered query.

4. **Safe Error Fallback for Partial Dispatches**:
   - If backend disconnection occurs after partial response bytes have already streamed to the client, transparent replay is disallowed to prevent duplicate processing.
   - The proxy transmits a standard Postgres `ErrorResponse` (SQLSTATE `57P01`) and `ReadyForQuery(Idle)`, preserving the client TCP session so the application can cleanly retry without reconnecting.
