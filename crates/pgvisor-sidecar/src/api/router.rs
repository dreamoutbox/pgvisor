use axum::middleware;
use axum::routing::{get, post};
use axum::Router;

use super::api_state::ApiState;
use super::auth::{api_auth_middleware, get_me, post_login};

/// Constructs the client-facing `/api/*` Axum router (served on `PGVISOR_API_PORT`).
pub fn build_api_router(state: ApiState) -> Router {
    let public_routes = Router::new().route("/api/auth/login", post(post_login));

    let protected_routes = Router::new()
        .route("/api/auth/me", get(get_me))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            api_auth_middleware,
        ));

    public_routes.merge(protected_routes).with_state(state)
}
