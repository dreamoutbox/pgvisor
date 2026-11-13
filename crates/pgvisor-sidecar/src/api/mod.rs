mod api_state;
#[cfg(test)]
mod api_test;
mod audit;
mod auth;
mod authenticated_user;
pub mod backup;
mod cluster;
mod cluster_actions;
mod error;
mod members;
mod metrics;
mod password_verifier;
mod peer_probe;
mod pg_activity;
mod router;
mod server;
mod session_token;
mod sql;
mod sql_runner;
mod superusers;
mod users;

pub use api_state::ApiState;
pub use audit::{get_audit_logs, AuditQuery};
pub use auth::{api_auth_middleware, extract_token_from_headers, get_me, post_login};
pub use authenticated_user::AuthenticatedUser;
pub use backup::{BackupRetentionConfig, LeaderRestoreExecutor, LocalLeaderRestoreExecutor};
pub use cluster::{
    collect_cluster_overview, get_node_config, get_node_logs, get_nodes, get_status, LogsQuery,
};
pub use cluster_actions::{
    post_node_action, post_repoint_node, post_restart_node, post_start_node, post_stop_node,
    post_switchover,
};
pub use error::ApiError;
pub use members::{
    build_api_url_from_peer, collect_cluster_members, get_cluster_members, parse_node_id_from_peer,
    require_leader,
};
pub use metrics::{
    build_metrics_snapshot, get_metrics_history, get_metrics_snapshot, spawn_metrics_sampler,
    MetricsRing,
};
pub use password_verifier::{
    verify_scram_sha256, PasswordCheckOutcome, PasswordChecker, PostgresPasswordChecker,
};
pub use peer_probe::{pg_address_from_peer_url, probe_all_peers, PeerProbe};
pub(crate) use peer_probe::fetch_peer_status;
pub use pg_activity::{
    sample_local_activity, sum_cluster_activity, sum_pg_stat_database_rows, ActivityTracker,
    PG_STAT_DATABASE_SQL,
};
pub use router::build_api_router;
pub use server::{resolve_sidecar_ports, spawn_api_server};
pub use session_token::{SessionClaims, SessionTokenSigner, DEFAULT_TOKEN_TTL_SECS};
pub use sql::{
    delete_table_row, execute_sql, get_table_data, get_table_schema, is_valid_identifier,
    list_databases, list_tables, update_table_row, TablesQuery,
};
pub use sql_runner::{ApiSqlRunner, LivePgSqlRunner};
pub use superusers::{SuperuserAllowlist, SUPERUSER_ENV};
pub use users::{
    alter_user, create_user, drop_user, get_user, get_user_privileges, grant_membership,
    list_users, revoke_membership, set_user_privilege,
};
pub use pgvisor_core::api::SidecarRole;
