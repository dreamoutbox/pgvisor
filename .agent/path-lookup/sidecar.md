### If you want to modify PostgreSQL sidecar process supervision, configuration templating, signals, or fencing, then check:

- `crates/pgvisor-sidecar/src/config.rs` = PostgreSQL configuration generator for `postgresql.conf`, `pg_hba.conf`, and replication `standby.signal`
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor` managing `initdb`, child process spawning, pipe logging, `pg_ctl promote`, and emergency fencing (`pg_ctl stop -m immediate`)
- `crates/pgvisor-sidecar/src/main.rs` = Sidecar service entry point and container PID 1 signal listener (`SIGTERM`, `SIGINT`, `SIGQUIT`)
