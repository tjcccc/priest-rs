use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

use crate::context_builder::Message;
use crate::errors::PriestError;
use crate::schema::config::PriestConfig;
use crate::schema::request::OutputSpec;
use super::adapter::{AdapterResult, ProviderAdapter};

pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    client: Client,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self { base_url: "https://api.anthropic.com".into(), api_key: api_key.into(), client: Client::new() }
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self { base_url: base_url.into(), api_key: api_key.into(), client: Client::new() }
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
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}

fn map_stop(r: Option<&str>) -> Option<String> {
    Some(match r? {
        "end_turn" | "stop_sequence" => "stop",
        "max_tokens"                 => "length",
        _                            => "unknown",
    }.to_string())
}

fn build_payload(messages: &[Message], config: &PriestConfig, output_spec: &OutputSpec, stream: bool) -> (Value, Option<String>) {
    let mut system_parts: Vec<String> = messages.iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.clone())
        .collect();
    if let Some(ref schema) = output_spec.json_schema {
        let schema_str = serde_json::to_string_pretty(schema).unwrap_or_default();
        system_parts.push(format!(
            "Respond with a valid JSON object that conforms to the following JSON Schema:\n\n<schema>\n{schema_str}\n</schema>\n\nReturn only the JSON object — no explanation, no markdown fences."
        ));
    }
    let turns: Vec<Value> = messages.iter()
        .filter(|m| m.role != "system")
        .map(|m| json!({"role": m.role, "content": m.content}))
        .collect();
    let system_str: Option<String> = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };
    let max_tokens = config.max_output_tokens.unwrap_or(8096);
    let mut payload = json!({ "model": config.model, "messages": turns, "max_tokens": max_tokens });
    if stream { payload["stream"] = json!(true); }
    if let Some(ref sys) = system_str { payload["system"] = json!(sys); }
    for (k, v) in &config.provider_options { payload[k] = v.clone(); }
    (payload, system_str)
}

fn provider_error(config: &PriestConfig, msg: impl Into<String>) -> PriestError {
    PriestError::ProviderError { provider: config.provider.clone(), message: msg.into() }
}

#[async_trait]
impl ProviderAdapter for AnthropicProvider {
    async fn complete(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
    ) -> Result<AdapterResult, PriestError> {
        let url = format!("{}/v1/messages", self.base_url);
        let (payload, _) = build_payload(messages, config, output_spec, false);
        let timeout = Duration::from_secs_f64(config.timeout_seconds);

        let resp = self.client
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
                    PriestError::ProviderTimeout { provider: config.provider.clone(), timeout: config.timeout_seconds }
                } else {
                    provider_error(config, e.to_string())
                }
            })?;

        if resp.status() == 429 {
            return Err(PriestError::ProviderRateLimited { provider: config.provider.clone(), retry_after: None });
        }
        if !resp.status().is_success() {
            return Err(provider_error(config, format!("HTTP {}", resp.status())));
        }

        let data: AnthropicResponse = resp.json().await.map_err(|e| provider_error(config, e.to_string()))?;
        let text = data.content.into_iter()
            .find(|c| c.kind == "text")
            .and_then(|c| c.text)
            .unwrap_or_default();

        Ok(AdapterResult {
            text,
            finish_reason: map_stop(data.stop_reason.as_deref()),
            input_tokens: data.usage.as_ref().and_then(|u| u.input_tokens),
            output_tokens: data.usage.as_ref().and_then(|u| u.output_tokens),
        })
    }

    async fn stream(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        let url = format!("{}/v1/messages", self.base_url);
        let (payload, _) = build_payload(messages, config, output_spec, true);
        let timeout = Duration::from_secs_f64(config.timeout_seconds);
        let provider = config.provider.clone();

        let resp = self.client
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
                    PriestError::ProviderTimeout { provider: provider.clone(), timeout: config.timeout_seconds }
                } else {
                    PriestError::ProviderError { provider: provider.clone(), message: e.to_string() }
                }
            })?;

        if !resp.status().is_success() {
            return Err(PriestError::ProviderError { provider, message: format!("HTTP {}", resp.status()) });
        }

        let lines = resp.bytes_stream();
        let provider2 = provider.clone();

        let stream = lines.flat_map(move |chunk: Result<Bytes, reqwest::Error>| {
            let prov = provider2.clone();
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => return futures::stream::iter(vec![Err(
                    PriestError::ProviderError { provider: prov, message: e.to_string() }
                )]),
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
