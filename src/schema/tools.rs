//! Tool calling types (spec 2.4.0, behavior/tool-calling.md).
//!
//! The library transports tool definitions and calls; it never executes
//! tools — execution is the caller's responsibility.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use super::reasoning::ReasoningInfo;

/// A tool the model may call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema object describing the tool's parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

/// Tool selection behavior. Only meaningful when tools are provided.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Tool { name: String },
}

/// A single tool call requested by the model. Providers that do not assign
/// call ids (Ollama) get synthesized ids "call_0", "call_1", ... in order.
/// Arguments are always a parsed JSON object; unparseable provider JSON
/// becomes an empty object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

/// One entry in the turn-local tool loop history. Callers replay the full
/// exchange on each loop iteration via `PriestRequest.tool_exchange`.
/// Exchange turns are never persisted in sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolExchangeTurn {
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        tool_calls: Vec<ToolCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<ReasoningInfo>,
    },
    ToolResult {
        tool_call_id: String,
        name: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Per spec, unparseable or non-object argument JSON becomes an empty object.
pub fn parse_tool_arguments(raw: &str) -> Map<String, Value> {
    if raw.trim().is_empty() {
        return Map::new();
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}
