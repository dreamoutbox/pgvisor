---
id: "000-l7-protocol-and-pooling"
title: "L7 Protocol & Pooling Mode"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

What level of PostgreSQL protocol handling and connection pooling should the proxy implement?

## Resolution

L7 Postgres Wire Protocol Proxy with read/write splitting and transaction-level connection pooling (clients borrow backend connections per transaction/query for maximum client scalability, tracking transaction status flags).
