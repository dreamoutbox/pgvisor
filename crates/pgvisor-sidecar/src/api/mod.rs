mod api_state;
#[cfg(test)]
mod api_test;
mod auth;
mod authenticated_user;
mod error;
mod members;
mod password_verifier;
mod router;
mod server;
mod session_token;
mod superusers;

pub use api_state::ApiState;
pub use auth::{api_auth_middleware, extract_token_from_headers, get_me, post_login};
pub use authenticated_user::AuthenticatedUser;
pub use error::ApiError;
pub use members::{
    build_api_url_from_peer, collect_cluster_members, get_cluster_members, parse_node_id_from_peer,
    require_leader,
};
pub use password_verifier::{
    verify_scram_sha256, PasswordCheckOutcome, PasswordChecker, PostgresPasswordChecker,
};
pub use router::build_api_router;
pub use server::{resolve_sidecar_ports, spawn_api_server};
pub use session_token::{SessionClaims, SessionTokenSigner, DEFAULT_TOKEN_TTL_SECS};
pub use superusers::{SuperuserAllowlist, SUPERUSER_ENV};
pub use crate::control::SidecarRole;
