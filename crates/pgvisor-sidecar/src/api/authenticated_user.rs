/// Authenticated PostgreSQL username attached to a request by [`super::api_auth_middleware`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedUser {
    pub username: String,
}
