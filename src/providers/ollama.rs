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

pub struct OllamaProvider {
    base_url: String,
    client: Client,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self { base_url: base_url.into(), client: Client::new() }
    }
}

impl Default for OllamaProvider {
    fn default() -> Self { Self::new("http://localhost:11434") }
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
}

fn map_done_reason(r: Option<&str>) -> Option<String> {
    Some(match r? {
        "stop" | "load" => "stop",
        "length"        => "length",
        _               => "unknown",
    }.to_string())
}

fn build_payload(messages: &[Message], config: &PriestConfig, output_spec: &OutputSpec, stream: bool) -> Value {
    let msgs: Vec<Value> = messages.iter().map(|m| json!({"role": m.role, "content": m.content})).collect();
    let mut payload = json!({ "model": config.model, "messages": msgs, "stream": stream });
    if let Some(max_tokens) = config.max_output_tokens {
        payload["options"] = json!({ "num_predict": max_tokens });
    }
    if let Some(ref schema) = output_spec.json_schema {
        payload["format"] = schema.clone();
    } else if output_spec.provider_format.as_deref() == Some("json") {
        payload["format"] = json!("json");
    }
    for (k, v) in &config.provider_options {
        payload[k] = v.clone();
    }
    payload
}

fn provider_error(config: &PriestConfig, msg: impl Into<String>) -> PriestError {
    PriestError::ProviderError { provider: config.provider.clone(), message: msg.into() }
}

#[async_trait]
impl ProviderAdapter for OllamaProvider {
    async fn complete(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
    ) -> Result<AdapterResult, PriestError> {
        let url = format!("{}/api/chat", self.base_url);
        let payload = build_payload(messages, config, output_spec, false);
        let timeout = Duration::from_secs_f64(config.timeout_seconds);

        let resp = self.client
            .post(&url)
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

        let data: OllamaResponse = resp.json().await.map_err(|e| provider_error(config, e.to_string()))?;
        Ok(AdapterResult {
            text: data.message.content,
            finish_reason: map_done_reason(data.done_reason.as_deref()),
            input_tokens: data.prompt_eval_count,
            output_tokens: data.eval_count,
        })
    }

    async fn stream(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        let url = format!("{}/api/chat", self.base_url);
        let payload = build_payload(messages, config, output_spec, true);
        let timeout = Duration::from_secs_f64(config.timeout_seconds);
        let provider = config.provider.clone();

        let resp = self.client
            .post(&url)
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

        let byte_stream = resp.bytes_stream();
        let provider2 = provider.clone();

        let stream = byte_stream.filter_map(move |chunk: Result<Bytes, reqwest::Error>| {
            let _prov = provider2.clone();
            async move {
                let bytes = chunk.ok()?;
                let line = std::str::from_utf8(&bytes).ok()?.trim().to_string();
                if line.is_empty() { return None; }
                let data: Value = serde_json::from_str(&line).ok()?;
                let done = data["done"].as_bool().unwrap_or(false);
                if done { return None; }
                let content = data["message"]["content"].as_str().unwrap_or("").to_string();
                if content.is_empty() { None } else { Some(Ok(content)) }
            }
        });

        Ok(Box::pin(stream))
    }
}
