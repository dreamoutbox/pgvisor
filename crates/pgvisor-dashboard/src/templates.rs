use crate::models::{
    BackupItemView, BackupOverviewSummary, ClusterOverview, ColumnInfo, NodeHealthState, NodeRole,
    NodeSummary, TableSummary,
};
use askama::Template;

#[derive(Template)]
#[template(path = "overview.html")]
pub struct OverviewTemplate<'a> {
    pub overview: &'a ClusterOverview,
}

#[derive(Template)]
#[template(path = "nodes.html")]
pub struct NodesTemplate<'a> {
    pub nodes: &'a [NodeSummary],
}

#[derive(Template)]
#[template(path = "sql.html")]
pub struct SqlConsoleTemplate {}

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
}
