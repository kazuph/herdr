mod id;
mod runtime;
mod runtime_registry;
pub mod state;
mod title;

pub use id::TerminalId;
pub use runtime::TerminalRuntime;
pub(crate) use runtime_registry::TerminalRuntimeRegistry;
pub(crate) use state::AGENT_ACTIVITY_STALE_AFTER;
pub use state::{
    AgentMetadataReport, EffectivePresentation, EffectiveStateChange, TerminalState,
    TerminalStateMutation,
};
pub(crate) use title::stripped_terminal_title;
