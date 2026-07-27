use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use super::adapter::{
    AdapterCallOptions, AdapterResult, AdapterStreamEvent, ProviderAdapter,
};
use crate::schema::tools::{parse_tool_arguments, ToolCall, ToolChoice};
use crate::context_builder::Message;
use crate::errors::PriestError;
use crate::schema::config::PriestConfig;
use crate::schema::request::OutputSpec;
use crate::schema::reasoning::{
    OpaqueReasoningState, ReasoningEffort, ReasoningInfo, ReasoningSummaryMode,
};

const REASONING_FORMAT: &str = "anthropic.messages.thinking.v1";

pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    client: Client,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.anthropic.com".into(),
            api_key: api_key.into(),
            client: Client::new(),
        }
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            client: Client::new(),
        }
    }
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
    usage: Option<AnthropicUsage>,
    stop_reason: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens_details: Option<AnthropicOutputTokenDetails>,
}

#[derive(Deserialize)]
struct AnthropicOutputTokenDetails {
    thinking_tokens: Option<u32>,
}

fn map_stop(r: Option<&str>) -> Option<String> {
    Some(
        match r? {
            "end_turn" | "stop_sequence" => "stop",
            "max_tokens" => "length",
            "tool_use" => "tool_calls",
            _ => "unknown",
        }
        .to_string(),
    )
}

/// Translate messages to Anthropic wire format. Tool results merge into a
/// user message of tool_result blocks (Anthropic requires alternating roles);
/// assistant tool calls become tool_use content blocks.
fn build_wire_turns(messages: &[Message]) -> Vec<Value> {
    let mut turns: Vec<Value> = vec![];
    let mut pending_tool_results: Vec<Value> = vec![];

    for m in messages.iter().filter(|m| m.role != "system") {
        if m.role == "tool" {
            pending_tool_results.push(json!({
                "type": "tool_result",
                "tool_use_id": m.tool_call_id,
                "content": m.content,
            }));
            continue;
        }
        if !pending_tool_results.is_empty() {
            turns.push(json!({"role": "user", "content": std::mem::take(&mut pending_tool_results)}));
        }
        if m.role == "assistant" && m.tool_calls.as_ref().is_some_and(|c| !c.is_empty()) {
            let mut blocks: Vec<Value> = vec![];
            for state in m
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.continuation.as_ref())
                .into_iter()
                .flatten()
            {
                if state.format == REASONING_FORMAT && state.value.is_object() {
                    blocks.push(state.value.clone());
                }
            }
            if !m.content.is_empty() {
                blocks.push(json!({"type": "text", "text": m.content}));
            }
            for call in m.tool_calls.as_ref().unwrap() {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": call.arguments,
                }));
            }
            turns.push(json!({"role": "assistant", "content": blocks}));
            continue;
        }
        turns.push(json!({"role": m.role, "content": m.content}));
    }
    if !pending_tool_results.is_empty() {
        turns.push(json!({"role": "user", "content": pending_tool_results}));
    }
    turns
}

fn apply_tools(payload: &mut Value, options: Option<&AdapterCallOptions>) {
    let Some(options) = options.filter(|o| !o.tools.is_empty()) else { return };
    let tools: Vec<Value> = options
        .tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters.clone()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            })
        })
        .collect();
    payload["tools"] = json!(tools);
    if let Some(choice) = &options.tool_choice {
        payload["tool_choice"] = match choice {
            ToolChoice::Auto => json!({"type": "auto"}),
            ToolChoice::None => json!({"type": "none"}),
            ToolChoice::Required => json!({"type": "any"}),
            ToolChoice::Tool { name } => json!({"type": "tool", "name": name}),
        };
    }
}

fn parse_tool_use_blocks(content: &[AnthropicContent]) -> Option<Vec<ToolCall>> {
    let mut calls = vec![];
    for (i, block) in content.iter().enumerate() {
        if block.kind != "tool_use" {
            continue;
        }
        let Some(name) = &block.name else { continue };
        let arguments = match &block.input {
            Some(Value::Object(map)) => map.clone(),
            _ => serde_json::Map::new(),
        };
        calls.push(ToolCall {
            id: block.id.clone().unwrap_or_else(|| format!("call_{i}")),
            name: name.clone(),
            arguments,
        });
    }
    if calls.is_empty() { None } else { Some(calls) }
}

fn parse_reasoning(
    content: &[AnthropicContent],
    include_continuation: bool,
) -> Option<ReasoningInfo> {
    let thinking: Vec<&AnthropicContent> = content
        .iter()
        .filter(|block| block.kind == "thinking" || block.kind == "redacted_thinking")
        .collect();
    let summaries: Vec<String> = thinking
        .iter()
        .filter(|block| block.kind == "thinking")
        .filter_map(|block| block.thinking.clone())
        .filter(|summary| !summary.is_empty())
        .collect();
    let continuation: Vec<OpaqueReasoningState> = if include_continuation {
        thinking
            .iter()
            .filter_map(|block| serde_json::to_value(*block).ok())
            .map(|value| OpaqueReasoningState {
                format: REASONING_FORMAT.into(),
                value,
            })
            .collect()
    } else {
        vec![]
    };

    if summaries.is_empty() && continuation.is_empty() {
        None
    } else {
        Some(ReasoningInfo {
            summary: (!summaries.is_empty()).then(|| summaries.join("\n\n")),
            continuation: (!continuation.is_empty()).then_some(continuation),
        })
    }
}

fn apply_reasoning(payload: &mut Value, config: &PriestConfig) {
    let Some(reasoning) = &config.reasoning else {
        return;
    };
    let disabled =
        reasoning.enabled == Some(false) || reasoning.effort == Some(ReasoningEffort::None);
    let needs_thinking =
        reasoning.enabled == Some(true) || reasoning.effort.is_some() || reasoning.summary.is_some();

    if disabled {
        payload["thinking"] = json!({"type": "disabled"});
    } else if needs_thinking {
        let mut thinking = json!({"type": "adaptive"});
        match reasoning.summary {
            Some(ReasoningSummaryMode::Auto) => thinking["display"] = json!("summarized"),
            Some(ReasoningSummaryMode::None) => thinking["display"] = json!("omitted"),
            None => {}
        }
        payload["thinking"] = thinking;
    }
    if let Some(effort) = reasoning.effort.filter(|effort| *effort != ReasoningEffort::None) {
        payload["output_config"] = json!({"effort": effort.as_str()});
    }
}

fn build_payload(
    messages: &[Message],
    config: &PriestConfig,
    output_spec: &OutputSpec,
    options: Option<&AdapterCallOptions>,
    stream: bool,
) -> (Value, Option<String>) {
    let mut system_parts: Vec<String> = messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.clone())
        .collect();
    if let Some(ref schema) = output_spec.json_schema {
        let schema_str = serde_json::to_string_pretty(schema).unwrap_or_default();
        system_parts.push(format!(
            "Respond with a valid JSON object that conforms to the following JSON Schema:\n\n<schema>\n{schema_str}\n</schema>\n\nReturn only the JSON object — no explanation, no markdown fences."
        ));
    }
    let turns = build_wire_turns(messages);
    let system_str: Option<String> = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };
    let max_tokens = config.max_output_tokens.unwrap_or(8096);
    let mut payload = json!({ "model": config.model, "messages": turns, "max_tokens": max_tokens });
    payload["stream"] = json!(stream);
    apply_reasoning(&mut payload, config);
    apply_tools(&mut payload, options);
    if let Some(ref sys) = system_str {
        payload["system"] = json!(sys);
    }
    for (k, v) in &config.provider_options {
        payload[k] = v.clone();
    }
    (payload, system_str)
}

fn provider_error(config: &PriestConfig, msg: impl Into<String>) -> PriestError {
    PriestError::ProviderError {
        provider: config.provider.clone(),
        message: msg.into(),
    }
}

fn sse_frames(body: &str) -> Vec<(String, Value)> {
    body.replace("\r\n", "\n")
        .split("\n\n")
        .filter_map(|frame| {
            let mut event = String::new();
            let mut data = vec![];
            for line in frame.lines() {
                if let Some(value) = line.strip_prefix("event:") {
                    event = value.trim().to_string();
                } else if let Some(value) = line.strip_prefix("data:") {
                    data.push(value.trim_start());
                }
            }
            if data.is_empty() {
                return None;
            }
            serde_json::from_str::<Value>(&data.join("\n"))
                .ok()
                .map(|value| (event, value))
        })
        .collect()
}

#[derive(Default)]
struct StreamingToolCall {
    event_index: usize,
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

fn parse_stream_events(body: &str) -> Vec<Result<AdapterStreamEvent, PriestError>> {
    let mut events = vec![];
    let mut tools: HashMap<u64, StreamingToolCall> = HashMap::new();
    let mut thinking: BTreeMap<u64, Value> = BTreeMap::new();
    let mut tool_count = 0usize;
    let mut stop_reason = None;
    let mut input_tokens = None;
    let mut output_tokens = None;
    let mut cached_input_tokens = None;
    let mut reasoning_tokens = None;

    for (event_name, value) in sse_frames(body) {
        let kind = if event_name.is_empty() {
            value["type"].as_str().unwrap_or("")
        } else {
            event_name.as_str()
        };
        match kind {
            "message_start" => {
                input_tokens = value["message"]["usage"]["input_tokens"].as_u64().map(|v| v as u32);
                cached_input_tokens = value["message"]["usage"]["cache_read_input_tokens"]
                    .as_u64()
                    .map(|v| v as u32);
            }
            "content_block_start" => {
                let Some(index) = value["index"].as_u64() else {
                    continue;
                };
                let block = &value["content_block"];
                match block["type"].as_str() {
                    Some("tool_use") => {
                        let event_index = tool_count;
                        tool_count += 1;
                        let state = StreamingToolCall {
                            event_index,
                            id: block["id"].as_str().map(str::to_string),
                            name: block["name"].as_str().map(str::to_string),
                            arguments: String::new(),
                        };
                        events.push(Ok(AdapterStreamEvent::ToolCallStart {
                            index: event_index,
                            id: state.id.clone(),
                            name: state.name.clone(),
                        }));
                        tools.insert(index, state);
                    }
                    Some("thinking") | Some("redacted_thinking") => {
                        thinking.insert(index, block.clone());
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let delta = &value["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        if let Some(text) = delta["text"].as_str().filter(|text| !text.is_empty()) {
                            events.push(Ok(AdapterStreamEvent::TextDelta {
                                text: text.to_string(),
                            }));
                        }
                    }
                    Some("thinking_delta") => {
                        let Some(index) = value["index"].as_u64() else {
                            continue;
                        };
                        if let Some(text) =
                            delta["thinking"].as_str().filter(|text| !text.is_empty())
                        {
                            if let Some(block) = thinking.get_mut(&index) {
                                let accumulated = block["thinking"].as_str().unwrap_or("");
                                block["thinking"] = json!(format!("{accumulated}{text}"));
                            }
                            events.push(Ok(AdapterStreamEvent::ReasoningSummaryDelta {
                                text: text.to_string(),
                            }));
                        }
                    }
                    Some("signature_delta") => {
                        let Some(index) = value["index"].as_u64() else {
                            continue;
                        };
                        if let Some(signature) = delta["signature"].as_str() {
                            if let Some(block) = thinking.get_mut(&index) {
                                let accumulated = block["signature"].as_str().unwrap_or("");
                                block["signature"] = json!(format!("{accumulated}{signature}"));
                            }
                        }
                    }
                    Some("input_json_delta") => {
                        let Some(index) = value["index"].as_u64() else {
                            continue;
                        };
                        if let (Some(state), Some(fragment)) =
                            (tools.get_mut(&index), delta["partial_json"].as_str())
                        {
                            if !fragment.is_empty() {
                                state.arguments.push_str(fragment);
                                events.push(Ok(AdapterStreamEvent::ToolCallDelta {
                                    index: state.event_index,
                                    arguments_delta: fragment.to_string(),
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let Some(index) = value["index"].as_u64() else {
                    continue;
                };
                if let Some(state) = tools.remove(&index) {
                    events.push(Ok(AdapterStreamEvent::ToolCallEnd {
                        index: state.event_index,
                        tool_call: ToolCall {
                            id: state
                                .id
                                .unwrap_or_else(|| format!("call_{}", state.event_index)),
                            name: state.name.unwrap_or_default(),
                            arguments: parse_tool_arguments(&state.arguments),
                        },
                    }));
                }
            }
            "message_delta" => {
                stop_reason = value["delta"]["stop_reason"].as_str().map(str::to_string);
                output_tokens = value["usage"]["output_tokens"].as_u64().map(|v| v as u32);
                reasoning_tokens = value["usage"]["output_tokens_details"]["thinking_tokens"]
                    .as_u64()
                    .map(|v| v as u32);
            }
            _ => {}
        }
    }

    if input_tokens.is_some()
        || output_tokens.is_some()
        || cached_input_tokens.is_some()
        || reasoning_tokens.is_some()
    {
        events.push(Ok(AdapterStreamEvent::Usage {
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
        }));
    }

    let thinking_blocks: Vec<AnthropicContent> = thinking
        .into_values()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect();
    events.push(Ok(AdapterStreamEvent::Finish {
        finish_reason: if tool_count > 0 {
            Some("tool_calls".into())
        } else {
            map_stop(stop_reason.as_deref())
        },
        reasoning: parse_reasoning(&thinking_blocks, tool_count > 0),
    }));
    events
}

#[async_trait]
impl ProviderAdapter for AnthropicProvider {
    async fn complete(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<AdapterResult, PriestError> {
        let url = format!("{}/v1/messages", self.base_url);
        let (payload, _) = build_payload(messages, config, output_spec, options, false);
        let timeout = Duration::from_secs_f64(config.timeout_seconds);

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&payload)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    PriestError::ProviderTimeout {
                        provider: config.provider.clone(),
                        timeout: config.timeout_seconds,
                    }
                } else {
                    provider_error(config, e.to_string())
                }
            })?;

        if resp.status() == 429 {
            return Err(PriestError::ProviderRateLimited {
                provider: config.provider.clone(),
                retry_after: None,
            });
        }
        if !resp.status().is_success() {
            return Err(provider_error(config, format!("HTTP {}", resp.status())));
        }

        let data: AnthropicResponse = resp
            .json()
            .await
            .map_err(|e| provider_error(config, e.to_string()))?;
        let tool_calls = parse_tool_use_blocks(&data.content);
        let reasoning = parse_reasoning(&data.content, tool_calls.is_some());
        let text = data
            .content
            .iter()
            .filter(|block| block.kind == "text")
            .filter_map(|block| block.text.as_deref())
            .collect::<String>();

        Ok(AdapterResult {
            text,
            finish_reason: if tool_calls.is_some() {
                Some("tool_calls".to_string())
            } else {
                map_stop(data.stop_reason.as_deref())
            },
            input_tokens: data.usage.as_ref().and_then(|u| u.input_tokens),
            output_tokens: data.usage.as_ref().and_then(|u| u.output_tokens),
            cached_input_tokens: data.usage.as_ref().and_then(|u| u.cache_read_input_tokens),
            reasoning_tokens: data
                .usage
                .as_ref()
                .and_then(|u| u.output_tokens_details.as_ref())
                .and_then(|details| details.thinking_tokens),
            tool_calls,
            reasoning,
        })
    }

    async fn stream(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        let url = format!("{}/v1/messages", self.base_url);
        let (payload, _) = build_payload(messages, config, output_spec, options, true);
        let timeout = Duration::from_secs_f64(config.timeout_seconds);
        let provider = config.provider.clone();

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&payload)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    PriestError::ProviderTimeout {
                        provider: provider.clone(),
                        timeout: config.timeout_seconds,
                    }
                } else {
                    PriestError::ProviderError {
                        provider: provider.clone(),
                        message: e.to_string(),
                    }
                }
            })?;

        if !resp.status().is_success() {
            return Err(PriestError::ProviderError {
                provider,
                message: format!("HTTP {}", resp.status()),
            });
        }

        let lines = resp.bytes_stream();
        let provider2 = provider.clone();

        let stream = lines.flat_map(move |chunk: Result<Bytes, reqwest::Error>| {
            let prov = provider2.clone();
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    return futures::stream::iter(vec![Err(PriestError::ProviderError {
                        provider: prov,
                        message: e.to_string(),
                    })])
                }
            };
            let text = String::from_utf8_lossy(&bytes).to_string();
            let mut items = vec![];
            for line in text.lines() {
                let line = line.trim();
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        if v["type"].as_str() == Some("content_block_delta") {
                            if let Some(delta_text) = v["delta"]["text"].as_str() {
                                if !delta_text.is_empty() {
                                    items.push(Ok(delta_text.to_string()));
                                }
                            }
                        }
                    }
                }
            }
            futures::stream::iter(items)
        });

        Ok(Box::pin(stream))
    }

    async fn stream_events(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<AdapterStreamEvent, PriestError>>, PriestError> {
        let url = format!("{}/v1/messages", self.base_url);
        let (payload, _) = build_payload(messages, config, output_spec, options, true);
        let timeout = Duration::from_secs_f64(config.timeout_seconds);

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&payload)
            .timeout(timeout)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    PriestError::ProviderTimeout {
                        provider: config.provider.clone(),
                        timeout: config.timeout_seconds,
                    }
                } else {
                    provider_error(config, error.to_string())
                }
            })?;

        if !resp.status().is_success() {
            return Err(provider_error(config, format!("HTTP {}", resp.status())));
        }
        let body = resp
            .text()
            .await
            .map_err(|error| provider_error(config, error.to_string()))?;
        Ok(Box::pin(futures::stream::iter(parse_stream_events(&body))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::reasoning::{ReasoningConfig, ReasoningEffort, ReasoningSummaryMode};

    #[test]
    fn parses_cache_read_input_tokens() {
        // spec 2.5.0: usage.cache_read_input_tokens → cached_input_tokens.
        let resp: AnthropicResponse = serde_json::from_value(json!({
            "content": [{ "type": "text", "text": "ok" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 1200, "output_tokens": 40, "cache_read_input_tokens": 1024 }
        })).unwrap();
        assert_eq!(resp.usage.as_ref().and_then(|u| u.cache_read_input_tokens), Some(1024));

        // None when omitted.
        let resp2: AnthropicResponse = serde_json::from_value(json!({
            "content": [{ "type": "text", "text": "ok" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 1200, "output_tokens": 40 }
        })).unwrap();
        assert_eq!(resp2.usage.unwrap().cache_read_input_tokens, None);
    }

    #[test]
    fn maps_reasoning_and_replays_signed_state_before_tools() {
        let mut config = PriestConfig::new("anthropic", "claude-test");
        config.reasoning = Some(ReasoningConfig {
            enabled: Some(true),
            effort: Some(ReasoningEffort::High),
            summary: Some(ReasoningSummaryMode::Auto),
        });
        let messages = vec![Message {
            role: "assistant".into(),
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                name: "lookup".into(),
                arguments: Map::new(),
            }]),
            reasoning: Some(ReasoningInfo {
                summary: Some("Checked.".into()),
                continuation: Some(vec![OpaqueReasoningState {
                    format: REASONING_FORMAT.into(),
                    value: json!({
                        "type":"thinking",
                        "thinking":"Checked.",
                        "signature":"opaque"
                    }),
                }]),
            }),
            ..Default::default()
        }];
        let (payload, _) =
            build_payload(&messages, &config, &OutputSpec::default(), None, false);

        assert_eq!(payload["thinking"], json!({"type":"adaptive","display":"summarized"}));
        assert_eq!(payload["output_config"], json!({"effort":"high"}));
        assert_eq!(payload["messages"][0]["content"][0]["type"], "thinking");
        assert_eq!(payload["messages"][0]["content"][1]["type"], "tool_use");

        let blocks: Vec<AnthropicContent> = serde_json::from_value(json!([
            {"type":"thinking","thinking":"Checked.","signature":"opaque"},
            {"type":"tool_use","id":"call_1","name":"lookup","input":{}}
        ]))
        .unwrap();
        let reasoning = parse_reasoning(&blocks, true).unwrap();
        assert_eq!(reasoning.summary.as_deref(), Some("Checked."));
        assert_eq!(
            reasoning.continuation.unwrap()[0].value,
            json!({"type":"thinking","thinking":"Checked.","signature":"opaque"})
        );
    }
}
