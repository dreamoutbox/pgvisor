use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use opendal::Operator;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::warn;

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("OpenDAL storage error: {0}")]
    Storage(#[from] opendal::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Closed set of auditable cluster and proxy events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventKind {
    NodeUp,
    NodeDown,
    BackupCreated,
    BackupRestored,
    DangerousSql,
    ElectionResult,
    NodeJoined,
    NodeLeft,
    UserPermission,
}

impl AuditEventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NodeUp => "node_up",
            Self::NodeDown => "node_down",
            Self::BackupCreated => "backup_created",
            Self::BackupRestored => "backup_restored",
            Self::DangerousSql => "dangerous_sql",
            Self::ElectionResult => "election_result",
            Self::NodeJoined => "node_joined",
            Self::NodeLeft => "node_left",
            Self::UserPermission => "user_permission",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::NodeUp => "Node Online",
            Self::NodeDown => "Node Offline",
            Self::BackupCreated => "Backup Created",
            Self::BackupRestored => "Backup Restored",
            Self::DangerousSql => "Dangerous SQL",
            Self::ElectionResult => "Election Result",
            Self::NodeJoined => "Worker Joined",
            Self::NodeLeft => "Worker Left",
            Self::UserPermission => "User & Role",
        }
    }

    pub fn from_snake_case(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "node_up" => Some(Self::NodeUp),
            "node_down" => Some(Self::NodeDown),
            "backup_created" => Some(Self::BackupCreated),
            "backup_restored" => Some(Self::BackupRestored),
            "dangerous_sql" => Some(Self::DangerousSql),
            "election_result" => Some(Self::ElectionResult),
            "node_joined" => Some(Self::NodeJoined),
            "node_left" => Some(Self::NodeLeft),
            "user_permission" => Some(Self::UserPermission),
            _ => None,
        }
    }
}

/// A structured cluster audit log entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEvent {
    pub id: u64,
    pub occurred_at: DateTime<Utc>,
    pub kind: AuditEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_address: Option<String>,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitr_target: Option<String>,
}

/// Central in-memory audit log store with S3/OpenDAL persistence.
pub struct AuditLog {
    cluster_name: String,
    operator: Option<Operator>,
    events: Arc<RwLock<VecDeque<AuditEvent>>>,
    next_id: AtomicU64,
    capacity: usize,
}

impl AuditLog {
    /// Creates a new audit log store with an optional OpenDAL storage operator and capacity limit.
    pub fn new(
        cluster_name: impl Into<String>,
        operator: Option<Operator>,
        capacity: usize,
    ) -> Self {
        Self {
            cluster_name: cluster_name.into(),
            operator,
            events: Arc::new(RwLock::new(VecDeque::with_capacity(capacity.min(1000)))),
            next_id: AtomicU64::new(1),
            capacity,
        }
    }

    /// Appends a new event to the in-memory ring buffer and persists it to S3 asynchronously.
    pub async fn append(
        &self,
        kind: AuditEventKind,
        node_id: Option<u64>,
        node_address: Option<&str>,
        detail: impl Into<String>,
        pitr_target: Option<String>,
    ) -> AuditEvent {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let event = AuditEvent {
            id,
            occurred_at: Utc::now(),
            kind,
            node_id,
            node_address: node_address.map(|s| s.to_string()),
            detail: detail.into(),
            pitr_target,
        };

        {
            let mut ring = self.events.write().await;
            if ring.len() >= self.capacity {
                ring.pop_front();
            }
            ring.push_back(event.clone()); // Cloning event to store in deque and return copy
        }

        if let Some(op) = self.operator.as_ref() {
            // Operator is cheap Arc clone internally in OpenDAL
            let op = op.clone();
            let key = format!("clusters/{}/audit/{:020}.json", self.cluster_name, id);
            let payload = serde_json::to_vec(&event);

            tokio::spawn(async move {
                match payload {
                    Ok(bytes) => {
                        if let Err(err) = op.write(&key, bytes).await {
                            warn!(?err, %key, "Failed to persist audit event to S3");
                        }
                    }
                    Err(err) => {
                        warn!(?err, "Failed to serialize audit event for S3");
                    }
                }
            });
        }

        event
    }

    /// Loads historical audit events from S3 on startup to restore the in-memory cache.
    pub async fn load_from_storage(&self) -> Result<usize, AuditError> {
        let Some(op) = self.operator.as_ref() else {
            return Ok(0);
        };

        let prefix = format!("clusters/{}/audit/", self.cluster_name);
        let entries = match op.list(&prefix).await {
            Ok(e) => e,
            Err(e) => {
                warn!(?e, "Could not list S3 audit log prefix");
                return Ok(0);
            }
        };

        let mut paths: Vec<String> = entries
            .into_iter()
            .map(|e| e.path().to_string())
            .filter(|p| p.ends_with(".json"))
            .collect();

        paths.sort();

        // Only read up to capacity latest items to keep startup memory bound
        let start_idx = if paths.len() > self.capacity {
            paths.len() - self.capacity
        } else {
            0
        };

        let mut loaded = Vec::new();
        let mut max_id = 0u64;

        for path in &paths[start_idx..] {
            if let Ok(data) = op.read(path).await {
                if let Ok(event) = serde_json::from_slice::<AuditEvent>(&data.to_vec()) {
                    if event.id > max_id {
                        max_id = event.id;
                    }
                    loaded.push(event);
                }
            }
        }

        let count = loaded.len();
        if !loaded.is_empty() {
            let mut ring = self.events.write().await;
            ring.clear();
            for ev in loaded {
                ring.push_back(ev);
            }
            self.next_id.store(max_id + 1, Ordering::SeqCst);
        }

        Ok(count)
    }

    /// Lists audit events sorted newest-first, with optional kind filtering, search, and pagination.
    pub async fn list(
        &self,
        limit: usize,
        offset: usize,
        kind: Option<AuditEventKind>,
        search: Option<&str>,
    ) -> (Vec<AuditEvent>, usize) {
        let ring = self.events.read().await;
        let search_lower = search.map(|s| s.trim().to_lowercase());

        let matches: Vec<&AuditEvent> = ring
            .iter()
            .rev()
            .filter(|e| {
                if let Some(target_kind) = kind {
                    if e.kind != target_kind {
                        return false;
                    }
                }
                if let Some(q) = search_lower.as_deref() {
                    if !q.is_empty() {
                        let matches_detail = e.detail.to_lowercase().contains(q);
                        let matches_kind = e.kind.as_str().contains(q);
                        let matches_addr = e
                            .node_address
                            .as_ref()
                            .map(|a| a.to_lowercase().contains(q))
                            .unwrap_or(false);
                        if !matches_detail && !matches_kind && !matches_addr {
                            return false;
                        }
                    }
                }
                true
            })
            .collect();

        let total = matches.len();
        let paged: Vec<AuditEvent> = matches
            .into_iter()
            .skip(offset)
            .take(limit)
            .cloned() // Cloned for page slice result
            .collect();

        (paged, total)
    }

    /// Aggregates summary counts for dashboard header statistics.
    pub async fn stats(&self) -> (usize, usize, Option<String>) {
        let ring = self.events.read().await;
        let total = ring.len();
        let mut dangerous_sql_count = 0;
        let mut latest_pitr = None;

        for e in ring.iter().rev() {
            if e.kind == AuditEventKind::DangerousSql {
                dangerous_sql_count += 1;
                if latest_pitr.is_none() {
                    latest_pitr = e.pitr_target.clone();
                }
            }
        }

        (total, dangerous_sql_count, latest_pitr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_audit_log_ring_buffer_capacity() {
        let audit = AuditLog::new("test-cluster", None, 3);
        audit
            .append(
                AuditEventKind::NodeUp,
                Some(1),
                Some("127.0.0.1:5432"),
                "Node 1 up",
                None,
            )
            .await;
        audit
            .append(
                AuditEventKind::BackupCreated,
                Some(1),
                None,
                "Backup 1",
                None,
            )
            .await;
        audit
            .append(
                AuditEventKind::DangerousSql,
                Some(1),
                None,
                "DROP TABLE t1",
                Some("2026-09-09 12:00:00 UTC".into()),
            )
            .await;
        audit
            .append(
                AuditEventKind::NodeDown,
                Some(2),
                Some("127.0.0.1:5433"),
                "Node 2 down",
                None,
            )
            .await;

        let (items, total) = audit.list(10, 0, None, None).await;
        assert_eq!(total, 3);
        assert_eq!(items.len(), 3);
        // Newest first
        assert_eq!(items[0].kind, AuditEventKind::NodeDown);
        assert_eq!(items[1].kind, AuditEventKind::DangerousSql);
        assert_eq!(items[2].kind, AuditEventKind::BackupCreated);
    }

    #[tokio::test]
    async fn test_audit_log_filtering_and_search() {
        let audit = AuditLog::new("test-cluster", None, 10);
        audit
            .append(
                AuditEventKind::NodeUp,
                Some(1),
                Some("10.0.0.1:5432"),
                "Node 1 ready",
                None,
            )
            .await;
        audit
            .append(
                AuditEventKind::DangerousSql,
                Some(1),
                None,
                "DROP TABLE users",
                Some("2026-09-09 10:00:00 UTC".into()),
            )
            .await;
        audit
            .append(
                AuditEventKind::DangerousSql,
                Some(1),
                None,
                "DELETE FROM accounts",
                Some("2026-09-09 11:00:00 UTC".into()),
            )
            .await;

        let (sql_items, total_sql) = audit
            .list(10, 0, Some(AuditEventKind::DangerousSql), None)
            .await;
        assert_eq!(total_sql, 2);
        assert_eq!(sql_items.len(), 2);

        let (search_items, total_search) = audit.list(10, 0, None, Some("users")).await;
        assert_eq!(total_search, 1);
        assert_eq!(search_items[0].detail, "DROP TABLE users");

        let (total, dangerous_count, latest_pitr) = audit.stats().await;
        assert_eq!(total, 3);
        assert_eq!(dangerous_count, 2);
        assert_eq!(latest_pitr.as_deref(), Some("2026-09-09 11:00:00 UTC"));
    }
}
