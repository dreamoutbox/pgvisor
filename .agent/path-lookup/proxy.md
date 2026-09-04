### If you want to modify PostgreSQL wire protocol parsing, connection pooling, or proxy routing, then check:

- `crates/pgvisor-core/src/protocol/message.rs` = PostgreSQL 3.0 wire protocol packet framing, StartupMessage, SSLRequest, Frontend and Backend message codecs
- `crates/pgvisor-core/src/protocol/tracker.rs` = `TransactionTracker` watching ReadyForQuery ('I'/'T'/'E') and query classifier for read/write splitting
- `crates/pgvisor-proxy/src/pool.rs` = `ConnectionPool` managing idle leader/standby connections, failover retry loop (`acquire_with_retry`), and topology notifications
- `crates/pgvisor-proxy/src/session.rs` = `ClientSession` driving handshake, borrowing backend connections, failover query buffering/transparent replay, and returning them on transaction boundary
- `crates/pgvisor-proxy/src/main.rs` = Proxy server entry point and TCP listener
