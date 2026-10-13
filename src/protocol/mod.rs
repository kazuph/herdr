//! Shared wire protocol and presentation encoding code.

pub(crate) mod endpoint;
pub(crate) mod endpoint_jobs;
pub(crate) mod endpoint_projection;
pub(crate) mod endpoint_wire;
pub(crate) mod render_ansi;
pub(crate) mod surface_delta;
pub(crate) mod surface_reuse;
pub(crate) mod surface_scroll;
mod wire;

pub use wire::*;
