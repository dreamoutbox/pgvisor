# AGENTS.md - PgVisor Developer & Agent Guide

## Project Overview

**PgVisor** is a lightweight, simplified PostgreSQL High Availability (HA) cluster supervisor and proxy built in Rust. It eliminates the complexity of traditional PostgreSQL cluster setups for developers through:
- **L7 Connection Proxy (`pgvisor-proxy`)**: PostgreSQL wire protocol proxy supporting transaction-level connection pooling and read/write splitting (writes to Raft leader, reads to standby replicas) with failover connection buffering.
- **Sidecar Supervisor (`pgvisor-sidecar`)**: Container PID 1 process managing the local PostgreSQL instance (`initdb`, configuration generation, process supervision, healthchecks, Raft quorum lease fencing via `pg_ctl stop -m immediate`, and promotion via `pg_ctl promote`).
- **Distributed Consensus**: OpenRaft-based HA election and membership using a custom pure-Rust append-only storage engine.
- **Continuous Backups**: Physical basebackup snapshots and continuous WAL archiving (`archive_command`) to S3/MinIO/cloud storage via OpenDAL, supporting Point-In-Time-Recovery (PITR).
- **Web Dashboard (`pgvisor-dashboard`)**: Axum + Askama UI embedded in the proxy service for cluster status, node inspection, backup management, and SQL execution.

---

## Planned Project Structure

```text
pgvisor/
├── .wayfinder/                      # Wayfinder roadmap, decisions & frontier tickets
│   ├── map.md                       # Canonical map index
│   └── tickets/                     # Open and closed decision tickets
├── crates/
│   ├── pgvisor-core/                # Shared types, Raft state machine, storage traits, wire protocol common
│   ├── pgvisor-proxy/               # L7 Postgres wire protocol proxy & connection pooler
│   ├── pgvisor-sidecar/             # PID 1 Postgres process supervisor, Raft node, OpenDAL backup worker
│   └── pgvisor-dashboard/           # Axum + Askama web UI and API handlers
├── docker-compose.yml               # Local MinIO dev setup & cluster compose
├── reset-docker-compose.sh          # Helper to wipe volumes and restart compose services
├── THIS-PROJECT-GOAL.md             # High-level product requirements
├── AGENTS.md                        # This file (guidelines & cautions for coding agents)
└── Cargo.toml                       # Workspace manifest
```

---

## Rules & Cautions for AI Agents

### 4. Technical Gotchas
- **OpenRaft Storage**: Do not pull in RocksDB or external C++ dependencies. PgVisor builds its own custom pure-Rust append-only storage engine.
- **Package Manager**: If any web assets or frontend tools are used, use `pnpm` exclusively (never `npm` or `yarn`).
- **PID 1 Responsibilities in Sidecar**: The sidecar acts as container PID 1. It must properly reap child zombie processes (`waitpid`) and propagate signals (`SIGTERM`, `SIGINT`, `SIGQUIT`) to Postgres.
- **Fencing over Promotion**: During network splits, the old leader must be fenced (killed via `pg_ctl stop -m immediate`) before or concurrently with standby promotion to prevent split-brain data corruption.
- **Cluster Restore & Standby Timeline Realignment**: Restoring a database snapshot rewinds the leader's timeline and LSN. Standby replicas cannot resume replication without re-cloning from the restored leader. Cluster restores must be coordinated through the sidecar control API:
  1. Restore leader node via `POST /control/restore`.
  2. Re-sync all standby replicas via `POST /control/resync` (`pg_basebackup`).
  3. Drain proxy connection pool (`pool.drain_all()`) so client connections refresh to the restored timeline.
- **Sidecar Supervision Boundaries (No Out-of-Band `pg_ctl`)**: When `pgvisor-sidecar` runs as container PID 1, never execute out-of-band `pg_ctl stop`/`pg_ctl start` commands via shell scripts or `docker exec`. External stops corrupt process accounting and supervisor state. Always issue lifecycle and restore commands through the sidecar's internal HTTP control API.
- **Cluster Reset & Docker Compose Helper (`reset-docker-compose.sh`)**: Running `./reset-docker-compose.sh` wipes volumes and restarts containers with fresh data, rebuilding Docker images by default. Pass `--no-build` (`./reset-docker-compose.sh --no-build`) to skip rebuilding images, or `-s`/`--silent` to suppress verbose output unless an error occurs.
- **Test Flakiness from Read/Write Splitting & Replication Lag**:
  - **Why it occurs**: `pgvisor-proxy` enforces L7 read/write splitting: mutating queries and DDL (`CREATE TABLE`, `DROP TABLE`, `CREATE ROLE`, `GRANT`, `ALTER`, etc.) route to the Raft leader, while read-only queries (`SELECT`, catalog lookups in `pg_roles` or `information_schema`) route to standby replicas. Under concurrent test runs (such as `./test.sh -j 5`) or heavy system load, PostgreSQL asynchronous streaming replication to standbys exhibits non-zero lag (from tens of milliseconds up to a few seconds). If a test script executes a write or DDL statement and immediately fires an instantaneous `SELECT` verification through the proxy, the read query can hit a replica that has not yet replayed the corresponding WAL, triggering false assertion failures (e.g., table still found after `DROP TABLE`, or newly created role/privilege not yet visible in `pg_roles`).
  - **Remedy & Rule**: Test scripts verifying state through the proxy after mutating commands must never rely on single-shot reads. Always use a polling retry loop (e.g., 5–10 attempts with 1-second sleeps) until the replica catches up, or query the leader directly if strict immediate consistency is required.

# Rust Rules

- use Rust 2024 edition

## Error Handling
- Lib crates: `thiserror`, no `anyhow` in public APIs.
- Bin/app crates: `anyhow::Result` at boundaries (`main`, CLI handlers).
- No `unwrap`/`expect` outside tests/`main`; propagate with `?`.
- Unavoidable `unwrap`/`expect` → comment why it can't panic.
- No `let _ = ...` swallowing fallible calls without a comment.

## Match Ergonomics
- No `ref`/`ref mut` in new code (2018+ ergonomics infer binding modes).
  - `if let Some(x) = opt.as_ref() {}` / `.as_mut()` — not `Some(ref x)`/`Some(ref mut x)`.
  - Already have `&mut Option<T>`? `if let Some(x) = opt {}` binds `&mut T` directly.
  - Exception: mixed by-ref/by-value struct destructure ergonomics can't express, e.g. `let Struct { ref mut a, b } = s;`.

## Ownership & Borrowing (perf-first)
- Borrow by default; `.clone()` is last resort.
- Read-only params: `&T` (`&str`/`&[T]`, not `&String`/`&Vec<T>`).
- Borrow conflicts: try smaller scope, reordering, split borrows (`split_at_mut`, distinct fields) before cloning.
- `Copy` types: pass by value.
- Return `&str`/slice/`impl Iterator` over owned `Vec`/`String` when caller may not need ownership.
- `Cow<'_, T>` for usually-borrow/sometimes-own.
- No allocation in hot loops — hoist out.
- Clone `Rc`/`Arc` handles, not underlying data; `Arc` only across threads.
- Build strings with `String::with_capacity` + `push_str`/`write!`, not repeated `format!`.
- Iterator chains over index loops + intermediate `Vec`s; `.collect()` only when ownership needed.
- In-place `&mut` mutation over clone-mutate-reassign.
- Closures: borrow (`|x: &T|`); `move` only if it must outlive scope.
- Struct fields: owned `String`/`Vec<T>` unless `&'a` clearly fits without cascading lifetimes into 3+ signatures.
- Any non-trivial `.clone()` needs a one-line justification comment.

## Module Organization

1. One responsibility per file; split past ~1000-2000 lines or multiple concerns.
2. File-per-module (2018+), sparse `mod.rs`:
   ```
   src/user/mod.rs        // re-exports + wiring only
   src/user/model.rs
   src/user/repository.rs
   src/user/service.rs
   ```
3. `mod.rs`/module roots: only `mod x;` + `pub use x::Y;` — no logic/structs/impls.
4. Split by domain concept (`repository.rs`, `validation.rs`), never `structs.rs`/`impls.rs`/`traits.rs` dumps.
5. `error.rs` per module: `thiserror` in libs, `anyhow` only at bin boundary — never mixed.
6. `lib.rs`/`main.rs`: module decls, top-level re-exports, wiring only.
7. Visibility: private by default, widen to `pub(crate)` before `pub`. No `pub use module::*`.
8. Tests: `#[cfg(test)] mod tests` in separate file, suffix with `_test.rs`; `tests/` at crate root, one file per surface (integration).
9. Circular module deps → extract shared types into `a_b_common.rs` or shared crate.

**Anti-patterns (never):** unrelated structs/traits crammed in one file · logic in `mod.rs` · wildcard re-exports · files past ~2000 lines unsplit · duplicated logic instead of a shared function.

## Rust: One Type Per File

Each `struct`/`enum` lives in its own file named after the type in `snake_case`. Its `impl` blocks and trait impls stay in the same file.

- `JobSummary` → `src/summary/job_summary.rs`
- `RemoteSummary` → `src/summary/remote_summary.rs`
- `StorageSummary` → `src/summary/storage_summary.rs`

### Module layout
- Group related types in a domain directory (`summary/`).
- `mod.rs` holds only `mod` declarations and `pub use` re-exports. No types or logic.
- Callers import from the module root: `use crate::summary::JobSummary;`
- Follow the repo's existing style (`mod.rs` vs `summary.rs` + `summary/`). Don't mix.

### Rules
- New type = new file + register in `mod.rs`. Never append to an existing type file.
- Touching a file with 2+ independent public types: split it, behavior unchanged.
- Allowed to share a file: private helpers used by one parent, tiny newtypes/markers (<10 lines), enums used only by one struct, and builders (with the type they build).
- Keep visibility unchanged. Don't widen to `pub` to make it compile; prefer `pub(crate)`.
- No `use super::*;` and no `#[path]`. Import explicitly.
- File name is the bare snake_case type name, no suffixes. Shared errors go in `error.rs`, shared traits in `traits.rs`.

## Rust testing

- Run tests with `cargo nextest run`, never `cargo test`.
- Run tests one at a time, serially: `--test-threads 1`.
- Fail fast: stop on first failure with `--fail-fast` (the default; don't override with `--no-fail-fast`).
- Default command:

```sh
cargo nextest run --test-threads 1 --fail-fast
```

- To run a single test, pass a filter or filterset:

```sh
cargo nextest run --test-threads 1 --fail-fast <test_name>
cargo nextest run --test-threads 1 --fail-fast -E 'test(=<exact_name>)'
cargo nextest run --test-threads 1 --fail-fast -p <crate>
```

- Doctests are not run by nextest. If needed, run them separately: `cargo test --doc`.
- When a test fails: read the output, fix the root cause, re-run only that test, then re-run the full suite once.
- Never skip, `#[ignore]`, delete, or weaken a failing test to get a green run. Never retry to mask flakiness; report it.
- Do not run tests in the background or in parallel with other cargo commands (lock contention on `target/`).

## Debug Logging With Rust

### Never use `println!`/`dbg!` for persistent debug output
Use `tracing` instead. Reserve `dbg!` only for throwaway, local, uncommitted debugging.

### Setup (required once per binary)
```rust
fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    // ...
}
```
Without this, `RUST_LOG` does nothing — no subscriber, no output.

### Logging conventions

- Use `debug!`/`trace!` for dev-only diagnostics, `info!`/`warn!`/`error!` for things that matter in prod.
- Use structured fields, not string concatenation:
  ```rust
  tracing::debug!(?user_id, %email, "checking auth");
  ```
  - `?field` → `Debug` format
  - `%field` → `Display` format
- Never manually prefix messages with markers like `=====` — target/module filtering replaces that need entirely.
- Override `target` when you want a logical grouping that cuts across modules:
  ```rust
  tracing::debug!(target: "auth_flow", ?user_id, "checking credentials");
  ```
- Use spans for anything async/concurrent — plain logs interleave unpredictably across tasks:
  ```rust
  let span = tracing::info_span!("handle_request", request_id = %id);
  let _guard = span.enter();
  ```

### Filtering with `RUST_LOG`

`RUST_LOG` matches on **target** (defaults to module path), not log text.

do not `RUST_LOG=debug`, scope what you need.

```bash
RUST_LOG=my_crate=debug                 # just your crate
RUST_LOG=my_crate::auth=trace           # just one module, trace+
RUST_LOG=my_crate::auth=trace,warn      # auth at trace, everything else at warn
RUST_LOG=my_crate::auth::login=debug    # down to a single submodule
RUST_LOG=auth_flow=debug                # matches a manual `target:` override
```

### Finding log output

Multi-target match in one non-interactive pass — no `less`, no re-running, no back-and-forth:

```bash
cargo run 2>&1 | rg "TARGET_1|TARGET_2|TARGET_3"
cargo run 2>&1 | grep -E "TARGET_1|TARGET_2|TARGET_3"
```

- `-E` (or `rg`'s default) enables regex alternation — `|` means OR, matches any line containing any target.
- Add `-A3` / `-B1` for trailing/leading context lines if a single match line isn't enough:
  ```bash
  cargo run 2>&1 | rg -A3 "TARGET_1|TARGET_2"
  ```
- Save to a file for later inspection instead of scrolling live:
  ```bash
  cargo run 2>&1 | rg "TARGET_1|TARGET_2" > filtered.log
  ```
- Always redirect stderr too (`2>&1`) — default `tracing` output can go to either stream depending on subscriber config.

Prefer scoping at the source with `RUST_LOG` targets (above) over grepping when possible — filtering before print is cheaper than filtering after.
