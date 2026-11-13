use std::sync::Arc;

use pgvisor_core::audit::AuditLog;

use super::password_verifier::{PasswordChecker, PostgresPasswordChecker};
use super::session_token::SessionTokenSigner;
use super::superusers::SuperuserAllowlist;
use crate::control::SidecarState;

/// Shared application state for the client-facing `/api/*` Axum router.
#[derive(Clone)]
pub struct ApiState {
    pub sidecar: SidecarState,
    pub pg_addr: String,
    pub pg_superuser: String,
    pub superusers: Arc<SuperuserAllowlist>,
    pub token_signer: Arc<SessionTokenSigner>,
    pub password_checker: Arc<dyn PasswordChecker>,
    pub audit_log: Arc<AuditLog>,
}

impl ApiState {
    /// Constructs [`ApiState`] using the real [`PostgresPasswordChecker`] against `pg_addr`.
    pub fn new(
        sidecar: SidecarState,
        pg_addr: impl Into<String>,
        pg_superuser: impl Into<String>,
        superusers: Arc<SuperuserAllowlist>,
        token_signer: Arc<SessionTokenSigner>,
        audit_log: Arc<AuditLog>,
    ) -> Self {
        let pg_addr = pg_addr.into();
        let pg_superuser = pg_superuser.into();
        let password_checker = Arc::new(PostgresPasswordChecker::new(
            pg_addr.clone(),
            pg_superuser.clone(),
        ));
        Self {
            sidecar,
            pg_addr,
            pg_superuser,
            superusers,
            token_signer,
            password_checker,
            audit_log,
        }
    }

    /// Overrides the password checker implementation (for unit tests).
    pub fn with_password_checker(mut self, checker: Arc<dyn PasswordChecker>) -> Self {
        self.password_checker = checker;
        self
    }
}
