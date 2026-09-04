use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SecurityError {
    #[error("Mutating SQL statement forbidden in dashboard read-only console: {0}")]
    MutationForbidden(String),

    #[error("Multi-statement SQL queries are forbidden: {0}")]
    MultiStatementForbidden(String),

    #[error("Empty SQL query")]
    EmptyQuery,

    #[error("Query execution timed out after {0:?}")]
    ExecutionTimeout(Duration),

    #[error("Unauthorized: invalid or missing admin token")]
    Unauthorized,
}

/// Validates SQL queries before execution to enforce read-only safety.
pub struct SqlSecurityGuard {
    pub max_execution_timeout: Duration,
    pub max_result_rows: usize,
}

impl Default for SqlSecurityGuard {
    fn default() -> Self {
        Self {
            max_execution_timeout: Duration::from_secs(5),
            max_result_rows: 500,
        }
    }
}

impl SqlSecurityGuard {
    pub fn new(timeout: Duration, max_rows: usize) -> Self {
        Self {
            max_execution_timeout: timeout,
            max_result_rows: max_rows,
        }
    }

    /// Strips comments (`--` single line and `/* ... */` block comments) from SQL.
    pub fn strip_comments(sql: &str) -> String {
        let mut cleaned = String::with_capacity(sql.len());
        let mut in_single_comment = false;
        let mut in_block_comment = false;
        let chars: Vec<char> = sql.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            if in_single_comment {
                if chars[i] == '\n' {
                    in_single_comment = false;
                    cleaned.push(' ');
                }
                i += 1;
            } else if in_block_comment {
                if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    in_block_comment = false;
                    i += 2;
                    cleaned.push(' ');
                } else {
                    i += 1;
                }
            } else if chars[i] == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
                in_single_comment = true;
                i += 2;
            } else if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
                in_block_comment = true;
                i += 2;
            } else {
                cleaned.push(chars[i]);
                i += 1;
            }
        }

        cleaned
    }

    /// Validates that an SQL string is strictly a single, read-only query.
    pub fn validate_read_only(&self, raw_sql: &str) -> Result<String, SecurityError> {
        let cleaned = Self::strip_comments(raw_sql);
        let trimmed = cleaned.trim();

        if trimmed.is_empty() {
            return Err(SecurityError::EmptyQuery);
        }

        // Check for multi-statement queries separated by semicolons
        let without_trailing_semicolon = trimmed.trim_end_matches(';').trim();
        if without_trailing_semicolon.contains(';') {
            return Err(SecurityError::MultiStatementForbidden(
                "Multiple statements separated by semicolon are disallowed".into(),
            ));
        }

        let upper = without_trailing_semicolon.to_uppercase();
        let first_word = upper
            .split_whitespace()
            .next()
            .unwrap_or_default();

        // Strictly allow only read-only statements
        let allowed_prefixes = ["SELECT", "SHOW", "EXPLAIN", "WITH"];
        if !allowed_prefixes.contains(&first_word) {
            return Err(SecurityError::MutationForbidden(format!(
                "Statement beginning with '{}' is not permitted",
                first_word
            )));
        }

        // Extra guard: check forbidden mutation keywords across statement
        let forbidden_keywords = [
            "INSERT ", "UPDATE ", "DELETE ", "DROP ", "ALTER ", "CREATE ", "TRUNCATE ",
            "GRANT ", "REVOKE ", "VACUUM ", "CALL ", "DO ", "COPY ", "INTO ",
        ];

        for kw in &forbidden_keywords {
            if upper.contains(kw) {
                // If it contains "INTO ", make sure it's not "SELECT ... INTO"
                if *kw == "INTO " && upper.contains("SELECT") && upper.contains("INTO") {
                    return Err(SecurityError::MutationForbidden(
                        "SELECT INTO is forbidden (creates a table)".into(),
                    ));
                } else if *kw != "INTO " {
                    return Err(SecurityError::MutationForbidden(format!(
                        "Forbidden mutating keyword '{}' detected",
                        kw.trim()
                    )));
                }
            }
        }

        Ok(without_trailing_semicolon.to_string())
    }

    /// Verifies admin authentication token.
    pub fn verify_admin_token(auth_header: Option<&str>, expected_token: &str) -> Result<(), SecurityError> {
        match auth_header {
            Some(header) => {
                let token = if header.starts_with("Bearer ") {
                    &header[7..]
                } else {
                    header
                };
                if token.trim() == expected_token.trim() {
                    Ok(())
                } else {
                    Err(SecurityError::Unauthorized)
                }
            }
            None => Err(SecurityError::Unauthorized),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_read_only_queries() {
        let guard = SqlSecurityGuard::default();

        assert!(guard.validate_read_only("SELECT 1;").is_ok());
        assert!(guard.validate_read_only("SHOW max_connections;").is_ok());
        assert!(guard.validate_read_only("EXPLAIN ANALYZE SELECT * FROM pg_stat_activity").is_ok());
        assert!(guard.validate_read_only("-- comment\nSELECT id, name FROM users").is_ok());
        assert!(guard.validate_read_only("/* block comment */ SELECT version()").is_ok());
        assert!(guard.validate_read_only("WITH cte AS (SELECT 1 AS val) SELECT * FROM cte").is_ok());
    }

    #[test]
    fn test_mutation_queries_rejected() {
        let guard = SqlSecurityGuard::default();

        assert!(matches!(
            guard.validate_read_only("DROP TABLE users;"),
            Err(SecurityError::MutationForbidden(_))
        ));
        assert!(matches!(
            guard.validate_read_only("DELETE FROM users WHERE id = 1;"),
            Err(SecurityError::MutationForbidden(_))
        ));
        assert!(matches!(
            guard.validate_read_only("INSERT INTO users VALUES (1);"),
            Err(SecurityError::MutationForbidden(_))
        ));
        assert!(matches!(
            guard.validate_read_only("UPDATE users SET name = 'foo';"),
            Err(SecurityError::MutationForbidden(_))
        ));
        assert!(matches!(
            guard.validate_read_only("TRUNCATE TABLE logs;"),
            Err(SecurityError::MutationForbidden(_))
        ));
        assert!(matches!(
            guard.validate_read_only("SELECT * INTO new_table FROM old_table;"),
            Err(SecurityError::MutationForbidden(_))
        ));
    }

    #[test]
    fn test_multi_statement_injection_rejected() {
        let guard = SqlSecurityGuard::default();

        assert!(matches!(
            guard.validate_read_only("SELECT 1; DROP TABLE users;"),
            Err(SecurityError::MultiStatementForbidden(_))
        ));
    }

    #[test]
    fn test_auth_token_verification() {
        let expected = "secret-pgvisor-token";
        assert!(SqlSecurityGuard::verify_admin_token(Some("Bearer secret-pgvisor-token"), expected).is_ok());
        assert!(SqlSecurityGuard::verify_admin_token(Some("secret-pgvisor-token"), expected).is_ok());
        assert!(SqlSecurityGuard::verify_admin_token(Some("wrong-token"), expected).is_err());
        assert!(SqlSecurityGuard::verify_admin_token(None, expected).is_err());
    }
}
