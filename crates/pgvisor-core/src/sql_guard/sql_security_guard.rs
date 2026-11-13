use std::time::Duration;

use super::error::SecurityError;

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
            // UPDATE and TRUNCATE are valid PostgreSQL table privilege names.
            // When the statement verb is GRANT or REVOKE, "UPDATE " or "TRUNCATE "
            // in the body is necessarily a privilege keyword, not a DDL/DML statement
            // — those cases would have been caught above by the first_word check.
            if (*kw == "UPDATE " || *kw == "TRUNCATE ")
                && (first_word == "GRANT" || first_word == "REVOKE")
            {
                continue;
            }
            if upper.contains(kw) {
                return Err(SecurityError::MutationForbidden(format!(
                    "Forbidden keyword '{}' detected in user management DDL",
                    kw
                )));
            }
        }

        Ok(without_trailing_semicolon.to_string())
    }
}
