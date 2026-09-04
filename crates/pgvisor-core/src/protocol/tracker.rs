use crate::protocol::message::TransactionStatus;

/// Classification of SQL queries for routing and pooling decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    Read,
    Write,
    Begin,
    Commit,
    Rollback,
    Set,
    Other,
}

/// Tracks client transaction state across ReadyForQuery ('Z') messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionTracker {
    status: TransactionStatus,
    in_manual_tx: bool,
}

impl Default for TransactionTracker {
    fn default() -> Self {
        Self {
            status: TransactionStatus::Idle,
            in_manual_tx: false,
        }
    }
}

impl TransactionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current transaction state reported by backend.
    pub fn status(&self) -> TransactionStatus {
        self.status
    }

    /// Returns true if client is not in a transaction block.
    pub fn is_idle(&self) -> bool {
        self.status == TransactionStatus::Idle
    }

    /// Returns true if client is in an active transaction block.
    pub fn in_transaction(&self) -> bool {
        self.status == TransactionStatus::Transaction
    }

    /// Returns true if client is in an error state awaiting rollback.
    pub fn in_error(&self) -> bool {
        self.status == TransactionStatus::Error
    }

    /// Updates tracker state when backend emits ReadyForQuery ('Z').
    pub fn on_ready_for_query(&mut self, status: TransactionStatus) {
        self.status = status;
        if status == TransactionStatus::Idle {
            self.in_manual_tx = false;
        }
    }

    /// Classifies an incoming SQL statement.
    pub fn classify_query(sql: &str) -> QueryKind {
        let trimmed = sql.trim_start();
        let first_word = trimmed
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();

        match first_word.as_str() {
            "SELECT" | "SHOW" | "EXPLAIN" => QueryKind::Read,
            "INSERT" | "UPDATE" | "DELETE" | "CREATE" | "DROP" | "ALTER" | "TRUNCATE" => {
                QueryKind::Write
            }
            "BEGIN" | "START" => QueryKind::Begin,
            "COMMIT" | "END" => QueryKind::Commit,
            "ROLLBACK" | "ABORT" => QueryKind::Rollback,
            "SET" | "RESET" => QueryKind::Set,
            _ => QueryKind::Other,
        }
    }

    /// Determines if a statement or current session state requires routing to the Raft Leader.
    pub fn requires_leader(&self, sql: &str) -> bool {
        // If already inside a transaction, all operations must remain on the current connection.
        if !self.is_idle() {
            return true;
        }

        let kind = Self::classify_query(sql);
        match kind {
            QueryKind::Read => false,
            QueryKind::Write | QueryKind::Begin | QueryKind::Other | QueryKind::Set => true,
            QueryKind::Commit | QueryKind::Rollback => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_queries() {
        assert_eq!(
            TransactionTracker::classify_query("SELECT * FROM users"),
            QueryKind::Read
        );
        assert_eq!(
            TransactionTracker::classify_query("  insert into t values (1)"),
            QueryKind::Write
        );
        assert_eq!(
            TransactionTracker::classify_query("BEGIN TRANSACTION"),
            QueryKind::Begin
        );
        assert_eq!(
            TransactionTracker::classify_query("commit"),
            QueryKind::Commit
        );
        assert_eq!(
            TransactionTracker::classify_query("rollback;"),
            QueryKind::Rollback
        );
    }

    #[test]
    fn test_routing_decisions() {
        let mut tracker = TransactionTracker::new();
        assert!(!tracker.requires_leader("SELECT 1"));
        assert!(tracker.requires_leader("INSERT INTO t VALUES (1)"));
        assert!(tracker.requires_leader("BEGIN"));

        // Once in a transaction, even SELECT must go to the established connection
        tracker.on_ready_for_query(TransactionStatus::Transaction);
        assert!(tracker.requires_leader("SELECT 1"));

        // When transaction commits, return to idle
        tracker.on_ready_for_query(TransactionStatus::Idle);
        assert!(!tracker.requires_leader("SELECT 1"));
    }
}
