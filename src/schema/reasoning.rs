use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider-neutral reasoning effort. Support is provider- and model-specific.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl ReasoningEffort {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Requested provider behavior for displayable reasoning summaries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningSummaryMode {
    None,
    Auto,
}

/// Optional provider-neutral reasoning request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReasoningConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummaryMode>,
}

/// Provider-owned state replayed unchanged only to a recognizing adapter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpaqueReasoningState {
    pub format: String,
    pub value: Value,
}

/// Safe provider-supplied reasoning information.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReasoningInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Vec<OpaqueReasoningState>>,
}
