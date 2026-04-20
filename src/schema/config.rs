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
    #[serde(default)]
    pub provider_options: HashMap<String, Value>,
}

fn default_timeout() -> f64 { 60.0 }

impl PriestConfig {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            timeout_seconds: 60.0,
            max_output_tokens: None,
            cost_limit: None,
            max_system_chars: None,
            provider_options: HashMap::new(),
        }
    }
}
