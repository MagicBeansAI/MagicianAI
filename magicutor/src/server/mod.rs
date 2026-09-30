pub mod bridge;
pub mod cdp_proxy;
pub mod cdp_scope_alias;
pub mod contextual_assist;
pub mod page_signals;
mod routes;
pub mod trace_capture;

pub use routes::configure_routes;
