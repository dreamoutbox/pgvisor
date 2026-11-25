/// Cryptographically random secrets provisioned during `pgvisor setup`.
#[derive(Debug, Clone)]
pub struct GeneratedSecrets {
    /// Superuser PostgreSQL password (`POSTGRES_PASSWORD`).
    pub postgres_password: String,
    /// Shared cluster HMAC secret (`PGVISOR_CLUSTER_SECRET`).
    pub cluster_secret: String,
    /// Dashboard/API session signing secret (`PGVISOR_API_SESSION_SECRET`).
    pub api_session_secret: String,
    /// Bundled RustFS access key (`S3_ACCESS_KEY`), if generated.
    pub s3_access_key: String,
    /// Bundled RustFS secret key (`S3_SECRET_KEY`), if generated.
    pub s3_secret_key: String,
}
