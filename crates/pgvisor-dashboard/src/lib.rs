pub mod client;
pub mod templates;

pub use client::{
    create_client_router, resolve_listen_addr, ClientDashboardState, SidecarClient,
};
