mod auth_handlers;
mod client_dashboard_state;
#[cfg(test)]
mod client_test;
mod error;
mod page_handlers;
mod proxy_handlers;
mod router;
mod sidecar_client;
mod sidecar_response;

pub use client_dashboard_state::ClientDashboardState;
pub use error::SidecarClientError;
pub use router::{create_client_router, resolve_listen_addr, DASHBOARD_LISTEN_ENV};
pub use sidecar_client::{
    parse_sidecar_urls, SidecarClient, DEFAULT_SIDECAR_API_URL, SIDECAR_URLS_ENV,
};
pub use sidecar_response::SidecarResponse;
