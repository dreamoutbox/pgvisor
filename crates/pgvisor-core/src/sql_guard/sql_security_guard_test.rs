use std::time::Duration;

use super::error::SecurityError;
use super::sql_security_guard::SqlSecurityGuard;

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

    assert!(guard.validate_sql("CREATE TABLE foo (id int);").is_ok());
    assert!(guard.validate_sql("DROP TABLE IF EXISTS foo;").is_ok());

    assert!(guard
        .validate_sql("DROP TABLE IF EXISTS foo; CREATE TABLE foo (id int);")
        .is_ok());

    assert_eq!(guard.validate_sql("   "), Err(SecurityError::EmptyQuery));

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
fn test_validate_user_management_ddl() {
    let guard = SqlSecurityGuard::default();

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

    assert!(guard
        .validate_user_management_ddl(
            "GRANT UPDATE ON TABLE public.\"pgvisor_demo\" TO \"app_user\";",
        )
        .is_ok());
    assert!(guard
        .validate_user_management_ddl(
            "GRANT TRUNCATE ON TABLE public.\"pgvisor_demo\" TO \"app_user\";",
        )
        .is_ok());
    assert!(guard
        .validate_user_management_ddl(
            "GRANT SELECT, INSERT, UPDATE, DELETE, TRUNCATE ON TABLE public.users TO app_user;",
        )
        .is_ok());
    assert!(guard
        .validate_user_management_ddl(
            "REVOKE UPDATE ON TABLE public.\"pgvisor_demo\" FROM \"app_user\";",
        )
        .is_ok());
    assert!(guard
        .validate_user_management_ddl(
            "REVOKE TRUNCATE ON TABLE public.\"pgvisor_demo\" FROM \"app_user\";",
        )
        .is_ok());

    assert!(matches!(
        guard.validate_user_management_ddl(
            "GRANT SELECT ON TABLE foo TO bar; UPDATE users SET pass='x';",
        ),
        Err(SecurityError::MultiStatementForbidden(_))
    ));
    assert!(matches!(
        guard.validate_user_management_ddl(
            "GRANT SELECT ON TABLE foo TO bar; TRUNCATE TABLE users;",
        ),
        Err(SecurityError::MultiStatementForbidden(_))
    ));

    let ro_guard = SqlSecurityGuard::new_read_only(Duration::from_secs(5), 100);
    assert!(matches!(
        ro_guard.validate_user_management_ddl("CREATE ROLE app_user;"),
        Err(SecurityError::MutationForbidden(_))
    ));
}
