pub mod autodetect;
#[cfg(unix)]
pub(crate) mod client_accept;
pub(crate) mod client_transport;
pub(crate) mod client_view;
pub(crate) mod clients;
pub(crate) mod clipboard_image;
pub(crate) mod endpoint_input;
pub(crate) mod endpoint_jobs;
pub(crate) mod endpoint_keyboard;
pub(crate) mod endpoint_mouse;
pub(crate) mod endpoint_snapshot;
pub(crate) mod endpoint_surface;
pub(crate) mod endpoint_transport;
#[cfg(unix)]
pub(crate) mod handoff;
pub mod headless;
pub(crate) mod keybindings;
pub(crate) mod notifications;
pub(crate) mod render_stream;
pub mod socket_paths;
pub(crate) mod terminal_attach;
