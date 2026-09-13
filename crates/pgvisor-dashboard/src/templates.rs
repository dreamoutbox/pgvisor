use crate::models::{
    AuditEventView, AuditOverviewStats, BackupItemView, BackupOverviewSummary, ClusterOverview,
    ColumnInfo, NodeHealthState, NodeRole, NodeSummary, PgRole, TablePrivilege, TableSummary,
};
use askama::Template;

#[derive(Template)]
#[template(path = "overview.html")]
pub struct OverviewTemplate<'a> {
    pub overview: &'a ClusterOverview,
    pub auth_enabled: bool,
}

#[derive(Template)]
#[template(path = "nodes.html")]
pub struct NodesTemplate<'a> {
    pub nodes: &'a [NodeSummary],
    pub auth_enabled: bool,
}

#[derive(Template)]
#[template(path = "sql.html")]
pub struct SqlConsoleTemplate {
    pub auth_enabled: bool,
}

#[derive(Template)]
#[template(path = "tables.html")]
pub struct TablesTemplate<'a> {
    pub tables: &'a [TableSummary],
    pub active_table: Option<&'a str>,
    pub active_tab: &'a str,
    pub columns: &'a [ColumnInfo],
    pub data_columns: &'a [String],
    pub data_rows: &'a [Vec<Option<String>>],
    pub total_rows: u64,
    pub page: usize,
    pub limit: usize,
    pub total_pages: usize,
    pub initial_sql: Option<&'a str>,
    pub auth_enabled: bool,
}

impl<'a> TablesTemplate<'a> {
    pub fn is_active_table(&self, name: &str) -> bool {
        self.active_table == Some(name)
    }
}

#[derive(Template)]
#[template(path = "backups.html")]
pub struct BackupsTemplate<'a> {
    pub backups: &'a [BackupItemView],
    pub summary: &'a BackupOverviewSummary,
    pub auth_enabled: bool,
}

#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginTemplate<'a> {
    pub cluster_id: &'a str,
    pub error: Option<&'a str>,
}

#[derive(Template)]
#[template(path = "users.html")]
pub struct UsersTemplate<'a> {
    pub roles: &'a [PgRole],
    pub active_role: Option<&'a PgRole>,
    pub active_tab: &'a str,
    pub table_privileges: &'a [TablePrivilege],
    pub all_tables: &'a [TableSummary],
    pub all_roles: &'a [PgRole],
    pub auth_enabled: bool,
}

impl<'a> UsersTemplate<'a> {
    pub fn is_active_role(&self, name: &str) -> bool {
        self.active_role.map(|r| r.rolname.as_str()) == Some(name)
    }
}

#[derive(Template)]
#[template(path = "audit.html")]
pub struct AuditTemplate<'a> {
    pub events: &'a [AuditEventView],
    pub stats: &'a AuditOverviewStats,
    pub active_kind: Option<&'a str>,
    pub search_query: Option<&'a str>,
    pub page: usize,
    pub limit: usize,
    pub total_pages: usize,
    pub total_events: usize,
    pub auth_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_template_rendering() {
        let stats = AuditOverviewStats {
            total_events: 2,
            dangerous_sql_count: 1,
            latest_pitr_target: Some("2026-09-09 12:00:00 UTC".to_string()),
        };
        let events = vec![
            AuditEventView {
                id: 1,
                occurred_at: "2026-09-09 12:00:00 UTC".to_string(),
                kind: "dangerous_sql".to_string(),
                kind_display: "Dangerous SQL".to_string(),
                node_id: Some(1),
                node_address: Some("pgvisor-node1:5432".to_string()),
                detail: "DROP TABLE test_tbl;".to_string(),
                pitr_target: Some("2026-09-09 11:59:59 UTC".to_string()),
            },
            AuditEventView {
                id: 2,
                occurred_at: "2026-09-09 12:01:00 UTC".to_string(),
                kind: "user_permission".to_string(),
                kind_display: "User & Role".to_string(),
                node_id: None,
                node_address: None,
                detail: "Role created".to_string(),
                pitr_target: None,
            },
        ];

        let template = AuditTemplate {
            events: &events,
            stats: &stats,
            active_kind: None,
            search_query: None,
            page: 1,
            limit: 50,
            total_pages: 1,
            total_events: 2,
            auth_enabled: false,
        };

        let rendered = template
            .render()
            .expect("AuditTemplate must render without errors");
        assert!(rendered.contains("Cluster Audit Logs"));
        assert!(rendered.contains("PITR Restore Target:"));
        assert!(rendered.contains("DROP TABLE test_tbl;"));
        assert!(rendered.contains("User &amp; Role"));
    }

    #[test]
    fn test_overview_template_rendering_with_charts() {
        let overview = ClusterOverview {
            cluster_id: "test-cluster".to_string(),
            current_term: 1,
            leader_id: Some(1),
            leader_address: Some("127.0.0.1:5432".to_string()),
            quorum_size: 2,
            total_nodes: 1,
            healthy_nodes: 1,
            last_backup_at: None,
            total_backups: 0,
            nodes: vec![NodeSummary {
                node_id: 1,
                address: "127.0.0.1:5432".into(),
                role: NodeRole::Leader,
                state: NodeHealthState::Healthy,
                pg_version: "18.6".into(),
                replication_lag_bytes: 0,
                uptime_secs: 3600,
                is_local: true,
            }],
        };
        let template = OverviewTemplate {
            overview: &overview,
            auth_enabled: true,
        };
        let rendered = template
            .render()
            .expect("OverviewTemplate must render without errors");
        assert!(rendered.contains("Cluster: test-cluster"));
        assert!(rendered.contains("Cluster Metrics &amp; Telemetry"));
        assert!(rendered.contains("uptimeChart"));
        assert!(rendered.contains("nodeQueriesChart"));
        assert!(rendered.contains("cpuChart"));
        assert!(rendered.contains("memChart"));
        assert!(rendered.contains("proxyQueriesChart"));
        assert!(rendered.contains("replicationLagChart"));
        assert!(rendered.contains("backupSizeThroughputChart"));
        assert!(rendered.contains("backupRateChart"));
    }
}
