use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

use super::adapter::{AdapterCallOptions, AdapterResult, ProviderAdapter};
use crate::schema::tools::ToolCall;
use crate::context_builder::Message;
use crate::errors::PriestError;
use crate::schema::config::PriestConfig;
use crate::schema::request::OutputSpec;
use crate::schema::reasoning::ReasoningEffort;

pub struct OllamaProvider {
    base_url: String,
    client: Client,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: Client::new(),
        }
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new("http://localhost:11434")
    }
}

#[derive(Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
    prompt_eval_count: Option<u32>,
    eval_count: Option<u32>,
    done_reason: Option<String>,
}

#[derive(Deserialize)]
struct OllamaMessage {
    content: String,
    #[serde(default)]
    tool_calls: Option<Vec<Value>>,
}

fn map_done_reason(r: Option<&str>) -> Option<String> {
    Some(
        match r? {
            "stop" | "load" => "stop",
            "length" => "length",
            _ => "unknown",
        }
        .to_string(),
    )
}

fn build_wire_messages(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            if m.role == "tool" {
                // Ollama correlates tool results by tool_name, not call id.
                json!({"role": "tool", "content": m.content, "tool_name": m.name})
            } else if m.role == "assistant" && m.tool_calls.as_ref().is_some_and(|c| !c.is_empty()) {
                // Synthesized call ids are dropped on the wire.
                let calls: Vec<Value> = m
                    .tool_calls
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|c| json!({"function": {"name": c.name, "arguments": c.arguments}}))
                    .collect();
                json!({"role": "assistant", "content": m.content, "tool_calls": calls})
            } else {
                json!({"role": m.role, "content": m.content})
            }
        })
        .collect()
}

fn apply_tools(payload: &mut Value, options: Option<&AdapterCallOptions>) {
    // Ollama accepts OpenAI-shaped tools; it has no tool_choice parameter.
    let Some(options) = options.filter(|o| !o.tools.is_empty()) else { return };
    let tools: Vec<Value> = options
        .tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters.clone().unwrap_or_else(|| json!({})),
                }
            })
        })
        .collect();
    payload["tools"] = json!(tools);
}

/// Parse Ollama wire tool calls, synthesizing ids "call_N" in order.
fn parse_wire_tool_calls(raw: Option<&Vec<Value>>) -> Option<Vec<ToolCall>> {
    let raw = raw?;
    let mut calls = vec![];
    for item in raw {
        let function = item.get("function")?;
        let Some(name) = function.get("name").and_then(Value::as_str) else { continue };
        let arguments = match function.get("arguments") {
            Some(Value::Object(map)) => map.clone(),
            _ => serde_json::Map::new(),
        };
        calls.push(ToolCall {
            id: format!("call_{}", calls.len()),
            name: name.to_string(),
            arguments,
        });
    }
    if calls.is_empty() { None } else { Some(calls) }
}

fn apply_reasoning(payload: &mut Value, config: &PriestConfig) -> Result<(), PriestError> {
    let Some(reasoning) = &config.reasoning else {
        return Ok(());
    };
    if reasoning.enabled == Some(false) || reasoning.effort == Some(ReasoningEffort::None) {
        payload["think"] = json!(false);
        return Ok(());
    }
    if matches!(
        reasoning.effort,
        Some(ReasoningEffort::Minimal | ReasoningEffort::XHigh)
    ) {
        let effort = reasoning.effort.unwrap().as_str();
        return Err(PriestError::RequestInvalid {
            message: format!("Ollama does not define the reasoning effort '{effort}'"),
        });
    }
    if let Some(effort) = reasoning.effort {
        payload["think"] = json!(effort.as_str());
    } else if reasoning.enabled == Some(true) {
        payload["think"] = json!(true);
    }
    Ok(())
}

fn build_payload(
    messages: &[Message],
    config: &PriestConfig,
    output_spec: &OutputSpec,
    options: Option<&AdapterCallOptions>,
    stream: bool,
) -> Result<Value, PriestError> {
    let msgs = build_wire_messages(messages);
    let mut payload = json!({ "model": config.model, "messages": msgs, "stream": stream });
    apply_tools(&mut payload, options);
    if let Some(max_tokens) = config.max_output_tokens {
        payload["options"] = json!({ "num_predict": max_tokens });
    }
    apply_reasoning(&mut payload, config)?;
    if let Some(ref schema) = output_spec.json_schema {
        payload["format"] = schema.clone();
    } else if output_spec.provider_format.as_deref() == Some("json") {
        payload["format"] = json!("json");
    }
    for (k, v) in &config.provider_options {
        payload[k] = v.clone();
    }
    Ok(payload)
}

fn provider_error(config: &PriestConfig, msg: impl Into<String>) -> PriestError {
    PriestError::ProviderError {
        provider: config.provider.clone(),
        message: msg.into(),
    }
}

#[async_trait]
impl ProviderAdapter for OllamaProvider {
    async fn complete(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<AdapterResult, PriestError> {
        let url = format!("{}/api/chat", self.base_url);
        let payload = build_payload(messages, config, output_spec, options, false)?;
        let timeout = Duration::from_secs_f64(config.timeout_seconds);

        let resp = self
            .client
            .post(&url)
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

        let data: OllamaResponse = resp
            .json()
            .await
            .map_err(|e| provider_error(config, e.to_string()))?;
        let tool_calls = parse_wire_tool_calls(data.message.tool_calls.as_ref());
        Ok(AdapterResult {
            text: data.message.content,
            finish_reason: if tool_calls.is_some() {
                Some("tool_calls".to_string())
            } else {
                map_done_reason(data.done_reason.as_deref())
            },
            input_tokens: data.prompt_eval_count,
            output_tokens: data.eval_count,
            cached_input_tokens: None,
            reasoning_tokens: None,
            tool_calls,
            reasoning: None,
        })
    }

    async fn stream(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        let url = format!("{}/api/chat", self.base_url);
        let payload = build_payload(messages, config, output_spec, options, true)?;
        let timeout = Duration::from_secs_f64(config.timeout_seconds);
        let provider = config.provider.clone();

        let resp = self
            .client
            .post(&url)
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

        let byte_stream = resp.bytes_stream();
        let provider2 = provider.clone();

        let stream = byte_stream.filter_map(move |chunk: Result<Bytes, reqwest::Error>| {
            let _prov = provider2.clone();
            async move {
                let bytes = chunk.ok()?;
                let line = std::str::from_utf8(&bytes).ok()?.trim().to_string();
                if line.is_empty() {
                    return None;
                }
                let data: Value = serde_json::from_str(&line).ok()?;
                let done = data["done"].as_bool().unwrap_or(false);
                if done {
                    return None;
                }
                let content = data["message"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                if content.is_empty() {
                    None
                } else {
                    Some(Ok(content))
                }
            }
        });

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::reasoning::{ReasoningConfig, ReasoningEffort};

    #[test]
    fn neutral_reasoning_maps_and_provider_options_override() {
        let mut config = PriestConfig::new("ollama", "qwen");
        config.reasoning = Some(ReasoningConfig {
            enabled: None,
            effort: Some(ReasoningEffort::High),
            summary: None,
        });
        config.provider_options.insert("think".into(), json!(false));
        let payload = build_payload(
            &[Message::user("Hi")],
            &config,
            &OutputSpec::default(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(payload["think"], false);
    }

    #[test]
    fn rejects_undefined_effort() {
        let mut config = PriestConfig::new("ollama", "qwen");
        config.reasoning = Some(ReasoningConfig {
            enabled: None,
            effort: Some(ReasoningEffort::XHigh),
            summary: None,
        });
        let error = build_payload(
            &[Message::user("Hi")],
            &config,
            &OutputSpec::default(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), "REQUEST_INVALID");
    }
}
