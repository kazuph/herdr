use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    CommandGuard,
    ToolPermission,
    AgentPrompt,
    Ask,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOptionRole {
    Approve,
    Reject,
    #[default]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Pending,
    Answered,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionOption {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub role: DecisionOptionRole,
    /// Per-option detail text (Claude Code AskUserQuestion description rows,
    /// `--option-desc` on `decision ask`). Absent on older wire payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Monospace preview the prompt drew for this option (Claude Code
    /// AskUserQuestion option previews — drafts, diffs, diagrams). Claude
    /// Code only paints the focused option's preview, so a screen capture
    /// can carry at most one real preview per frame. Absent on older wire
    /// payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// The prompt indicated this option has a preview but it was not on
    /// screen when the decision was captured (Claude Code paints only the
    /// focused option's preview). Renders as "プレビューあり（画面で確認）".
    /// Absent on older wire payloads.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preview_unavailable: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_id: Option<String>,
    /// All options chosen for a `multi_select` decision (Claude Code
    /// AskUserQuestion multiSelect answers). Absent on older wire payloads;
    /// single-choice answers keep using `option_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responder: Option<String>,
    pub answered_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Decision {
    pub decision_id: String,
    pub kind: DecisionKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub options: Vec<DecisionOption>,
    pub allow_text: bool,
    /// Multiple options may be chosen (Claude Code AskUserQuestion
    /// multiSelect): the answer carries `option_ids`. Absent on older wire
    /// payloads (treated as single-choice).
    #[serde(default)]
    pub multi_select: bool,
    /// Short label plus progress for multi-question prompts (Claude Code
    /// AskUserQuestion tab header: "質問 1/3 果物", "最終確認"). Shown near
    /// the dialog header. Absent on older wire payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<DecisionOrigin>,
    pub created_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_unix_ms: Option<u64>,
    pub status: DecisionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<DecisionAnswer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionCreateParams {
    pub kind: DecisionKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub options: Vec<DecisionOption>,
    #[serde(default)]
    pub allow_text: bool,
    /// The decision accepts several options at once (see
    /// `Decision::multi_select`). Absent on older wire payloads.
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<DecisionOrigin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionGetParams {
    pub decision_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<DecisionStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionWaitParams {
    pub decision_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionAnswerParams {
    pub decision_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_id: Option<String>,
    /// Multiple choices for a `multi_select` decision. Absent on older wire
    /// payloads; `option_id` remains the single-choice field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionCancelParams {
    pub decision_id: String,
}
