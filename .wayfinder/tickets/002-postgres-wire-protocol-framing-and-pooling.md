---
id: "002"
title: "Postgres Wire Protocol Framing and Pooling"
type: "research"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

How will the L7 proxy handle PostgreSQL wire protocol state tracking (StartupMessage, SSLRequest, SCRAM/password authentication forwarding, ReadyForQuery transaction status indicators 'I'/'T'/'E', prepared statement lifetime, and backend multiplexing)?

## Resolution

Implemented PostgreSQL wire protocol framing, query classification, and transaction-level pooling across `crates/pgvisor-core` and `crates/pgvisor-proxy`:
1. **Wire Protocol 3.0 Framing (`pgvisor-core/src/protocol/message.rs`)**:
   - Initial handshake decoder supporting `StartupMessage` (extracting `user`, `database`, parameters), `SSLRequest` (responding with `'N'`), and `CancelRequest`.
   - Regular message codecs for Frontend (`Query`, `Sync`, `Flush`, `Terminate`, `Password`, `Raw`) and Backend (`AuthenticationOk`, `ParameterStatus`, `ReadyForQuery`, `CommandComplete`, `BackendKeyData`).
2. **Transaction Status Tracking (`pgvisor-core/src/protocol/tracker.rs`)**:
   - `TransactionTracker` watches `ReadyForQuery` ('Z') status indicators:
     - `'I'`: Idle (safe to decouple and return backend connection to pool).
     - `'T'`: Active transaction block (client pinned to backend).
     - `'E'`: Error state (client pinned to backend awaiting rollback).
   - `classify_query` categorizes SQL into Read (`SELECT`, `SHOW`), Write (`INSERT`, `UPDATE`, `DELETE`, DDL), Begin, Commit, and Rollback.
3. **Connection Pool & Failover Draining (`pgvisor-proxy/src/pool.rs`)**:
   - `ConnectionPool` maintains separate idle queues for Leader and Standby nodes.
   - `update_topology` immediately drains idle leader connections when a new Raft leader is elected, preventing stale writes.
4. **Client Session Driver (`pgvisor-proxy/src/session.rs`)**:
   - Implements transaction-level pooling: client borrows backend connection on first query of transaction, proxies stream, and releases connection back to pool on receiving `'I'` in ReadyForQuery.
5. **Unit Tests**:
   - Verified encode/decode roundtrips for StartupMessage, SSLRequest, Query, and ReadyForQuery.
   - Verified query classification and routing decisions.
   - Verified pool topology update and failover connection draining.
