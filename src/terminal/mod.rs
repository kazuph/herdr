mod id;
pub mod state;

pub use id::TerminalId;
pub(crate) use state::stabilize_agent_state;
pub use state::{EffectiveStateChange, PendingAgentRestore, TerminalState};

/// Live runtime for a server-owned terminal.
///
/// The implementation still lives in the pane module during this migration, but
/// all new ownership should address it by `TerminalId` through this terminal
/// noun.
pub type TerminalRuntime = crate::pane::PaneRuntime;
