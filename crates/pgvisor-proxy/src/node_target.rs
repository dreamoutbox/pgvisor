/// Registered PostgreSQL cluster target endpoint polled for topology discovery.
#[derive(Clone, Debug)]
pub struct NodeTarget {
    pub pg_addr: String,
    pub control_url: String,
    pub is_dynamic: bool,
    pub consecutive_failures: usize,
}
