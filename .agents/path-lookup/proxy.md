### If you want to modify PostgreSQL wire protocol parsing, connection pooling, or proxy routing, then check:

- `crates/pgvisor-core/src/protocol/message.rs` = PostgreSQL 3.0 wire protocol packet framing, StartupMessage, SSLRequest, Frontend and Backend message codecs backed by `pgwire` crate
- `crates/pgvisor-core/src/protocol/tracker.rs` = `TransactionTracker` watching ReadyForQuery ('I'/'T'/'E') and query classifier for read/write splitting
- `crates/pgvisor-proxy/src/pool.rs` = `ConnectionPool` managing round-robin standby routing, per-node idle connection pools, failover retry loop (`acquire_with_retry`), and topology notifications
- `crates/pgvisor-proxy/src/session.rs` = `ClientSession` driving handshake, borrowing backend connections, failover query buffering/transparent replay, and returning them on transaction boundary
- `crates/pgvisor-proxy/src/main.rs` = Proxy server entry point, TCP listener, embedded Axum dashboard, and dynamic topology monitor task polling sidecar nodes

### If you want to modify TLS/SSL encryption on the proxy or backend pool, then check:

- `crates/pgvisor-core/src/tls.rs` = `TlsCertPair` self-signed certificate generation via `rcgen`, certificate and key file persistence with 0600 permissions
- `crates/pgvisor-proxy/src/tls.rs` = `ClientStream` (Plain/Tls), `BackendStream` (Plain/Tls), `TlsAcceptor` builder for clients, `TlsConnector` builder for backends with `AcceptAnyServerCertVerifier`
- `crates/pgvisor-proxy/src/session.rs` = `ClientSession` SSLRequest handling ('S'/'N'), TLS stream upgrade via `TlsAcceptor`, and `tls_required` enforcement
- `crates/pgvisor-proxy/src/pool.rs` = `PooledConnection::connect_with_tls` executing backend SSLRequest handshake, and `ConnectionPool::with_tls_connector`
- `crates/pgvisor-proxy/src/main.rs` = Reading `PGVISOR_TLS_ENABLED`, `PGVISOR_TLS_REQUIRED`, `PGVISOR_TLS_CERT_DIR`, `PGVISOR_TLS_CERT_FILE`, `PGVISOR_TLS_KEY_FILE`, and wiring TLS into pool and listener
- `docker-compose.yml` = TLS environment variables and `proxy_tls_data` volume definition
- `scripts/test-tls.sh` = Standalone script testing client TLS connections via `openssl s_client` and `psql sslmode=require`
- `tests/test-tls.sh` = Integration test verifying cert creation, key permissions, unencrypted rejection, and encrypted query execution
- `dev-generate-ssl.sh` = Developer helper script generating local CA, proxy, node, and client certificates with 0600 key permissions
- `knowledges/tls-certificate-deployment-models.md` = Documentation of shared cluster vs per-node certificate deployment and renewal models
