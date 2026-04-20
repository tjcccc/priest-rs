use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriestResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub execution: ExecutionInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PriestErrorModel>,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

impl PriestResponse {
    pub fn ok(&self) -> bool {
        self.error.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionInfo {
    pub provider: String,
    pub model: String,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
}

impl UsageInfo {
    pub fn new(input: Option<u32>, output: Option<u32>) -> Self {
        let total = match (input, output) {
            (Some(i), Some(o)) => Some(i + o),
            _ => None,
        };
        Self { input_tokens: input, output_tokens: output, total_tokens: total, estimated_cost_usd: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    #[serde(default)]
    pub is_new: bool,
    #[serde(default)]
    pub turn_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriestErrorModel {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub details: HashMap<String, String>,
}

impl PriestErrorModel {
    pub fn from_priest_error(e: &crate::errors::PriestError) -> Self {
        Self {
            code: e.code().to_string(),
            message: e.to_string(),
            details: e.details(),
        }
    }
}
