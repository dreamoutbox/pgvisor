use askama::Template;
use crate::models::{ClusterOverview, NodeHealthState, NodeRole, NodeSummary};

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
