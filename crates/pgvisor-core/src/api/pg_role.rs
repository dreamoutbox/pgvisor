use serde::{Deserialize, Serialize};

/// A PostgreSQL role as returned by pg_roles (excluding system pg_* roles).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgRole {
    pub rolname: String,
    pub rolcanlogin: bool,
    pub rolcreatedb: bool,
    pub rolcreaterole: bool,
    pub rolreplication: bool,
    pub rolsuper: bool,
    pub rolconnlimit: i32,
    pub member_of: Vec<String>,
}
