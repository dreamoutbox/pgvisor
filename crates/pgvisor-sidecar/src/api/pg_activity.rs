use std::collections::HashMap;
use std::time::Duration;

use pgvisor_core::api::{ClusterActivity, NodeMetrics, PgNodeActivity};
use pgvisor_core::pg_client::PgConnection;

const ACTIVITY_QUERY_TIMEOUT: Duration = Duration::from_millis(500);

/// Aggregates transaction and tuple counters across all databases on a PostgreSQL node.
pub const PG_STAT_DATABASE_SQL: &str = "SELECT COALESCE(SUM(xact_commit), 0)::bigint AS xact_commit, COALESCE(SUM(xact_rollback), 0)::bigint AS xact_rollback, COALESCE(SUM(tup_returned), 0)::bigint AS tup_returned, COALESCE(SUM(tup_fetched), 0)::bigint AS tup_fetched, COALESCE(SUM(tup_inserted), 0)::bigint AS tup_inserted, COALESCE(SUM(tup_updated), 0)::bigint AS tup_updated, COALESCE(SUM(tup_deleted), 0)::bigint AS tup_deleted FROM pg_stat_database;";

#[derive(Debug, Clone, Copy, Default)]
struct MonotonicCounter {
    last_raw: u64,
    offset: u64,
}

impl MonotonicCounter {
    fn current(&self) -> u64 {
        self.offset.saturating_add(self.last_raw)
    }

    fn observe(&mut self, raw: u64) -> u64 {
        if raw < self.last_raw {
            self.offset = self.offset.saturating_add(self.last_raw);
        }
        self.last_raw = raw;
        self.current()
    }
}

/// Preserves monotonic read/write counters per node across `pg_stat_reset()` or restarts.
#[derive(Debug, Default)]
pub struct ActivityTracker {
    per_node_reads: HashMap<u64, MonotonicCounter>,
    per_node_writes: HashMap<u64, MonotonicCounter>,
}

impl ActivityTracker {
    /// Records a newly sampled `PgNodeActivity` (if available) and returns cumulative `(reads, writes)`.
    ///
    /// # Arguments
    /// * `node_id` - Cluster identifier of the reporting node.
    /// * `activity` - Latest `pg_stat_database` reading, or `None` if temporarily unavailable.
    pub fn observe(&mut self, node_id: u64, activity: Option<PgNodeActivity>) -> (u64, u64) {
        let reads_counter = self.per_node_reads.entry(node_id).or_default();
        let writes_counter = self.per_node_writes.entry(node_id).or_default();
        match activity {
            Some(act) => (
                reads_counter.observe(act.reads()),
                writes_counter.observe(act.writes()),
            ),
            None => (reads_counter.current(), writes_counter.current()),
        }
    }
}

/// Sums `pg_reads` and `pg_writes` across all nodes into a cluster-wide [`ClusterActivity`].
///
/// # Arguments
/// * `nodes` - Per-node metrics collected for the current snapshot.
pub fn sum_cluster_activity(nodes: &[NodeMetrics]) -> ClusterActivity {
    let mut total = ClusterActivity::default();
    for node in nodes {
        total.reads_total = total.reads_total.saturating_add(node.pg_reads);
        total.writes_total = total.writes_total.saturating_add(node.pg_writes);
    }
    total
}

/// Maps query columns and rows returned from `pg_stat_database` into a summed [`PgNodeActivity`].
///
/// # Arguments
/// * `columns` - Column names from the simple-query response header.
/// * `rows` - Result rows returned by PostgreSQL.
pub fn sum_pg_stat_database_rows(columns: &[String], rows: &[Vec<String>]) -> Option<PgNodeActivity> {
    if rows.is_empty() {
        return None;
    }

    let find_col = |name: &str, fallback_idx: usize| -> usize {
        columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
            .unwrap_or(fallback_idx)
    };

    let idx_commit = find_col("xact_commit", 0);
    let idx_rollback = find_col("xact_rollback", 1);
    let idx_returned = find_col("tup_returned", 2);
    let idx_fetched = find_col("tup_fetched", 3);
    let idx_inserted = find_col("tup_inserted", 4);
    let idx_updated = find_col("tup_updated", 5);
    let idx_deleted = find_col("tup_deleted", 6);

    let parse_u64 = |row: &[String], idx: usize| -> u64 {
        row.get(idx)
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0)
    };

    let mut total = PgNodeActivity::default();
    for row in rows {
        total.xact_commit = total.xact_commit.saturating_add(parse_u64(row, idx_commit));
        total.xact_rollback = total
            .xact_rollback
            .saturating_add(parse_u64(row, idx_rollback));
        total.tup_returned = total
            .tup_returned
            .saturating_add(parse_u64(row, idx_returned));
        total.tup_fetched = total.tup_fetched.saturating_add(parse_u64(row, idx_fetched));
        total.tup_inserted = total
            .tup_inserted
            .saturating_add(parse_u64(row, idx_inserted));
        total.tup_updated = total.tup_updated.saturating_add(parse_u64(row, idx_updated));
        total.tup_deleted = total.tup_deleted.saturating_add(parse_u64(row, idx_deleted));
    }

    Some(total)
}

/// Samples local `pg_stat_database` counters using a short timeout; returns `None` when Postgres is down.
///
/// # Arguments
/// * `pg_addr` - Local PostgreSQL TCP endpoint (e.g. `127.0.0.1:5432`).
/// * `pg_superuser` - PostgreSQL superuser role for the trust connection.
pub async fn sample_local_activity(pg_addr: &str, pg_superuser: &str) -> Option<PgNodeActivity> {
    let res = tokio::time::timeout(
        ACTIVITY_QUERY_TIMEOUT,
        PgConnection::query_simple(
            pg_addr,
            Some(pg_superuser),
            Some("postgres"),
            PG_STAT_DATABASE_SQL,
            100,
        ),
    )
    .await
    .ok()?
    .ok()?;

    sum_pg_stat_database_rows(&res.columns, &res.rows)
}
