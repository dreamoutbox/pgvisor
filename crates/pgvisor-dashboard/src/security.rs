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

/// Validates SQL queries before execution.
pub struct SqlSecurityGuard {
    pub max_execution_timeout: Duration,
    pub max_result_rows: usize,
    pub allow_mutations: bool,
}

impl Default for SqlSecurityGuard {
    fn default() -> Self {
        Self {
            max_execution_timeout: Duration::from_secs(10),
            max_result_rows: 500,
            allow_mutations: true,
        }
    }
}

impl SqlSecurityGuard {
    pub fn new(timeout: Duration, max_rows: usize) -> Self {
        Self {
            max_execution_timeout: timeout,
            max_result_rows: max_rows,
            allow_mutations: true,
        }
    }

    pub fn new_read_only(timeout: Duration, max_rows: usize) -> Self {
        Self {
            max_execution_timeout: timeout,
            max_result_rows: max_rows,
            allow_mutations: false,
        }
    }

    /// Validates SQL query according to security settings.
    pub fn validate_sql(&self, raw_sql: &str) -> Result<String, SecurityError> {
        let cleaned = Self::strip_comments(raw_sql);
        let trimmed = cleaned.trim();

        if trimmed.is_empty() {
            return Err(SecurityError::EmptyQuery);
        }

        if !self.allow_mutations {
            return self.validate_read_only(raw_sql);
        }

        Ok(trimmed.to_string())
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
        let first_word = upper.split_whitespace().next().unwrap_or_default();

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
            "INSERT ",
            "UPDATE ",
            "DELETE ",
            "DROP ",
            "ALTER ",
            "CREATE ",
            "TRUNCATE ",
            "GRANT ",
            "REVOKE ",
            "VACUUM ",
            "CALL ",
            "DO ",
            "COPY ",
            "INTO ",
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

    /// Validates DDL statements for user and permission management.
    /// Strictly permits only CREATE ROLE/USER, DROP ROLE/USER, ALTER ROLE/USER,
    /// GRANT, and REVOKE statements.
    pub fn validate_user_management_ddl(&self, raw_sql: &str) -> Result<String, SecurityError> {
        let cleaned = Self::strip_comments(raw_sql);
        let trimmed = cleaned.trim();

        if trimmed.is_empty() {
            return Err(SecurityError::EmptyQuery);
        }

        if !self.allow_mutations {
            return Err(SecurityError::MutationForbidden(
                "User management DDL is forbidden in read-only mode".into(),
            ));
        }

        let without_trailing_semicolon = trimmed.trim_end_matches(';').trim();
        if without_trailing_semicolon.contains(';') {
            return Err(SecurityError::MultiStatementForbidden(
                "Multiple statements separated by semicolon are disallowed".into(),
            ));
        }

        let upper = without_trailing_semicolon.to_uppercase();
        let first_word = upper.split_whitespace().next().unwrap_or_default();
        let second_word = upper.split_whitespace().nth(1).unwrap_or_default();

        let is_valid_role_ddl = match first_word {
            "CREATE" => second_word == "ROLE" || second_word == "USER",
            "DROP" => second_word == "ROLE" || second_word == "USER",
            "ALTER" => second_word == "ROLE" || second_word == "USER",
            "GRANT" => upper.contains(" TO "),
            "REVOKE" => upper.contains(" FROM "),
            _ => false,
        };

        if !is_valid_role_ddl {
            return Err(SecurityError::MutationForbidden(format!(
                "Statement is not an allowed user management DDL: '{}'",
                first_word
            )));
        }

        // Safeguard against nested hazardous DDL or injection
        let forbidden = [
            "DROP TABLE",
            "DROP DATABASE",
            "DROP SCHEMA",
            "TRUNCATE ",
            "DELETE FROM",
            "UPDATE ",
            "INSERT INTO",
            "COPY ",
            "EXECUTE ",
            "DO $$",
        ];
        for kw in &forbidden {
            if upper.contains(kw) {
                return Err(SecurityError::MutationForbidden(format!(
                    "Forbidden keyword '{}' detected in user management DDL",
                    kw
                )));
            }
        }

        Ok(without_trailing_semicolon.to_string())
    }

    /// Verifies admin authentication token.
    pub fn verify_admin_token(
        auth_header: Option<&str>,
        expected_token: &str,
    ) -> Result<(), SecurityError> {
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

    /// Extracts an admin token from HTTP request headers.
    /// Supports Authorization: Bearer <token>, X-Admin-Token: <token>,
    /// and Cookie: pgvisor_token=<token>.
    pub fn extract_token_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
        // 1. Authorization: Bearer <token>
        if let Some(auth_val) = headers.get(axum::http::header::AUTHORIZATION) {
            if let Ok(auth_str) = auth_val.to_str() {
                let trimmed = auth_str.trim();
                if let Some(bearer) = trimmed.strip_prefix("Bearer ") {
                    let token = bearer.trim();
                    if !token.is_empty() {
                        return Some(token.to_string());
                    }
                } else if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }

        // 2. X-Admin-Token: <token>
        if let Some(custom_val) = headers.get("x-admin-token") {
            if let Ok(custom_str) = custom_val.to_str() {
                let trimmed = custom_str.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }

        // 3. Cookie: pgvisor_token=<token>
        if let Some(cookie_val) = headers.get(axum::http::header::COOKIE) {
            if let Ok(cookie_str) = cookie_val.to_str() {
                for pair in cookie_str.split(';') {
                    let mut parts = pair.splitn(2, '=');
                    if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                        if k.trim() == "pgvisor_token" {
                            let val = v.trim();
                            if !val.is_empty() {
                                return Some(val.to_string());
                            }
                        }
                    }
                }
            }
        }

        None
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
        assert!(guard
            .validate_read_only("EXPLAIN ANALYZE SELECT * FROM pg_stat_activity")
            .is_ok());
        assert!(guard
            .validate_read_only("-- comment\nSELECT id, name FROM users")
            .is_ok());
        assert!(guard
            .validate_read_only("/* block comment */ SELECT version()")
            .is_ok());
        assert!(guard
            .validate_read_only("WITH cte AS (SELECT 1 AS val) SELECT * FROM cte")
            .is_ok());
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
    fn test_validate_sql_with_mutations_and_multi_statements() {
        let guard = SqlSecurityGuard::default();
        assert!(guard.allow_mutations);

        // Single CREATE / DROP allowed
        assert!(guard.validate_sql("CREATE TABLE foo (id int);").is_ok());
        assert!(guard.validate_sql("DROP TABLE IF EXISTS foo;").is_ok());

        // Multi-statement allowed
        assert!(guard
            .validate_sql("DROP TABLE IF EXISTS foo; CREATE TABLE foo (id int);")
            .is_ok());

        // Empty query rejected
        assert_eq!(guard.validate_sql("   "), Err(SecurityError::EmptyQuery));

        // Read-only guard rejects mutating and multi-statements
        let ro_guard = SqlSecurityGuard::new_read_only(Duration::from_secs(5), 100);
        assert!(!ro_guard.allow_mutations);
        assert!(matches!(
            ro_guard.validate_sql("CREATE TABLE foo (id int);"),
            Err(SecurityError::MutationForbidden(_))
        ));
        assert!(matches!(
            ro_guard.validate_sql("DROP TABLE foo;"),
            Err(SecurityError::MutationForbidden(_))
        ));
        assert!(matches!(
            ro_guard.validate_sql("SELECT 1; SELECT 2;"),
            Err(SecurityError::MultiStatementForbidden(_))
        ));
        assert!(ro_guard.validate_sql("SELECT 1;").is_ok());
    }

    #[test]
    fn test_auth_token_verification() {
        let expected = "secret-pgvisor-token";
        assert!(SqlSecurityGuard::verify_admin_token(
            Some("Bearer secret-pgvisor-token"),
            expected
        )
        .is_ok());
        assert!(
            SqlSecurityGuard::verify_admin_token(Some("secret-pgvisor-token"), expected).is_ok()
        );
        assert!(SqlSecurityGuard::verify_admin_token(Some("wrong-token"), expected).is_err());
        assert!(SqlSecurityGuard::verify_admin_token(None, expected).is_err());
    }

    #[test]
    fn test_extract_token_from_headers() {
        use axum::http::header::{AUTHORIZATION, COOKIE};
        use axum::http::HeaderMap;

        // Bearer token
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer my-secret-token".parse().unwrap());
        assert_eq!(
            SqlSecurityGuard::extract_token_from_headers(&headers),
            Some("my-secret-token".to_string())
        );

        // Custom X-Admin-Token
        let mut headers = HeaderMap::new();
        headers.insert("x-admin-token", "custom-token-xyz".parse().unwrap());
        assert_eq!(
            SqlSecurityGuard::extract_token_from_headers(&headers),
            Some("custom-token-xyz".to_string())
        );

        // Cookie pgvisor_token
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            "other_val=123; pgvisor_token=cookie-secret; foo=bar"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            SqlSecurityGuard::extract_token_from_headers(&headers),
            Some("cookie-secret".to_string())
        );

        // Empty / none
        let headers = HeaderMap::new();
        assert_eq!(SqlSecurityGuard::extract_token_from_headers(&headers), None);
    }

    #[test]
    fn test_validate_user_management_ddl() {
        let guard = SqlSecurityGuard::default();

        // Valid user management DDL statements
        assert!(guard
            .validate_user_management_ddl("CREATE ROLE app_user WITH LOGIN PASSWORD 'secret';")
            .is_ok());
        assert!(guard
            .validate_user_management_ddl("ALTER ROLE app_user WITH CREATEDB;")
            .is_ok());
        assert!(guard
            .validate_user_management_ddl("DROP ROLE app_user;")
            .is_ok());
        assert!(guard
            .validate_user_management_ddl("GRANT SELECT, INSERT ON TABLE public.users TO app_user;")
            .is_ok());
        assert!(guard
            .validate_user_management_ddl("REVOKE INSERT ON TABLE public.users FROM app_user;")
            .is_ok());
        assert!(guard
            .validate_user_management_ddl("GRANT admin_group TO app_user;")
            .is_ok());
        assert!(guard
            .validate_user_management_ddl("REVOKE admin_group FROM app_user;")
            .is_ok());

        // Dangerous or forbidden SQL
        assert!(matches!(
            guard.validate_user_management_ddl("DROP TABLE users;"),
            Err(SecurityError::MutationForbidden(_))
        ));
        assert!(matches!(
            guard.validate_user_management_ddl("SELECT * FROM pg_roles;"),
            Err(SecurityError::MutationForbidden(_))
        ));
        assert!(matches!(
            guard.validate_user_management_ddl("INSERT INTO users VALUES (1);"),
            Err(SecurityError::MutationForbidden(_))
        ));
        assert!(matches!(
            guard.validate_user_management_ddl("CREATE ROLE app_user; DROP TABLE users;"),
            Err(SecurityError::MultiStatementForbidden(_))
        ));

        // Read-only mode blocks user management DDL
        let ro_guard = SqlSecurityGuard::new_read_only(Duration::from_secs(5), 100);
        assert!(matches!(
            ro_guard.validate_user_management_ddl("CREATE ROLE app_user;"),
            Err(SecurityError::MutationForbidden(_))
        ));
    }
}
