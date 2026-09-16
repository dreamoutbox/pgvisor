mod auth;
mod handlers;
mod server;
mod state;

pub use auth::cluster_auth_middleware;
pub use handlers::{
    handle_cancel_restore, handle_demote, handle_events, handle_fence, handle_prepare_restore,
    handle_promote, handle_repoint, handle_restart, handle_restore, handle_resync, handle_start,
    handle_status, handle_stop, start_postgres_safely,
};
pub use server::{build_control_router, spawn_control_server};
pub use state::{
    EventsQuery, RepointPayload, RestorePayload, ResyncPayload, SidecarEventRecord, SidecarState,
    StatusResponse,
};
