---
id: "002"
title: "Postgres Wire Protocol Framing and Pooling"
type: "research"
status: "open"
assignee: "unassigned"
blocked_by: []
---

## Question

How will the L7 proxy handle PostgreSQL wire protocol state tracking (StartupMessage, SSLRequest, SCRAM/password authentication forwarding, ReadyForQuery transaction status indicators 'I'/'T'/'E', prepared statement lifetime, and backend multiplexing)?
