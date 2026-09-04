# PgVisor

the current PostgreSQL cluster setup is very complex. I want it to be easy for newbie developer or who is not familliar with PostgreSQL to use it.

I want to create a proxy. and a sidecar worker for each Postgresql instance. 

## Objectives.

- create this project in Rust programming languages.
- I want proxy to loadbalance the connection to the Postgresql instances. (like pgbouncer or pgpool2)
- the sidebar worker is doing PostgreSQL config setup, backup/restore, healthchecks.
- the sidecar worker can vote for leader election or promote standby to leader for High Availability without human intervention. (use rust openraft crate)
- the backup data can be store on S3/Dropbox/GoogleDrive or local disk. (use opendal crate)
- I want the web dashboard to view the cluster status. view each node config, manage backup, direct SQL access.

## Development

- keep it simple.
- setup and testing in Docker.
- use Minio for S3 dev testing.
- use Rust tokio for async runtime.
- use Axum for web server. use askama for template.
