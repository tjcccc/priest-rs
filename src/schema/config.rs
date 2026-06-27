use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriestConfig {
    pub provider: String,
    pub model: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_limit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_system_chars: Option<usize>,
    /// Conversation compaction budget (spec 2.5.0). When set, a chat turn whose
    /// reported input usage crosses 80% of this budget triggers compaction.
    /// None = compaction off (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u32>,
    /// Most-recent turns kept verbatim when compacting (spec 2.5.0). Default 6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_keep_turns: Option<usize>,
    /// Hard cap on how many recent session turns are replayed (spec 2.6.0).
    /// 0 replays none (summary only); None replays all (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_context_turns: Option<usize>,
    #[serde(default)]
    pub provider_options: HashMap<String, Value>,
}

fn default_timeout() -> f64 {
    60.0
}

impl PriestConfig {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            timeout_seconds: 60.0,
            max_output_tokens: None,
            cost_limit: None,
            max_system_chars: None,
            max_context_tokens: None,
            compaction_keep_turns: None,
            session_context_turns: None,
            provider_options: HashMap::new(),
        }
    }
}
