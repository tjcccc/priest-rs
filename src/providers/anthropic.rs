use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

use super::adapter::{AdapterCallOptions, AdapterResult, ProviderAdapter};
use crate::schema::tools::{ToolCall, ToolChoice};
use crate::context_builder::Message;
use crate::errors::PriestError;
use crate::schema::config::PriestConfig;
use crate::schema::request::OutputSpec;

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

#[derive(Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
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
    apply_tools(&mut payload, options);
    if stream {
        payload["stream"] = json!(true);
    }
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
        let text = data
            .content
            .into_iter()
            .find(|c| c.kind == "text")
            .and_then(|c| c.text)
            .unwrap_or_default();

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
            tool_calls,
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
