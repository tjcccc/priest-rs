use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use super::config::PriestConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriestRequest {
    pub config: PriestConfig,
    #[serde(default = "default_profile")]
    pub profile: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionRef>,
    #[serde(default)]
    pub context: Vec<String>,
    #[serde(default)]
    pub memory: Vec<String>,
    #[serde(default)]
    pub user_context: Vec<String>,
    #[serde(default)]
    pub images: Vec<ImageInput>,
    #[serde(default)]
    pub output: OutputSpec,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

fn default_profile() -> String { "default".into() }

impl PriestRequest {
    pub fn new(config: PriestConfig, prompt: impl Into<String>) -> Self {
        Self {
            config,
            profile: "default".into(),
            prompt: prompt.into(),
            session: None,
            context: vec![],
            memory: vec![],
            user_context: vec![],
            images: vec![],
            output: OutputSpec::default(),
            metadata: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRef {
    pub id: String,
    #[serde(default = "bool_true")]
    pub continue_existing: bool,
    #[serde(default = "bool_true")]
    pub create_if_missing: bool,
}

fn bool_true() -> bool { true }

impl SessionRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), continue_existing: true, create_if_missing: true }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_format: Option<String>,
    /// JSON Schema for structured output.
    /// OpenAI-compat: maps to response_format={"type":"json_schema",...}.
    /// Ollama (v0.5+): maps to format:<schema_dict>.
    /// Anthropic: schema description injected into system message (no native support).
    /// When set, takes precedence over provider_format for the schema-capable path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<Value>,
    /// Schema name passed to OpenAI's json_schema.name field. Defaults to "response".
    #[serde(default = "default_json_schema_name")]
    pub json_schema_name: String,
    /// Maps to OpenAI's json_schema.strict. Requires all properties in required and
    /// additionalProperties:false. Most schemas won't satisfy this. Defaults to false.
    #[serde(default)]
    pub json_schema_strict: bool,
}

fn default_json_schema_name() -> String { "response".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default = "default_media_type")]
    pub media_type: String,
}

fn default_media_type() -> String { "image/jpeg".into() }
