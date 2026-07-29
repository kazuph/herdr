use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RunStartParams {
    pub caller_pane: String,
    pub label: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub completion: String,
}
