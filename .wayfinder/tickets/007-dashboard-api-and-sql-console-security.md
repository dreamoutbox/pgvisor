---
id: "007"
title: "Dashboard API and SQL Console Security"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: ["002", "004"]
---

## Question

How does the Axum + Askama dashboard query cluster state from sidecars, authenticate admin users, and safely execute direct SQL queries (read-only enforcement, execution timeouts, session isolation) against the cluster?

## Resolution

1. **Dashboard UI & API (`pgvisor-dashboard`)**:
   - Axum web server embedding Askama HTML templates for cluster overview (`/`), node topology inspection (`/nodes`), and guarded SQL console (`/sql`).
   - JSON API endpoints: `GET /api/status` for cluster health metrics, and `POST /api/sql` for guarded query execution.

2. **SQL Console Security Engine (`SqlSecurityGuard`)**:
   - **Comment Stripping**: Removes single-line (`--`) and block comments (`/* ... */`) before inspecting SQL AST tokens to prevent comment-smuggling bypasses.
   - **Multi-Statement Rejection**: Strictly forbids semicolon-delimited multi-statements (`SELECT 1; DROP TABLE users;`).
   - **Read-Only Statement Enforcement**: Only permits read-only statements (`SELECT`, `SHOW`, `EXPLAIN`, `WITH ... SELECT`). Rejects any mutating statements (`INSERT`, `UPDATE`, `DELETE`, `DROP`, `ALTER`, `CREATE`, `TRUNCATE`, `GRANT`, `REVOKE`, `VACUUM`, `CALL`, `DO`, `COPY`, `SELECT INTO`).
   - **Statement Timeout & Row Capping**: Wraps all executions in `tokio::time::timeout` (default 5s) returning `STATEMENT_TIMEOUT` on overrun, and caps result sets at 500 rows.

3. **Admin Token Authentication**:
   - Admin routes and API actions validated against configurable `PGVISOR_ADMIN_TOKEN` via `Bearer` token header.

4. **Backup Target & Schedule**:
   - Default target: S3 MinIO local development server (`http://127.0.0.1:9000`, bucket `pgvisor-backups`).
   - Schedule defaults: Continuous WAL archiving + hourly incremental base snapshot (`incremental_interval_secs: 3600`), and full physical snapshot daily after midnight (`01:00 UTC`). Cloud providers like Google Drive/Dropbox can be attached via OpenDAL later.
